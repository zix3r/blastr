//! Structural comparison of two revision graphs.
//!
//! [`classify`] pairs the symbols of the two graphs and reports what happened
//! to every symbol that lives in a changed file. [`manifest_changes`] reports
//! feature and dependency differences between two `Cargo.toml` texts. Both are
//! pure functions on the data of `blastr-model`.

mod manifest;
#[cfg(test)]
mod manifest_tests;
mod matching;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use blastr_model::{
    Change, ChangeDetail, ChangeKind, ChangedFile, Delta, FileStatus, GraphIndex, Location,
    RelPath, RevisionGraph, Symbol, SymbolId, SymbolKind,
};

pub use manifest::manifest_changes;

/// How close two symbols must be to count as the same one after a rename.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchParams {
    /// Minimum body similarity for a rename when signatures are identical. Default 0.7.
    pub rename_similarity: f32,
}

impl Default for MatchParams {
    fn default() -> Self {
        MatchParams {
            rename_similarity: 0.7,
        }
    }
}

/// Structural changes between two graphs.
///
/// `changed` limits the detailed comparison to symbols located in those files,
/// on either side. The result is sorted by (file of the symbol, start offset,
/// kind, path text) and never depends on hash map iteration order.
pub fn classify(
    base: &RevisionGraph,
    head: &RevisionGraph,
    changed: &[ChangedFile],
    params: &MatchParams,
) -> Vec<Change> {
    let base_index = GraphIndex::new(base);
    let head_index = GraphIndex::new(head);
    let scope = matching::scope(base, head, changed);
    let matches = matching::match_symbols(base, head, &base_index, &head_index, &scope, params);

    let mut changes = Vec::new();

    // A variant reported as reshaped covers its own fields, whether they are
    // matched, added or removed.
    let mut reshaped_base = HashSet::new();
    let mut reshaped_head = HashSet::new();
    for &(base_id, head_id) in &matches.pairs {
        let old = &base.symbols[base_id.0 as usize];
        let new = &head.symbols[head_id.0 as usize];
        if new.path.kind == SymbolKind::Variant && old.signature_hash != new.signature_hash {
            reshaped_base.insert(base_id);
            reshaped_head.insert(head_id);
        }
    }

    for &(base_id, head_id) in &matches.pairs {
        let old = &base.symbols[base_id.0 as usize];
        let new = &head.symbols[head_id.0 as usize];
        let renamed = matches.renamed.contains(&head_id);
        if renamed {
            changes.push(Change {
                kind: ChangeKind::Renamed,
                symbol: new.path.clone(),
                old: Some(old.location.clone()),
                new: Some(new.location.clone()),
                detail: ChangeDetail::Renamed {
                    from: old.path.clone(),
                },
            });
        }
        let inside_variant = new.path.kind == SymbolKind::Field
            && (old
                .parent
                .is_some_and(|parent| reshaped_base.contains(&parent))
                || new
                    .parent
                    .is_some_and(|parent| reshaped_head.contains(&parent)));
        if inside_variant {
            continue;
        }
        compare(old, new, &mut changes);
    }

    report_unmatched(
        base,
        &scope.base,
        &matches.base_to_head,
        &reshaped_base,
        Delta::Removed,
        &mut changes,
    );
    report_unmatched(
        head,
        &scope.head,
        &matches.head_to_base,
        &reshaped_head,
        Delta::Added,
        &mut changes,
    );

    let additional = formatting_only(changed, &changes, head, &head_index);
    changes.extend(additional);
    sort(&mut changes);
    changes
}

