//! The commit list: what is loaded, what a click means, and how old a commit is.

use blastr_model::CommitInfo;

use super::session::{CommitRef, Compare};

/// How many commits one page holds.
pub const PAGE_SIZE: usize = 200;

/// The commits of the open repository, loaded page by page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommitList {
    /// The commits loaded so far, newest first.
    pub commits: Vec<CommitInfo>,
    /// A page is being read.
    pub loading: bool,
    /// The last page came back short: there is nothing older.
    pub exhausted: bool,
    /// Why the list could not be read.
    pub failure: Option<String>,
    /// The commit a shift-click extends from.
    anchor: Option<String>,
}

/// One row of the list as it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitRow {
    /// The uncommitted changes.
    WorkingTree,
    /// The commit at this index of [`CommitList::commits`].
    Commit(usize),
}

impl CommitList {
    /// Forgets the list, as when another repository is opened.
    pub fn reset(&mut self) {
        *self = CommitList::default();
    }

    /// The page to read next as `(skip, limit)`, or `None` when a page is
    /// already being read or nothing older exists. Marks the list as loading.
    pub fn next_page(&mut self) -> Option<(usize, usize)> {
        if self.loading || self.exhausted {
            return None;
        }
        self.loading = true;
        self.failure = None;
        Some((self.commits.len(), PAGE_SIZE))
    }

    /// A page arrived. A page that does not continue the list is dropped.
    pub fn page_arrived(&mut self, skip: usize, page: Vec<CommitInfo>) {
        self.loading = false;
        if skip != self.commits.len() {
            return;
        }
        self.exhausted = page.len() < PAGE_SIZE;
        self.commits.extend(page);
    }

    /// Reading a page failed.
    pub fn page_failed(&mut self, message: String) {
        self.loading = false;
        self.failure = Some(message);
    }

    /// Whether the row at `row` is close enough to the end that the next page
    /// should be read.
    pub fn wants_more(&self, visible_end: usize) -> bool {
        !self.loading && !self.exhausted && visible_end + PAGE_SIZE / 4 >= self.commits.len()
    }

    /// The rows to show: the working tree first when it has changes.
    pub fn row_count(&self, dirty: bool) -> usize {
        self.commits.len() + usize::from(dirty)
    }

    /// The row at `index`.
    pub fn row(&self, index: usize, dirty: bool) -> Option<CommitRow> {
        if dirty {
            if index == 0 {
                return Some(CommitRow::WorkingTree);
            }
            return (index - 1 < self.commits.len()).then_some(CommitRow::Commit(index - 1));
        }
        (index < self.commits.len()).then_some(CommitRow::Commit(index))
    }

    /// What clicking a row compares.
    ///
    /// A plain click on a commit compares it with its parent. A shift-click
    /// extends from the commit clicked before to this one and compares the
    /// whole run, the changes of its oldest commit included.
    pub fn click(&mut self, row: CommitRow, shift: bool) -> Option<Compare> {
        let index = match row {
            CommitRow::WorkingTree => {
                self.anchor = None;
                return Some(Compare::WorkingTree);
            }
            CommitRow::Commit(index) => index,
        };
        let clicked = self.commits.get(index)?;
        let anchor = self
            .anchor
            .as_ref()
            .and_then(|id| self.commits.iter().position(|commit| &commit.id == id));
        match anchor {
            Some(anchor) if shift && anchor != index => {
                // The list is newest first, so the larger index is the older commit.
                let oldest = self.commits.get(anchor.max(index))?;
                let newest = self.commits.get(anchor.min(index))?;
                Some(Compare::Range {
                    oldest: commit_ref(oldest),
                    newest: commit_ref(newest),
                    from_parent: oldest.parents > 0,
                })
            }
            _ => {
                self.anchor = Some(clicked.id.clone());
                Some(Compare::Commit(commit_ref(clicked)))
            }
        }
    }

    /// The commit indices a comparison covers, as an inclusive range.
    pub fn selected(&self, compare: &Compare) -> Option<(usize, usize)> {
        let position = |id: &str| self.commits.iter().position(|commit| commit.id == id);
        match compare {
            Compare::WorkingTree => None,
            Compare::Commit(commit) => position(&commit.id).map(|index| (index, index)),
            Compare::Range { oldest, newest, .. } => {
                Some((position(&newest.id)?, position(&oldest.id)?))
            }
        }
    }
}

/// The reference to a listed commit.
pub fn commit_ref(commit: &CommitInfo) -> CommitRef {
    CommitRef {
        id: commit.id.clone(),
        short: commit.short_id.clone(),
    }
}

