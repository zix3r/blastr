//! Turning the text a user typed into one symbol of a graph.
//!
//! A query names a symbol by its full path, or by any trailing run of
//! segments that starts at a `::` boundary. When nothing matches, the
//! closest names are offered as suggestions.

use blastr_model::SymbolPath;

/// How many suggestions a failed lookup offers.
const MAX_SUGGESTIONS: usize = 5;
/// How many matching paths an ambiguous lookup lists.
const MAX_AMBIGUOUS: usize = 20;
/// The largest edit distance that still counts as a near miss.
const MAX_SUGGESTION_DISTANCE: usize = 3;

/// Why a query does not name exactly one symbol.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    /// Nothing carries the query as a path or a path suffix.
    #[error("no symbol matches `{query}`")]
    NotFound {
        /// The query, trimmed of surrounding whitespace.
        query: String,
        /// Up to five nearby paths, closest first.
        suggestions: Vec<String>,
    },
    /// Several symbols carry the query as a path suffix.
    #[error("`{query}` matches more than one symbol")]
    Ambiguous {
        /// The query, trimmed of surrounding whitespace.
        query: String,
        /// The matching path texts, sorted, without duplicates.
        candidates: Vec<String>,
    },
}

/// Index of the candidate `query` names: an exact path text match first,
/// otherwise the only candidate whose path ends with `::<query>`.
pub fn resolve(candidates: &[&SymbolPath], query: &str) -> Result<usize, LookupError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(not_found(candidates, query));
    }
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate.path == query)
    {
        return Ok(index);
    }

    let matching: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| ends_with_segments(&candidate.path, query))
        .map(|(index, _)| index)
        .collect();
    match matching.as_slice() {
        [] => Err(not_found(candidates, query)),
        [index] => Ok(*index),
        _ => {
            let mut paths: Vec<String> = matching
                .iter()
                .filter_map(|index| candidates.get(*index))
                .map(|candidate| candidate.path.clone())
                .collect();
            paths.sort_unstable();
            paths.dedup();
            paths.truncate(MAX_AMBIGUOUS);
            Err(LookupError::Ambiguous {
                query: query.to_owned(),
                candidates: paths,
            })
        }
    }
}

/// The error for a query that matches nothing, with its suggestions.
fn not_found(candidates: &[&SymbolPath], query: &str) -> LookupError {
    LookupError::NotFound {
        query: query.to_owned(),
        suggestions: suggest(candidates, query),
    }
}

/// Whether `path` ends with `query` at a `::` boundary.
fn ends_with_segments(path: &str, query: &str) -> bool {
    path.strip_suffix(query)
        .is_some_and(|prefix| prefix.ends_with("::"))
}

/// The paths whose last segment is closest to the query's last segment,
/// closest first, then by path text, without duplicates.
fn suggest(candidates: &[&SymbolPath], query: &str) -> Vec<String> {
    let wanted = last_segment(query);
    // A short name has few letters to get wrong: allow one edit per three
    // characters, so that `thing` does not suggest `String`.
    let allowed = (wanted.chars().count() / 3).clamp(1, MAX_SUGGESTION_DISTANCE);
    let mut scored: Vec<(usize, &str)> = candidates
        .iter()
        .map(|candidate| {
            (
                edit_distance(wanted, candidate.name()),
                candidate.path.as_str(),
            )
        })
        .filter(|(distance, _)| *distance <= allowed)
        .collect();
    scored.sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));

    let mut suggestions: Vec<String> = Vec::new();
    for (_, path) in scored {
        if suggestions.len() == MAX_SUGGESTIONS {
            break;
        }
        if !suggestions.iter().any(|known| known.as_str() == path) {
            suggestions.push(path.to_owned());
        }
    }
    suggestions
}

/// The last `::`-separated segment of a path text.
fn last_segment(path: &str) -> &str {
    path.rfind("::").map_or(path, |index| &path[index + 2..])
}

