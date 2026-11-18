//! The one selected symbol, shared by every view, and where it has been.

use blastr_model::{Location, SymbolPath};

/// How many selections the history remembers.
const HISTORY_LIMIT: usize = 200;

/// What the user has selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The selected symbol.
    pub symbol: SymbolPath,
    /// The reference to show in the source, when the selection came from a
    /// step of an evidence path.
    pub site: Option<Location>,
}

impl Selection {
    /// Selects a symbol as a whole.
    pub fn symbol(symbol: SymbolPath) -> Selection {
        Selection { symbol, site: None }
    }

    /// Selects a symbol at one reference inside it.
    pub fn at_site(symbol: SymbolPath, site: Location) -> Selection {
        Selection {
            symbol,
            site: Some(site),
        }
    }
}

/// The selections made so far, with a cursor for going back and forward.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    entries: Vec<Selection>,
    /// Index of the current selection; meaningless while `entries` is empty.
    cursor: usize,
}

impl History {
    /// The current selection.
    pub fn current(&self) -> Option<&Selection> {
        self.entries.get(self.cursor)
    }

    /// Makes `selection` the current one.
    ///
    /// Anything that could be gone forward to is dropped, as in a browser.
    /// Selecting the same symbol again only moves the reference shown.
    pub fn select(&mut self, selection: Selection) {
        if let Some(current) = self.entries.get_mut(self.cursor)
            && current.symbol == selection.symbol
        {
            current.site = selection.site;
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(selection);
        if self.entries.len() > HISTORY_LIMIT {
            let extra = self.entries.len() - HISTORY_LIMIT;
            self.entries.drain(..extra);
        }
        self.cursor = self.entries.len() - 1;
    }

    /// Whether there is an earlier selection to return to.
    pub fn can_go_back(&self) -> bool {
        self.cursor > 0 && !self.entries.is_empty()
    }

    /// Whether there is a later selection to return to.
    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// Moves to the earlier selection and returns it.
    pub fn back(&mut self) -> Option<&Selection> {
        if !self.can_go_back() {
            return None;
        }
        self.cursor -= 1;
        self.current()
    }

    /// Moves to the later selection and returns it.
    pub fn forward(&mut self) -> Option<&Selection> {
        if !self.can_go_forward() {
            return None;
        }
        self.cursor += 1;
        self.current()
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.cursor = 0;
    }

    /// Drops the selections `keep` rejects, keeping the cursor on the nearest
    /// selection that is left.
    pub fn retain(&mut self, mut keep: impl FnMut(&Selection) -> bool) {
        let mut kept = Vec::with_capacity(self.entries.len());
        let mut cursor = 0;
        for (index, entry) in self.entries.drain(..).enumerate() {
            if keep(&entry) {
                if index <= self.cursor {
                    cursor = kept.len();
                }
                kept.push(entry);
            }
        }
        self.entries = kept;
        self.cursor = cursor;
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::{RelPath, Span, SymbolKind};

    use super::*;

    fn symbol(name: &str) -> SymbolPath {
        SymbolPath::new(format!("krate::{name}"), SymbolKind::Function)
    }

    fn select(history: &mut History, name: &str) {
        history.select(Selection::symbol(symbol(name)));
    }

    fn current(history: &History) -> Option<&str> {
        history.current().map(|selection| selection.symbol.name())
    }

    #[test]
    fn an_empty_history_has_nothing_to_go_to() {
        let mut history = History::default();
        assert_eq!(history.current(), None);
        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), None);
        assert_eq!(history.forward(), None);
    }

    #[test]
    fn back_and_forward_walk_the_selections() {
        let mut history = History::default();
        select(&mut history, "a");
        select(&mut history, "b");
        select(&mut history, "c");
        assert_eq!(current(&history), Some("c"));

        history.back();
        history.back();
        assert_eq!(current(&history), Some("a"));
        assert!(!history.can_go_back());

        history.forward();
        assert_eq!(current(&history), Some("b"));
        assert!(history.can_go_forward());
    }

    #[test]
    fn a_new_selection_drops_what_was_ahead() {
        let mut history = History::default();
        select(&mut history, "a");
        select(&mut history, "b");
        select(&mut history, "c");
        history.back();
        history.back();

        select(&mut history, "d");

        assert_eq!(current(&history), Some("d"));
        assert!(!history.can_go_forward());
        history.back();
        assert_eq!(current(&history), Some("a"));
    }

    #[test]
    fn selecting_the_same_symbol_again_only_moves_the_site() {
        let mut history = History::default();
        select(&mut history, "a");
        let site = Location {
            file: RelPath::new("src/lib.rs"),
            span: Span {
                start: 0,
                end: 1,
                start_line: 4,
                end_line: 4,
            },
        };

        history.select(Selection::at_site(symbol("a"), site.clone()));

        assert!(!history.can_go_back());
        assert_eq!(history.current().and_then(|s| s.site.clone()), Some(site));
    }

    #[test]
    fn the_history_is_bounded() {
        let mut history = History::default();
        for index in 0..HISTORY_LIMIT + 20 {
            select(&mut history, &format!("f{index}"));
        }
        let mut steps = 0;
        while history.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_LIMIT - 1);
    }

    #[test]
    fn retain_keeps_the_cursor_on_a_remaining_selection() {
        let mut history = History::default();
        select(&mut history, "a");
        select(&mut history, "b");
        select(&mut history, "c");
        history.back();

        history.retain(|selection| selection.symbol.name() != "b");

        assert_eq!(current(&history), Some("a"));
        assert!(history.can_go_forward());

        history.retain(|_| false);
        assert_eq!(history.current(), None);
    }
}
