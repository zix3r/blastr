//! Planning the cases of a check: which definition gets which edit, in which
//! order, and how a sample is taken.
//!
//! The order is fixed by the project, not by chance: the symbols of the graph
//! in the order the extraction found them (file by file, item by item), and the
//! edit kinds in their declared order. Two runs of the same project therefore
//! plan the same cases with the same ids, which is what makes a miss
//! reproducible from the report alone.

use std::collections::BTreeMap;
use std::sync::Arc;

use blastr_model::{RelPath, RevisionGraph, Symbol};

use crate::Result;
use crate::edits::{self, Edit, EditKind, Source};

/// One planned case: one definition, one edit, and the file text that results.
#[derive(Debug, Clone)]
pub struct PlannedCase {
    /// Identifier, unique within the project and check.
    pub id: String,
    /// The edit kind.
    pub kind: EditKind,
    /// The symbol the edit applies to, as the graph names it.
    pub symbol: String,
    /// The file the edit is in.
    pub file: RelPath,
    /// First byte of the definition on the base side.
    pub span_start: u32,
    /// The edit itself.
    pub edit: Edit,
    /// The whole file after the edit.
    pub text: String,
}

/// Plans the compiler cases of `graph`.
///
/// Symbols whose source cannot be found — items a macro produced have the span
/// of the macro call, and a file that does not parse cannot be edited — are
/// skipped, and the count of skipped edits is returned with the cases.
pub fn compiler_cases(
    graph: &RevisionGraph,
    project_dir: &std::path::Path,
    kinds: &[EditKind],
) -> Result<(Vec<PlannedCase>, u32)> {
    let mut cases = Vec::new();
    let mut skipped = 0_u32;
    let mut sources: BTreeMap<RelPath, Option<Arc<Source>>> = BTreeMap::new();
    let mut number = 0_usize;

    for symbol in &graph.symbols {
        let source = sources
            .entry(symbol.location.file.clone())
            .or_insert_with(|| load(&symbol.location.file, project_dir));
        let Some(source) = source.clone() else {
            skipped = skipped.saturating_add(kinds.len() as u32);
            continue;
        };
        for kind in kinds {
            let Some(edit) = source.edit(*kind, symbol) else {
                continue;
            };
            let Ok(text) = edits::apply_checked(&edit, source.text()) else {
                skipped = skipped.saturating_add(1);
                continue;
            };
            number += 1;
            cases.push(PlannedCase {
                id: format!("{number:03}-{}-{}", kind.as_str(), slug(&symbol.path.path)),
                kind: *kind,
                symbol: symbol.path.path.clone(),
                file: edit.file.clone(),
                span_start: symbol.location.span.start,
                edit,
                text,
            });
        }
    }
    Ok((cases, skipped))
}

/// One mutation of one function body, for the test check.
#[derive(Debug, Clone)]
pub struct PlannedMutant {
    /// Identifier, unique within the project.
    pub id: String,
    /// The mutation kind.
    pub kind: crate::mutate::MutationKind,
    /// The symbol the mutation is in.
    pub symbol: String,
    /// The file the mutation is in.
    pub file: RelPath,
    /// The edit itself.
    pub edit: MutantEdit,
    /// The whole file after the mutation.
    pub text: String,
}

/// The edit of a mutant, without the item snippets a compiler case records.
#[derive(Debug, Clone)]
pub struct MutantEdit {
    /// The byte range replaced.
    pub range: std::ops::Range<usize>,
    /// The text before the edit, clipped.
    pub before: String,
    /// The text after the edit, clipped.
    pub after: String,
}