/// The number of single-character insertions, deletions and substitutions
/// that turn `left` into `right`.
fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }

    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current: Vec<usize> = vec![0; right.len() + 1];
    for (row, source) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, target) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(source != target);
            let insertion = current[column] + 1;
            let deletion = previous[column + 1] + 1;
            current[column + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::SymbolKind;

    /// Resolves against paths built from `texts`, all functions.
    fn resolve_texts(texts: &[&str], query: &str) -> Result<usize, LookupError> {
        let symbols: Vec<SymbolPath> = texts
            .iter()
            .map(|text| SymbolPath::new(*text, SymbolKind::Function))
            .collect();
        let candidates: Vec<&SymbolPath> = symbols.iter().collect();
        resolve(&candidates, query)
    }

    #[test]
    fn an_exact_path_wins_over_a_suffix() {
        let texts = ["krate::discount", "krate::billing::discount"];
        assert_eq!(resolve_texts(&texts, " krate::discount "), Ok(0));
    }

    #[test]
    fn an_exact_path_text_picks_the_first_candidate() {
        let symbols = [
            SymbolPath::new("krate::Point", SymbolKind::Struct),
            SymbolPath::new("krate::Point", SymbolKind::Enum),
        ];
        let candidates: Vec<&SymbolPath> = symbols.iter().collect();
        assert_eq!(resolve(&candidates, "krate::Point"), Ok(0));
    }

    #[test]
    fn a_unique_suffix_resolves() {
        let texts = ["krate::billing::discount", "krate::tax::rounds"];
        assert_eq!(resolve_texts(&texts, "tax::rounds"), Ok(1));
        assert_eq!(resolve_texts(&texts, "discount"), Ok(0));
    }

    #[test]
    fn a_suffix_starts_at_a_segment_boundary() {
        let texts = ["krate::billing::apply_discount"];
        let error = resolve_texts(&texts, "discount").expect_err("no boundary match");
        assert_eq!(
            error,
            LookupError::NotFound {
                query: "discount".to_owned(),
                suggestions: Vec::new(),
            }
        );
    }

    #[test]
    fn an_ambiguous_suffix_lists_sorted_candidates() {
        let texts = [
            "krate::tax::rounds",
            "krate::billing::rounds",
            "krate::billing::rounds",
        ];
        let error = resolve_texts(&texts, "rounds").expect_err("three matches");
        assert_eq!(
            error,
            LookupError::Ambiguous {
                query: "rounds".to_owned(),
                candidates: vec![
                    "krate::billing::rounds".to_owned(),
                    "krate::tax::rounds".to_owned(),
                ],
            }
        );
    }

    #[test]
    fn an_ambiguous_suffix_lists_at_most_twenty() {
        let texts: Vec<String> = (0..25)
            .map(|index| format!("crate{index}::rounds"))
            .collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let error = resolve_texts(&texts, "rounds").expect_err("many matches");
        match error {
            LookupError::Ambiguous { candidates, .. } => {
                assert_eq!(candidates.len(), MAX_AMBIGUOUS);
                assert_eq!(candidates[0], "crate0::rounds");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn a_failed_lookup_suggests_a_near_miss() {
        let texts = ["krate::billing::apply_discount", "krate::tax::rate"];
        let error = resolve_texts(&texts, "apply_discout").expect_err("a typo");
        assert_eq!(
            error,
            LookupError::NotFound {
                query: "apply_discout".to_owned(),
                suggestions: vec!["krate::billing::apply_discount".to_owned()],
            }
        );
    }

    #[test]
    fn a_failed_lookup_suggests_nothing_for_garbage() {
        let texts = ["krate::tax::rate"];
        let error = resolve_texts(&texts, "zzzzzzzzzz").expect_err("garbage");
        assert_eq!(
            error,
            LookupError::NotFound {
                query: "zzzzzzzzzz".to_owned(),
                suggestions: Vec::new(),
            }
        );
    }

    #[test]
    fn edit_distance_counts_edits() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("discount", "discout"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
    }
}