/// Reports what happened to a pair of symbols that are the same item.
///
/// No hash contains the item's own name, so a renamed pair is compared like
/// any other: a signature difference on it is a real one.
fn compare(old: &Symbol, new: &Symbol, out: &mut Vec<Change>) {
    let change = |kind: ChangeKind, detail: ChangeDetail| Change {
        kind,
        symbol: new.path.clone(),
        old: Some(old.location.clone()),
        new: Some(new.location.clone()),
        detail,
    };
    if old.visibility != new.visibility {
        out.push(change(
            ChangeKind::Visibility,
            ChangeDetail::Visibility {
                from: old.visibility,
                to: new.visibility,
            },
        ));
    }
    if old.attr_hash != new.attr_hash {
        out.push(change(ChangeKind::Attribute, ChangeDetail::None));
    }

    let signature = old.signature_hash != new.signature_hash;
    let body = old.body_hash != new.body_hash;
    match new.path.kind {
        SymbolKind::Function | SymbolKind::Method => {
            if new.flags.in_trait {
                if signature {
                    out.push(change(
                        ChangeKind::TraitItem,
                        ChangeDetail::Member(Delta::Modified),
                    ));
                } else if body {
                    out.push(change(ChangeKind::TraitItem, ChangeDetail::DefaultBody));
                }
            } else if signature {
                out.push(change(ChangeKind::Signature, ChangeDetail::None));
            } else if body {
                out.push(change(ChangeKind::Body, ChangeDetail::None));
            }
        }
        SymbolKind::Field => {
            if signature {
                out.push(change(
                    ChangeKind::Field,
                    ChangeDetail::Member(Delta::Modified),
                ));
            }
        }
        SymbolKind::Variant => {
            if signature {
                out.push(change(
                    ChangeKind::Variant,
                    ChangeDetail::Member(Delta::Modified),
                ));
            }
        }
        SymbolKind::Const | SymbolKind::Static => {
            if signature {
                out.push(change(ChangeKind::ConstValue, ChangeDetail::Retyped));
            } else if body {
                out.push(change(ChangeKind::ConstValue, ChangeDetail::None));
            }
        }
        SymbolKind::TypeAlias => {
            if signature {
                out.push(change(ChangeKind::TypeAlias, ChangeDetail::None));
            }
        }
        SymbolKind::Macro => {
            if body {
                out.push(change(ChangeKind::MacroDef, ChangeDetail::None));
            }
        }
        SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Union
        | SymbolKind::Trait
        | SymbolKind::Impl => {
            if signature {
                out.push(change(ChangeKind::Signature, ChangeDetail::None));
            }
        }
        SymbolKind::Module => {}
    }
}

/// Reports the symbols of one side that have no counterpart on the other.
///
/// `reshaped` holds the matched variants of this side that are reported as
/// modified; their fields are covered by that change.
fn report_unmatched(
    graph: &RevisionGraph,
    scope: &[SymbolId],
    matched: &HashMap<SymbolId, SymbolId>,
    reshaped: &HashSet<SymbolId>,
    delta: Delta,
    out: &mut Vec<Change>,
) {
    let fallback = match delta {
        Delta::Added => ChangeKind::Added,
        _ => ChangeKind::Removed,
    };
    for &id in scope {
        if matched.contains_key(&id) {
            continue;
        }
        let symbol = &graph.symbols[id.0 as usize];
        if matching::covered_by_ancestor(graph, id, matched) {
            continue;
        }
        if symbol
            .parent
            .is_some_and(|parent| reshaped.contains(&parent))
        {
            continue;
        }
        let (kind, detail) = member_change(symbol, delta, fallback);
        let (old, new) = match delta {
            Delta::Added => (None, Some(symbol.location.clone())),
            _ => (Some(symbol.location.clone()), None),
        };
        out.push(Change {
            kind,
            symbol: symbol.path.clone(),
            old,
            new,
            detail,
        });
    }
}

/// The kind and detail for a symbol that appeared or disappeared.
fn member_change(
    symbol: &Symbol,
    delta: Delta,
    fallback: ChangeKind,
) -> (ChangeKind, ChangeDetail) {
    match symbol.path.kind {
        SymbolKind::Field => (ChangeKind::Field, ChangeDetail::Member(delta)),
        SymbolKind::Variant => (ChangeKind::Variant, ChangeDetail::Member(delta)),
        SymbolKind::Impl => (ChangeKind::Impl, ChangeDetail::Member(delta)),
        _ if symbol.flags.in_trait => (ChangeKind::TraitItem, ChangeDetail::Member(delta)),
        _ => (fallback, ChangeDetail::None),
    }
}

