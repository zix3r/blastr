//! Pairing the symbols of two revision graphs.
//!
//! Only symbols located in a changed file take part. Identity is the canonical
//! [`SymbolPath`](blastr_model::SymbolPath); symbols left over are paired as
//! renames or moves when a body hash, a signature hash plus a body sketch, or
//! the shape of a container's members says they are the same item.

use std::collections::{BTreeSet, HashMap, HashSet};

use blastr_model::{
    ChangedFile, GraphIndex, RelPath, RevisionGraph, Symbol, SymbolId, SymbolKind, SymbolPath,
    sketch_similarity,
};

use super::MatchParams;

/// Symbol kinds a rename or a move may pair up. Fields, variants, impl blocks
/// and trait items follow their parent instead.
const RENAMEABLE: [SymbolKind; 10] = [
    SymbolKind::Function,
    SymbolKind::Method,
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Union,
    SymbolKind::Trait,
    SymbolKind::Const,
    SymbolKind::Static,
    SymbolKind::TypeAlias,
    SymbolKind::Macro,
];

/// Kinds that own members: when such a symbol was added or removed, its
/// members are covered by that one change.
const COVERS_MEMBERS: [SymbolKind; 6] = [
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Union,
    SymbolKind::Trait,
    SymbolKind::Impl,
    SymbolKind::Variant,
];

/// The symbols of both graphs that lie in a changed file, in ascending id order.
pub(crate) struct Scope {
    /// Symbols of the base graph to compare in detail.
    pub(crate) base: Vec<SymbolId>,
    /// Symbols of the head graph to compare in detail.
    pub(crate) head: Vec<SymbolId>,
}

/// Every pair of symbols the two graphs share.
#[derive(Debug, Default)]
pub(crate) struct Matches {
    /// Matched pairs, sorted by base id.
    pub(crate) pairs: Vec<(SymbolId, SymbolId)>,
    /// Base id to head id.
    pub(crate) base_to_head: HashMap<SymbolId, SymbolId>,
    /// Head id to base id.
    pub(crate) head_to_base: HashMap<SymbolId, SymbolId>,
    /// Head ids that were paired as a rename or a move rather than matched
    /// by identity or by their parent's members.
    pub(crate) renamed: BTreeSet<SymbolId>,
}

impl Matches {
    /// Records one pair.
    fn pair(&mut self, base: SymbolId, head: SymbolId) {
        self.base_to_head.insert(base, head);
        self.head_to_base.insert(head, base);
        self.pairs.push((base, head));
    }

    /// True when both sides of the pair are already taken.
    fn taken(&self, base: SymbolId, head: SymbolId) -> bool {
        self.base_to_head.contains_key(&base) || self.head_to_base.contains_key(&head)
    }
}

/// Collects the symbols of both graphs that lie in a changed file.
pub(crate) fn scope(base: &RevisionGraph, head: &RevisionGraph, changed: &[ChangedFile]) -> Scope {
    let base_files: HashSet<&RelPath> = changed
        .iter()
        .map(|file| file.old_path.as_ref().unwrap_or(&file.path))
        .collect();
    let head_files: HashSet<&RelPath> = changed.iter().map(|file| &file.path).collect();
    Scope {
        base: in_files(base, &base_files),
        head: in_files(head, &head_files),
    }
}

/// The ids of the non-module symbols located in one of `files`.
fn in_files(graph: &RevisionGraph, files: &HashSet<&RelPath>) -> Vec<SymbolId> {
    graph
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, symbol)| {
            symbol.path.kind != SymbolKind::Module && files.contains(&symbol.location.file)
        })
        .map(|(index, _)| SymbolId(index as u32))
        .collect()
}

/// True when one of the symbol's ancestors was itself added or removed, so
/// that the ancestor's change covers this symbol.
pub(crate) fn covered_by_ancestor(
    graph: &RevisionGraph,
    id: SymbolId,
    matched: &HashMap<SymbolId, SymbolId>,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut current = graph.symbols[id.0 as usize].parent;
    while let Some(parent) = current {
        if !seen.insert(parent) {
            return false;
        }
        let symbol = &graph.symbols[parent.0 as usize];
        if COVERS_MEMBERS.contains(&symbol.path.kind) && !matched.contains_key(&parent) {
            return true;
        }
        current = symbol.parent;
    }
    false
}

