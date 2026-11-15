//! Which lines of a file a comparison actually changes.

use std::ops::RangeInclusive;

use similar::{ChangeTag, TextDiff};

/// The lines of `head` that differ from `base`, as inclusive 1-based ranges
/// in `head`, in ascending order and never overlapping.
///
/// `base` is `None` when the file did not exist before, in which case the
/// whole file counts as changed. An empty file has no changed lines.
pub fn changed_lines(base: Option<&str>, head: &str) -> Vec<RangeInclusive<u32>> {
    let line_count = u32::try_from(head.lines().count()).unwrap_or(u32::MAX);
    let Some(base) = base else {
        return if line_count == 0 {
            Vec::new()
        } else {
            std::iter::once(1..=line_count).collect()
        };
    };

    let diff = TextDiff::from_lines(base, head);
    let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
    let mut previous: Option<u32> = None;
    for change in diff.iter_all_changes() {
        if change.tag() != ChangeTag::Insert {
            continue;
        }
        let Some(index) = change.new_index() else {
            continue;
        };
        let line = u32::try_from(index + 1).unwrap_or(u32::MAX);
        match ranges.last_mut() {
            // Adjacent lines belong to the same range.
            Some(last) if previous == Some(line.saturating_sub(1)) => {
                *last = *last.start()..=line;
            }
            _ => ranges.push(line..=line),
        }
        previous = Some(line);
    }
    ranges
}

/// Whether any of the `ranges` overlaps `start..=end`.
pub fn overlaps(ranges: &[RangeInclusive<u32>], start: u32, end: u32) -> bool {
    ranges
        .iter()
        .any(|range| *range.start() <= end && start <= *range.end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<(u32, u32)> {
        changed_lines(Some("a\nb\nc\nd\n"), text)
            .into_iter()
            .map(|range| (*range.start(), *range.end()))
            .collect()
    }

    #[test]
    fn identical_text_changes_nothing() {
        assert!(lines("a\nb\nc\nd\n").is_empty());
    }

    #[test]
    fn one_changed_line() {
        assert_eq!(lines("a\nB\nc\nd\n"), vec![(2, 2)]);
    }

    #[test]
    fn adjacent_changed_lines_form_one_range() {
        assert_eq!(lines("a\nB\nC\nd\n"), vec![(2, 3)]);
    }

    #[test]
    fn separate_changed_lines_stay_separate() {
        assert_eq!(lines("a\nB\nc\nD\n"), vec![(2, 2), (4, 4)]);
    }

    #[test]
    fn an_inserted_line_is_reported() {
        assert_eq!(lines("a\nnew\nb\nc\nd\n"), vec![(2, 2)]);
    }

    #[test]
    fn a_deleted_line_reports_nothing() {
        // Deleting `b` is a change to the file, but no line of the head is new.
        assert!(lines("a\nc\nd\n").is_empty());
    }

    #[test]
    fn a_new_file_is_changed_throughout() {
        let ranges = changed_lines(None, "a\nb\n");
        assert_eq!(
            ranges
                .into_iter()
                .map(|range| (*range.start(), *range.end()))
                .collect::<Vec<_>>(),
            vec![(1, 2)]
        );
    }

    #[test]
    fn an_empty_new_file_has_no_changed_lines() {
        assert!(changed_lines(None, "").is_empty());
    }

    #[test]
    fn overlap_is_inclusive() {
        let ranges = vec![2..=3, 7..=7];
        assert!(overlaps(&ranges, 1, 2));
        assert!(overlaps(&ranges, 3, 4));
        assert!(overlaps(&ranges, 7, 9));
        assert!(!overlaps(&ranges, 4, 6));
        assert!(!overlaps(&ranges, 8, 9));
        assert!(!overlaps(&[], 1, 100));
    }
}