/// Plans the test-check mutants of `graph`.
pub fn mutant_cases(
    graph: &RevisionGraph,
    project_dir: &std::path::Path,
) -> Result<(Vec<PlannedMutant>, u32)> {
    let mut cases = Vec::new();
    let mut skipped = 0_u32;
    let mut sources: BTreeMap<RelPath, Option<Arc<Source>>> = BTreeMap::new();
    let mut number = 0_usize;

    for symbol in &graph.symbols {
        if !mutatable(symbol) {
            continue;
        }
        let source = sources
            .entry(symbol.location.file.clone())
            .or_insert_with(|| load(&symbol.location.file, project_dir));
        let Some(source) = source.clone() else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        let Some(mutation) = crate::mutate::mutate(source.text(), symbol) else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        let Ok(text) = mutation.apply(source.text()) else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        number += 1;
        cases.push(PlannedMutant {
            id: format!(
                "{number:03}-{}-{}",
                mutation.kind.as_str(),
                slug(&symbol.path.path)
            ),
            kind: mutation.kind,
            symbol: symbol.path.path.clone(),
            file: symbol.location.file.clone(),
            edit: MutantEdit {
                range: mutation.range.clone(),
                before: mutation.before.clone(),
                after: mutation.after.clone(),
            },
            text,
        });
    }
    Ok((cases, skipped))
}

/// Functions and methods whose body can be mutated: what a test check is about.
fn mutatable(symbol: &Symbol) -> bool {
    use blastr_model::SymbolKind;
    matches!(symbol.path.kind, SymbolKind::Function | SymbolKind::Method)
        && symbol.flags.has_body
        // A test function's own failure proves nothing about selection: Blastr
        // selects every test it changes by definition.
        && !symbol.flags.is_test
}

/// Reads and parses one file of the project.
fn load(file: &RelPath, project_dir: &std::path::Path) -> Option<Arc<Source>> {
    let path = file.to_path(project_dir);
    let text = std::fs::read_to_string(path).ok()?;
    Source::new(file.clone(), text).map(Arc::new)
}

/// A file name for the case identifier that keeps the item recognisable.
fn slug(path: &str) -> String {
    let mut slug = String::with_capacity(path.len());
    for character in path.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_owned()
}

/// A deterministic pseudo-random order, for `--seed`.
fn shuffle(cases: &mut [PlannedCase], seed: u64) {
    let mut state = seed | 1;
    // xorshift64*: small, deterministic and good enough for sampling.
    for index in (1..cases.len()).rev() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let pick = usize::try_from(state.wrapping_mul(0x2545_F491_4F6C_DD1D) % (index as u64 + 1))
            .unwrap_or_default();
        cases.swap(index, pick);
    }
}

/// A planned case of either check: it has an identifier that the command line
/// can name.
pub trait Identified {
    /// The identifier of the case.
    fn identifier(&self) -> &str;
}

impl Identified for PlannedCase {
    fn identifier(&self) -> &str {
        &self.id
    }
}

impl Identified for PlannedMutant {
    fn identifier(&self) -> &str {
        &self.id
    }
}

/// Keeps only the one case `wanted` names, or all of them when nothing was
/// asked for. Rerunning a single case is how a miss is reproduced.
pub fn only<T: Identified>(cases: Vec<T>, wanted: Option<&str>) -> Result<Vec<T>> {
    let Some(wanted) = wanted else {
        return Ok(cases);
    };
    let known: Vec<String> = cases
        .iter()
        .map(|case| case.identifier().to_owned())
        .collect();
    let kept: Vec<T> = cases
        .into_iter()
        .filter(|case| case.identifier() == wanted)
        .collect();
    if kept.is_empty() {
        return Err(format!(
            "no case with the id `{wanted}` was planned; known ids: {}",
            known.join(", ")
        )
        .into());
    }
    Ok(kept)
}

/// Takes a sample of `limit` cases: the first ones in plan order when no seed
/// is given, a seeded sample otherwise. The sample is put back into plan order,
/// so a report never depends on the sampling.
pub fn sample(
    cases: Vec<PlannedCase>,
    limit: Option<usize>,
    seed: Option<u64>,
) -> Vec<PlannedCase> {
    let Some(limit) = limit else {
        return cases;
    };
    let Some(seed) = seed else {
        return cases.into_iter().take(limit).collect();
    };
    let mut cases = cases;
    shuffle(&mut cases, seed);
    let chosen: std::collections::HashSet<String> = cases
        .iter()
        .take(limit)
        .map(|case| case.id.clone())
        .collect();
    cases.retain(|case| chosen.contains(&case.id));
    // A report never depends on the sampling: the sample is put back into plan
    // order, which the identifiers carry.
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    cases
}