/// One [`ChangeKind::NonSemantic`] change per formatting-only modified file.
fn formatting_only(
    changed: &[ChangedFile],
    changes: &[Change],
    head: &RevisionGraph,
    head_index: &GraphIndex,
) -> Vec<Change> {
    let mut group_of: HashMap<&RelPath, usize> = HashMap::new();
    for (index, file) in changed.iter().enumerate() {
        group_of.insert(&file.path, index);
        if let Some(old_path) = &file.old_path {
            group_of.insert(old_path, index);
        }
    }
    let mut touched = vec![false; changed.len()];
    for change in changes {
        for location in [change.old.as_ref(), change.new.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(&group) = group_of.get(&location.file) {
                touched[group] = true;
            }
        }
    }

    let mut out = Vec::new();
    for (index, file) in changed.iter().enumerate() {
        if file.status != FileStatus::Modified || touched[index] {
            continue;
        }
        let symbols = head_index.symbols_in_file(&file.path);
        let Some(symbol) = anchor(head, symbols) else {
            continue;
        };
        out.push(Change {
            kind: ChangeKind::NonSemantic,
            symbol: symbol.path.clone(),
            old: None,
            new: Some(Location {
                file: file.path.clone(),
                span: symbol.location.span,
            }),
            detail: ChangeDetail::None,
        });
    }
    out
}

/// The outermost module declared in a file, else its first symbol.
fn anchor<'a>(head: &'a RevisionGraph, symbols: &[SymbolId]) -> Option<&'a Symbol> {
    let all: Vec<&'a Symbol> = symbols
        .iter()
        .map(|&id| &head.symbols[id.0 as usize])
        .collect();
    let module = all
        .iter()
        .copied()
        .filter(|symbol| symbol.path.kind == SymbolKind::Module)
        .min_by(by_position);
    module.or_else(|| all.iter().copied().min_by(by_position))
}

/// Orders symbols by position: the earliest start, then the outermost one.
fn by_position(a: &&Symbol, b: &&Symbol) -> std::cmp::Ordering {
    a.location
        .span
        .start
        .cmp(&b.location.span.start)
        .then_with(|| b.location.span.end.cmp(&a.location.span.end))
        .then_with(|| a.path.path.cmp(&b.path.path))
        .then_with(|| a.path.disambiguator.cmp(&b.path.disambiguator))
}

/// The total order of the result: file, offset, kind, then path text.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    file: String,
    start: u32,
    kind: usize,
    path: String,
    disambiguator: u32,
    old_start: u32,
    new_start: u32,
    detail: String,
}

impl SortKey {
    /// The key of one change.
    fn of(change: &Change) -> SortKey {
        let anchor = change.new.as_ref().or(change.old.as_ref());
        SortKey {
            file: anchor
                .map(|location| location.file.as_str().to_owned())
                .unwrap_or_default(),
            start: anchor.map(|location| location.span.start).unwrap_or(0),
            kind: ChangeKind::ALL
                .iter()
                .position(|kind| *kind == change.kind)
                .unwrap_or(usize::MAX),
            path: change.symbol.path.clone(),
            disambiguator: change.symbol.disambiguator,
            old_start: change
                .old
                .as_ref()
                .map(|location| location.span.start)
                .unwrap_or(u32::MAX),
            new_start: change
                .new
                .as_ref()
                .map(|location| location.span.start)
                .unwrap_or(u32::MAX),
            detail: format!("{:?}", change.detail),
        }
    }
}

/// Puts the changes into their documented order.
fn sort(changes: &mut [Change]) {
    changes.sort_by_cached_key(SortKey::of);
}