/// How long ago `time` was, seen from `now`; both in seconds since the Unix
/// epoch. Never a calendar date.
pub fn relative_time(time: i64, now: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let age = now.saturating_sub(time);
    match age {
        age if age < MINUTE => "just now".to_owned(),
        age if age < HOUR => format!("{}m ago", age / MINUTE),
        age if age < DAY => format!("{}h ago", age / HOUR),
        age if age < 2 * DAY => "yesterday".to_owned(),
        age if age < 14 * DAY => format!("{}d ago", age / DAY),
        age if age < 60 * DAY => format!("{}w ago", age / (7 * DAY)),
        age if age < 365 * DAY => format!("{}mo ago", age / (30 * DAY)),
        age => format!("{}y ago", age / (365 * DAY)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(index: usize, parents: u32) -> CommitInfo {
        CommitInfo {
            id: format!("{index:040x}"),
            short_id: format!("{index:07x}"),
            subject: format!("commit {index}"),
            author: "A".to_owned(),
            time: 0,
            parents,
        }
    }

    /// Five commits, newest first; the last one is the root.
    fn list() -> CommitList {
        let mut list = CommitList::default();
        let (skip, _) = list.next_page().expect("the first page");
        list.page_arrived(
            skip,
            (0..5)
                .map(|index| commit(index, u32::from(index != 4)))
                .collect(),
        );
        list
    }

    #[test]
    fn pages_continue_where_the_list_ends() {
        let mut list = CommitList::default();
        assert_eq!(list.next_page(), Some((0, PAGE_SIZE)));
        assert_eq!(list.next_page(), None, "one page at a time");

        list.page_arrived(0, (0..PAGE_SIZE).map(|index| commit(index, 1)).collect());
        assert!(!list.exhausted);
        assert_eq!(list.next_page(), Some((PAGE_SIZE, PAGE_SIZE)));

        list.page_arrived(PAGE_SIZE, vec![commit(PAGE_SIZE, 0)]);
        assert!(list.exhausted);
        assert_eq!(list.next_page(), None);
        assert_eq!(list.commits.len(), PAGE_SIZE + 1);
    }

    #[test]
    fn a_page_for_another_position_is_dropped() {
        let mut list = list();
        list.exhausted = false;
        list.next_page();

        list.page_arrived(0, vec![commit(9, 1)]);

        assert_eq!(list.commits.len(), 5);
        assert!(!list.loading);
    }

    #[test]
    fn a_failed_page_can_be_asked_for_again() {
        let mut list = CommitList::default();
        list.next_page();
        list.page_failed("git error".to_owned());
        assert_eq!(list.failure.as_deref(), Some("git error"));
        assert_eq!(list.next_page(), Some((0, PAGE_SIZE)));
        assert_eq!(list.failure, None);
    }

    #[test]
    fn the_working_tree_row_comes_first_when_dirty() {
        let list = list();
        assert_eq!(list.row_count(true), 6);
        assert_eq!(list.row(0, true), Some(CommitRow::WorkingTree));
        assert_eq!(list.row(1, true), Some(CommitRow::Commit(0)));
        assert_eq!(list.row(6, true), None);
        assert_eq!(list.row_count(false), 5);
        assert_eq!(list.row(0, false), Some(CommitRow::Commit(0)));
        assert_eq!(list.row(5, false), None);
    }

    #[test]
    fn a_click_compares_a_commit_with_its_parent() {
        let mut list = list();
        let compare = list.click(CommitRow::Commit(2), false);
        assert_eq!(compare, Some(Compare::Commit(commit_ref(&list.commits[2]))));
        assert_eq!(list.selected(&compare.expect("a comparison")), Some((2, 2)));
    }

    #[test]
    fn a_shift_click_compares_the_run_between_two_clicks() {
        let mut list = list();
        list.click(CommitRow::Commit(1), false);

        let compare = list
            .click(CommitRow::Commit(3), true)
            .expect("a comparison");

        assert_eq!(
            compare,
            Compare::Range {
                oldest: commit_ref(&list.commits[3]),
                newest: commit_ref(&list.commits[1]),
                from_parent: true,
            }
        );
        assert_eq!(list.selected(&compare), Some((1, 3)));

        // Extending upwards from the same anchor.
        let upwards = list
            .click(CommitRow::Commit(0), true)
            .expect("a comparison");
        assert_eq!(list.selected(&upwards), Some((0, 1)));
    }

    #[test]
    fn a_range_down_to_the_root_commit_starts_at_the_root() {
        let mut list = list();
        list.click(CommitRow::Commit(2), false);
        let compare = list.click(CommitRow::Commit(4), true);
        assert!(matches!(
            compare,
            Some(Compare::Range {
                from_parent: false,
                ..
            })
        ));
    }

    #[test]
    fn a_shift_click_without_an_anchor_is_a_plain_click() {
        let mut list = list();
        assert!(matches!(
            list.click(CommitRow::Commit(2), true),
            Some(Compare::Commit(_))
        ));
        assert_eq!(
            list.click(CommitRow::WorkingTree, true),
            Some(Compare::WorkingTree)
        );
        assert_eq!(list.selected(&Compare::WorkingTree), None);
        assert_eq!(list.click(CommitRow::Commit(99), false), None);
    }

    #[test]
    fn more_is_wanted_near_the_end_of_the_list() {
        let mut list = CommitList {
            commits: (0..PAGE_SIZE).map(|index| commit(index, 1)).collect(),
            ..CommitList::default()
        };
        assert!(!list.wants_more(10));
        assert!(list.wants_more(PAGE_SIZE - 10));
        list.exhausted = true;
        assert!(!list.wants_more(PAGE_SIZE - 10));
    }

    #[test]
    fn ages_are_relative_and_never_dates() {
        const HOUR: i64 = 3600;
        const DAY: i64 = 24 * HOUR;
        let now = 1_000 * DAY;
        assert_eq!(relative_time(now - 5, now), "just now");
        assert_eq!(relative_time(now + 50, now), "just now");
        assert_eq!(relative_time(now - 120, now), "2m ago");
        assert_eq!(relative_time(now - 5 * HOUR, now), "5h ago");
        assert_eq!(relative_time(now - 30 * HOUR, now), "yesterday");
        assert_eq!(relative_time(now - 3 * DAY, now), "3d ago");
        assert_eq!(relative_time(now - 21 * DAY, now), "3w ago");
        assert_eq!(relative_time(now - 100 * DAY, now), "3mo ago");
        assert_eq!(relative_time(now - 800 * DAY, now), "2y ago");
    }
}