/// Pairs the symbols of both graphs: first by identity, then by rename.
pub(crate) fn match_symbols(
    base: &RevisionGraph,
    head: &RevisionGraph,
    base_index: &GraphIndex,
    head_index: &GraphIndex,
    scope: &Scope,
    params: &MatchParams,
) -> Matches {
    let context = Context {
        base,
        head,
        base_index,
        head_index,
        params,
    };
    let mut matches = Matches::default();
    context.exact(scope, &mut matches);
    context.renames(scope, &mut matches);
    matches.pairs.sort_unstable();
    matches
}

/// One rename candidate pair with the evidence that supports it.
struct Candidate {
    /// Smaller is better: an equal body hash, then a similar body, then an
    /// equal signature alone (type aliases and containers).
    tier: u8,
    /// Body similarity, from 0.0 to 1.0.
    similarity: f32,
    base: SymbolId,
    head: SymbolId,
}

/// The two graphs under comparison, with the tables and parameters they need.
struct Context<'a> {
    base: &'a RevisionGraph,
    head: &'a RevisionGraph,
    base_index: &'a GraphIndex,
    head_index: &'a GraphIndex,
    params: &'a MatchParams,
}

impl<'a> Context<'a> {
    fn base_symbol(&self, id: SymbolId) -> &'a Symbol {
        &self.base.symbols[id.0 as usize]
    }

    fn head_symbol(&self, id: SymbolId) -> &'a Symbol {
        &self.head.symbols[id.0 as usize]
    }

    /// Step 1: pairs with equal identity, which also covers a file that moved
    /// without its items changing.
    fn exact(&self, scope: &Scope, matches: &mut Matches) {
        let mut by_path: HashMap<&SymbolPath, Vec<SymbolId>> = HashMap::new();
        for &id in &scope.head {
            by_path
                .entry(&self.head_symbol(id).path)
                .or_default()
                .push(id);
        }
        for &id in &scope.base {
            let Some(candidates) = by_path.get(&self.base_symbol(id).path) else {
                continue;
            };
            let found = candidates
                .iter()
                .copied()
                .find(|head| !matches.head_to_base.contains_key(head));
            if let Some(head) = found {
                matches.pair(id, head);
            }
        }
    }

    /// Step 2: pairs the leftovers as renames and moves, best first.
    fn renames(&self, scope: &Scope, matches: &mut Matches) {
        let base_candidates = self.candidates(self.base, &scope.base, &matches.base_to_head);
        let head_candidates = self.candidates(self.head, &scope.head, &matches.head_to_base);
        let aliases = Aliases {
            base: alias_counts(self.base, &scope.base, &matches.base_to_head),
            head: alias_counts(self.head, &scope.head, &matches.head_to_base),
        };

        let mut candidates = Vec::new();
        for &base in &base_candidates {
            for &head in &head_candidates {
                if self.base_symbol(base).path.kind != self.head_symbol(head).path.kind {
                    continue;
                }
                if let Some((tier, similarity)) = self.qualify(base, head, &aliases) {
                    candidates.push(Candidate {
                        tier,
                        similarity,
                        base,
                        head,
                    });
                }
            }
        }
        candidates.sort_by(|a, b| {
            a.tier
                .cmp(&b.tier)
                .then_with(|| b.similarity.total_cmp(&a.similarity))
                .then_with(|| {
                    self.distance(a.base, a.head)
                        .cmp(&self.distance(b.base, b.head))
                })
                .then_with(|| {
                    self.base_symbol(a.base)
                        .path
                        .path
                        .cmp(&self.base_symbol(b.base).path.path)
                })
                .then_with(|| {
                    self.head_symbol(a.head)
                        .path
                        .path
                        .cmp(&self.head_symbol(b.head).path.path)
                })
                .then_with(|| a.base.cmp(&b.base))
                .then_with(|| a.head.cmp(&b.head))
        });

        for candidate in &candidates {
            if matches.taken(candidate.base, candidate.head) {
                continue;
            }
            matches.renamed.insert(candidate.head);
            matches.pair(candidate.base, candidate.head);
            if is_container(self.base_symbol(candidate.base).path.kind) {
                self.members(candidate.base, candidate.head, matches);
            }
        }
    }

    /// How far apart two symbols are declared: 0 in the same container, 1 in
    /// the same crate, 2 in different crates. Equally good rename candidates
    /// are paired nearest first.
    fn distance(&self, base: SymbolId, head: SymbolId) -> u8 {
        let old = &self.base_symbol(base).path;
        let new = &self.head_symbol(head).path;
        if container_of(&old.path) == container_of(&new.path) {
            0
        } else if old.crate_name() == new.crate_name() {
            1
        } else {
            2
        }
    }

    /// The symbols of one side that may be paired as a rename.
    fn candidates(
        &self,
        graph: &RevisionGraph,
        side: &[SymbolId],
        matched: &HashMap<SymbolId, SymbolId>,
    ) -> Vec<SymbolId> {
        side.iter()
            .copied()
            .filter(|id| !matched.contains_key(id))
            .filter(|&id| {
                let symbol = &graph.symbols[id.0 as usize];
                !symbol.flags.in_trait
                    && RENAMEABLE.contains(&symbol.path.kind)
                    && !covered_by_ancestor(graph, id, matched)
            })
            .collect()
    }

    /// The evidence that `base` and `head` are one item, if there is any.
    ///
    /// A signature hash does not contain the item's own name, so a renamed
    /// item keeps it.
    fn qualify(&self, base: SymbolId, head: SymbolId, aliases: &Aliases) -> Option<(u8, f32)> {
        let old = self.base_symbol(base);
        let new = self.head_symbol(head);
        let same_signature = old.signature_hash == new.signature_hash;
        let kind = new.path.kind;
        if is_container(kind) {
            // No body: the header and the members identify the item.
            return (same_signature && self.members_agree(base, head)).then_some((2, 1.0));
        }
        if kind == SymbolKind::TypeAlias {
            // No body either: the target alone identifies the alias, which is
            // only safe when no other unmatched alias has the same target.
            let unique = old.signature_hash != 0
                && aliases.base.get(&old.signature_hash) == Some(&1)
                && aliases.head.get(&new.signature_hash) == Some(&1);
            return (same_signature && unique).then_some((2, 1.0));
        }
        if old.body_hash != 0 && old.body_hash == new.body_hash {
            return Some((0, 1.0));
        }
        if same_signature {
            let similarity = sketch_similarity(&old.body_sketch, &new.body_sketch);
            if similarity >= self.params.rename_similarity {
                return Some((1, similarity));
            }
        }
        None
    }

    /// True when every member of `base` has a member of `head` with the same
    /// last path segment, the same kind and an equal signature hash.
    fn members_agree(&self, base: SymbolId, head: SymbolId) -> bool {
        let old = self.base_index.children(base);
        let new = self.head_index.children(head);
        if old.len() != new.len() {
            return false;
        }
        let mut used = vec![false; new.len()];
        for &child in old {
            let symbol = self.base_symbol(child);
            let name = symbol.path.name();
            let found = new.iter().enumerate().find(|(index, candidate)| {
                if used[*index] {
                    return false;
                }
                let other = self.head_symbol(**candidate);
                other.path.name() == name
                    && other.path.kind == symbol.path.kind
                    && other.signature_hash == symbol.signature_hash
            });
            match found {
                Some((index, _)) => used[index] = true,
                None => return false,
            }
        }
        true
    }

    /// Pairs the members of two containers that were paired as a rename.
    fn members(&self, base: SymbolId, head: SymbolId, matches: &mut Matches) {
        for &child in self.base_index.children(base) {
            if matches.base_to_head.contains_key(&child) {
                continue;
            }
            let symbol = self.base_symbol(child);
            let name = symbol.path.name();
            let found = self
                .head_index
                .children(head)
                .iter()
                .copied()
                .find(|&candidate| {
                    if matches.head_to_base.contains_key(&candidate) {
                        return false;
                    }
                    let other = self.head_symbol(candidate);
                    other.path.name() == name && other.path.kind == symbol.path.kind
                });
            if let Some(candidate) = found {
                matches.pair(child, candidate);
            }
        }
    }
}

/// How many unmatched type aliases each side has per signature hash.
struct Aliases {
    base: HashMap<u64, usize>,
    head: HashMap<u64, usize>,
}

/// Counts the unmatched type aliases of one side by signature hash.
fn alias_counts(
    graph: &RevisionGraph,
    side: &[SymbolId],
    matched: &HashMap<SymbolId, SymbolId>,
) -> HashMap<u64, usize> {
    let mut counts = HashMap::new();
    for id in side.iter().filter(|id| !matched.contains_key(id)) {
        let symbol = &graph.symbols[id.0 as usize];
        if symbol.path.kind == SymbolKind::TypeAlias {
            *counts.entry(symbol.signature_hash).or_insert(0) += 1;
        }
    }
    counts
}

/// The path text of the item a path is declared in.
fn container_of(path: &str) -> &str {
    path.rsplit_once("::")
        .map_or("", |(container, _)| container)
}

/// True for kinds that own members and have no body of their own.
fn is_container(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Union | SymbolKind::Trait
    )
}