/// The same for mutants.
pub fn sample_mutants(
    cases: Vec<PlannedMutant>,
    limit: Option<usize>,
    seed: Option<u64>,
) -> Vec<PlannedMutant> {
    let Some(limit) = limit else {
        return cases;
    };
    let Some(seed) = seed else {
        return cases.into_iter().take(limit).collect();
    };
    let mut cases = cases;
    let mut order: Vec<(String, u64)> = cases
        .iter()
        .map(|case| (case.id.clone(), mix(seed, &case.id)))
        .collect();
    order.sort();
    let chosen: std::collections::HashSet<String> =
        order.iter().take(limit).map(|(id, _)| id.clone()).collect();
    cases.retain(|case| chosen.contains(&case.id));
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    cases
}

/// A hash of `seed` and `text`, so that sampling mutants is deterministic
/// without depending on the order they were planned in.
fn mix(seed: u64, text: &str) -> u64 {
    let mut hash = seed ^ 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A case with just an id.
    fn case(id: &str) -> PlannedCase {
        PlannedCase {
            id: id.to_owned(),
            kind: EditKind::AddParameter,
            symbol: String::new(),
            file: RelPath::new("src/lib.rs"),
            span_start: 0,
            edit: Edit {
                file: RelPath::new("src/lib.rs"),
                range: 0..0,
                replacement: String::new(),
                before: String::new(),
                after: String::new(),
            },
            text: String::new(),
        }
    }

    #[test]
    fn identifiers_keep_the_item_recognisable() {
        assert_eq!(slug("krate::Outer::inner"), "krate-Outer-inner");
        assert_eq!(slug("krate::a_b"), "krate-a_b");
        assert_eq!(slug("::x::"), "x");
    }

    #[test]
    fn a_limit_takes_the_first_cases_in_plan_order() {
        let cases = vec![case("a"), case("b"), case("c")];
        let sampled = sample(cases, Some(2), None);
        let ids: Vec<&str> = sampled.iter().map(|case| case.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn a_seeded_sample_is_deterministic_and_in_plan_order() {
        let cases = || {
            (0..20)
                .map(|i| case(&format!("c{i:02}")))
                .collect::<Vec<_>>()
        };
        let first = sample(cases(), Some(5), Some(7));
        let second = sample(cases(), Some(5), Some(7));
        let ids = |cases: &[PlannedCase]| {
            cases
                .iter()
                .map(|case| case.id.clone())
                .collect::<Vec<String>>()
        };
        assert_eq!(ids(&first), ids(&second));
        assert_eq!(first.len(), 5);
        let mut sorted = ids(&first);
        sorted.sort();
        assert_eq!(sorted, ids(&first), "the sample keeps plan order");
        let other = sample(cases(), Some(5), Some(8));
        assert_ne!(ids(&other), ids(&first));
    }

    #[test]
    fn sampling_without_a_limit_keeps_every_case() {
        let cases = vec![case("a"), case("b")];
        assert_eq!(sample(cases, None, Some(3)).len(), 2);
    }

    #[test]
    fn mutant_sampling_is_deterministic() {
        let mutants = || {
            (0..10)
                .map(|i| PlannedMutant {
                    id: format!("m{i:02}"),
                    kind: crate::mutate::MutationKind::SwapOperator,
                    symbol: String::new(),
                    file: RelPath::new("src/lib.rs"),
                    edit: MutantEdit {
                        range: 0..0,
                        before: String::new(),
                        after: String::new(),
                    },
                    text: String::new(),
                })
                .collect::<Vec<_>>()
        };
        let picked = sample_mutants(mutants(), Some(3), Some(11));
        assert_eq!(picked.len(), 3);
        assert_eq!(
            picked[0].id,
            sample_mutants(mutants(), Some(3), Some(11))[0].id
        );
    }

    #[test]
    fn every_planned_edit_parses() {
        assert!(crate::edits::parses("fn f() {}"));
        assert!(!crate::edits::parses("fn f( {"));
    }
}
