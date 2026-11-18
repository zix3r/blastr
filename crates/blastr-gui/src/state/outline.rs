//! The rows of the left column: the changes, then what they affect.
//!
//! The lists are virtualised, so they are flattened here into rows of known
//! height. The order inside a block is the engine's order, which is the order
//! the result names its items in.

use blastr_model::{
    Analysis, Change, ChangeKind, Confidence, DiffResult, FileStatus, ImpactClass, ImpactItem,
    RelPath, SelectedTest, SymbolPath, TestCommand,
};

/// One of the blocks of the left column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    /// The cause: what changed.
    Changes,
    /// The effect that has to be edited.
    NeedsEditing,
    /// The effect that still compiles but may behave differently.
    MayBehave,
}

/// One row of the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The header of a block, with how many entries it holds.
    Head {
        /// Which block.
        block: Block,
        /// Entries in the block, before the text filter.
        count: usize,
        /// Whether the block's rows are hidden.
        collapsed: bool,
    },
    /// The line that separates the cause from its effects.
    ImpactGroup,
    /// The file the following changes are in.
    File {
        /// The path.
        path: RelPath,
        /// How the file itself changed, when it is among the changed files.
        status: Option<FileStatus>,
    },
    /// A change; the index is into [`DiffResult::changes`].
    Change(usize),
    /// An affected item; the index is into [`Analysis::items`].
    Item {
        /// Index into [`Analysis::items`].
        index: usize,
        /// Whether the item is in the dimmed "edited with the change" group.
        dim: bool,
    },
    /// The line above the adaptations the change already made itself.
    Edited(usize),
    /// The line that reveals the rest of a long block.
    More {
        /// Which block the row belongs to.
        block: Block,
        /// How many entries of the block are still hidden.
        hidden: usize,
    },
    /// A sentence in place of rows.
    Note(String),
    /// A placeholder while the block is still being computed.
    Skeleton,
    /// Space that ends a block.
    Gap,
}

/// The heights the views draw the outline's rows at, in pixels.
///
/// [`Row::height`] returns these numbers and the views use the same ones, so
/// the virtualised list never clips a row or leaves a gap under one.
pub mod height {
    /// A block header.
    pub const HEAD: f32 = 32.0;
    /// The line that separates the cause from its effects.
    pub const GROUP_HEAD: f32 = 28.0;
    /// The file above a run of changes.
    pub const FILE: f32 = 22.0;
    /// One change: a single line.
    pub const CHANGE: f32 = 28.0;
    /// One affected item: a name line and a reason line.
    pub const ITEM: f32 = 38.0;
    /// The line above the items that were edited with the change.
    pub const EDITED: f32 = 22.0;
    /// The "Show N more" line.
    pub const MORE: f32 = 26.0;
    /// A placeholder while a block is computed.
    pub const SKELETON: f32 = 26.0;
    /// The space that ends a block.
    pub const GAP: f32 = 10.0;
    /// The padding above and below a note.
    pub const NOTE: f32 = 8.0;
    /// The height of one line of a note.
    pub const NOTE_LINE: f32 = 17.0;
}

impl Row {
    /// The height of the row in pixels.
    ///
    /// The views draw every row at exactly this height; the two-line impact
    /// rows hold an 18 px name line and a 15 px reason line with 2 px between
    /// them, centred in 38 px.
    pub fn height(&self) -> f32 {
        match self {
            Row::Head { .. } => height::HEAD,
            Row::ImpactGroup => height::GROUP_HEAD,
            Row::File { .. } => height::FILE,
            Row::Change(_) => height::CHANGE,
            Row::Item { .. } => height::ITEM,
            Row::Edited(_) => height::EDITED,
            Row::More { .. } => height::MORE,
            Row::Note(text) => note_height(text),
            Row::Skeleton => height::SKELETON,
            Row::Gap => height::GAP,
        }
    }

    /// The symbol the row selects, if it selects one.
    pub fn symbol<'a>(
        &self,
        changes: Option<&'a DiffResult>,
        analysis: Option<&'a Analysis>,
    ) -> Option<&'a SymbolPath> {
        match self {
            Row::Change(index) => changes?.changes.get(*index).map(|change| &change.symbol),
            Row::Item { index, .. } => analysis?.items.get(*index).map(|item| &item.symbol),
            _ => None,
        }
    }
}

/// The height of a sentence that wraps inside the left column.
///
/// At the narrowest width of the column about 38 characters fit on a line;
/// no sentence of the outline is longer than three lines.
fn note_height(text: &str) -> f32 {
    const PER_LINE: usize = 38;
    let lines = text.chars().count().div_ceil(PER_LINE).clamp(1, 3);
    height::NOTE + height::NOTE_LINE * lines as f32
}

/// How many rows a long block lists before its "Show more" row.
pub const BLOCK_LIMIT: usize = 50;

/// How many rows one click on "Show more" adds.
pub const BLOCK_STEP: usize = 100;

/// What narrows the lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Text every listed symbol must contain; case is ignored.
    pub text: String,
    /// The lowest confidence listed.
    pub min_confidence: Confidence,
    /// The blocks whose rows are hidden.
    pub collapsed: Vec<Block>,
    /// The blocks that were asked for more rows, and how many they show.
    pub shown: Vec<(Block, usize)>,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            text: String::new(),
            min_confidence: Confidence::Possible,
            collapsed: Vec::new(),
            shown: Vec::new(),
        }
    }
}

impl Filter {
    /// Whether `symbol` passes the text filter.
    pub fn matches(&self, symbol: &str) -> bool {
        let needle = self.text.trim();
        needle.is_empty() || contains_ignoring_case(symbol, needle)
    }

    /// Whether `block` is collapsed.
    pub fn is_collapsed(&self, block: Block) -> bool {
        self.collapsed.contains(&block)
    }

    /// Collapses or expands `block`.
    pub fn toggle(&mut self, block: Block) {
        if let Some(position) = self.collapsed.iter().position(|entry| *entry == block) {
            self.collapsed.remove(position);
        } else {
            self.collapsed.push(block);
        }
    }

    /// How many rows `block` lists before its "Show more" row.
    pub fn limit(&self, block: Block) -> usize {
        self.shown
            .iter()
            .find(|(entry, _)| *entry == block)
            .map_or(BLOCK_LIMIT, |(_, count)| *count)
    }

    /// Reveals another step of rows in `block`.
    pub fn show_more(&mut self, block: Block) {
        let limit = self.limit(block).saturating_add(BLOCK_STEP);
        match self.shown.iter_mut().find(|(entry, _)| *entry == block) {
            Some((_, count)) => *count = limit,
            None => self.shown.push((block, limit)),
        }
    }

    /// The next confidence filter in the cycle all → likely → definite → all.
    pub fn next_confidence(&self) -> Confidence {
        match self.min_confidence {
            Confidence::Possible => Confidence::Likely,
            Confidence::Likely => Confidence::Definite,
            Confidence::Definite => Confidence::Possible,
        }
    }
}

/// Whether `haystack` contains `needle`, ignoring ASCII case.
pub fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// The counts shown in the block headers and in the summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Structural changes, formatting-only ones left out.
    pub changes: usize,
    /// Changes that only touch formatting or comments.
    pub formatting_only: usize,
    /// Items that still need editing.
    pub needs_editing: usize,
    /// Adaptations the change already made itself.
    pub edited: usize,
    /// Items that may behave differently.
    pub may_behave: usize,
}

impl Counts {
    /// Counts the changes of a diff.
    pub fn of_changes(diff: &DiffResult) -> Counts {
        let formatting_only = diff
            .changes
            .iter()
            .filter(|change| change.kind == ChangeKind::NonSemantic)
            .count();
        Counts {
            changes: diff.changes.len() - formatting_only,
            formatting_only,
            ..Counts::default()
        }
    }

    /// Counts everything in an analysis, the way the command line counts it.
    pub fn of(analysis: &Analysis) -> Counts {
        let mut counts = Counts::of_changes(&analysis.diff);
        for item in &analysis.items {
            match (item.class, item.tags.changed) {
                (ImpactClass::Adaptation, false) => counts.needs_editing += 1,
                (ImpactClass::Adaptation, true) => counts.edited += 1,
                (ImpactClass::Behaviour, false) => counts.may_behave += 1,
                // An item that changed itself and may behave differently is
                // already listed as a change.
                (ImpactClass::Behaviour, true) => {}
            }
        }
        counts
    }
}

/// The flattened left column.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outline {
    /// The rows, top to bottom.
    pub rows: Vec<Row>,
    /// The counts of the shown result.
    pub counts: Counts,
}

impl Outline {
    /// Builds the rows.
    ///
    /// `changes` may arrive before `analysis`; while `loading`, the blocks
    /// that are still being computed hold placeholders.
    pub fn build(
        changes: Option<&DiffResult>,
        analysis: Option<&Analysis>,
        loading: bool,
        filter: &Filter,
    ) -> Outline {
        let changes = analysis.map(|analysis| &analysis.diff).or(changes);
        let counts = match (analysis, changes) {
            (Some(analysis), _) => Counts::of(analysis),
            (None, Some(diff)) => Counts::of_changes(diff),
            (None, None) => Counts::default(),
        };
        let mut rows = Vec::new();

        // The cause.
        let collapsed = filter.is_collapsed(Block::Changes);
        rows.push(Row::Head {
            block: Block::Changes,
            count: counts.changes,
            collapsed,
        });
        if !collapsed {
            match changes {
                Some(diff) => change_rows(diff, counts, filter, &mut rows),
                None if loading => rows.extend([Row::Skeleton, Row::Skeleton]),
                None => {}
            }
        }
        rows.push(Row::Gap);

        // The effects.
        rows.push(Row::ImpactGroup);
        item_block(
            Block::NeedsEditing,
            counts.needs_editing,
            analysis,
            loading,
            filter,
            &mut rows,
        );
        item_block(
            Block::MayBehave,
            counts.may_behave,
            analysis,
            loading,
            filter,
            &mut rows,
        );

        Outline { rows, counts }
    }

    /// The index of the row that selects `symbol`.
    pub fn position(
        &self,
        symbol: &SymbolPath,
        changes: Option<&DiffResult>,
        analysis: Option<&Analysis>,
    ) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.symbol(changes, analysis) == Some(symbol))
    }

    /// The selectable row after (`forward`) or before the row at `from`;
    /// with no starting row, the first or last selectable one.
    pub fn step(&self, from: Option<usize>, forward: bool) -> Option<usize> {
        let selectable = |row: &Row| matches!(row, Row::Change(_) | Row::Item { .. });
        match (from, forward) {
            (Some(from), true) => self
                .rows
                .iter()
                .enumerate()
                .skip(from + 1)
                .find(|(_, row)| selectable(row))
                .map(|(index, _)| index),
            (Some(from), false) => self
                .rows
                .iter()
                .enumerate()
                .take(from)
                .rev()
                .find(|(_, row)| selectable(row))
                .map(|(index, _)| index),
            (None, true) => self.rows.iter().position(selectable),
            (None, false) => self.rows.iter().rposition(selectable),
        }
    }
}

/// The rows of the changes block: grouped by file, in the engine's order.
fn change_rows(diff: &DiffResult, counts: Counts, filter: &Filter, rows: &mut Vec<Row>) {
    let mut listed = 0usize;
    let mut current: Option<&RelPath> = None;
    for (index, change) in diff.changes.iter().enumerate() {
        if change.kind == ChangeKind::NonSemantic || !filter.matches(&change.symbol.path) {
            continue;
        }
        let file = change_file(change);
        if file != current || listed == 0 {
            if let Some(path) = file {
                let status = diff
                    .changed_files
                    .iter()
                    .find(|changed| &changed.path == path)
                    .map(|changed| changed.status);
                rows.push(Row::File {
                    path: path.clone(),
                    status,
                });
            }
            current = file;
        }
        rows.push(Row::Change(index));
        listed += 1;
    }

    if listed == 0 {
        let note = if counts.changes > 0 {
            "No change matches the filter.".to_owned()
        } else if counts.formatting_only > 0 {
            let files = diff.changed_files.len();
            let noun = if files == 1 {
                "file differs"
            } else {
                "files differ"
            };
            format!("No structural changes. {files} {noun} in formatting or comments only.")
        } else {
            "No structural changes in this comparison.".to_owned()
        };
        rows.push(Row::Note(note));
    }
}

/// The file a change is listed under.
fn change_file(change: &Change) -> Option<&RelPath> {
    change
        .new
        .as_ref()
        .or(change.old.as_ref())
        .map(|location| &location.file)
}

/// One effect block: its header, then its items best first.
fn item_block(
    block: Block,
    count: usize,
    analysis: Option<&Analysis>,
    loading: bool,
    filter: &Filter,
    rows: &mut Vec<Row>,
) {
    let collapsed = filter.is_collapsed(block);
    rows.push(Row::Head {
        block,
        count,
        collapsed,
    });
    if collapsed {
        rows.push(Row::Gap);
        return;
    }
    let Some(analysis) = analysis else {
        if loading {
            rows.extend([Row::Skeleton, Row::Skeleton, Row::Skeleton]);
        }
        rows.push(Row::Gap);
        return;
    };

    let class = match block {
        Block::NeedsEditing => ImpactClass::Adaptation,
        _ => ImpactClass::Behaviour,
    };
    let listed = |item: &ImpactItem| {
        item.class == class
            && item.confidence >= filter.min_confidence
            && filter.matches(&item.symbol.path)
    };
    let open: Vec<usize> = analysis
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| !item.tags.changed && listed(item))
        .map(|(index, _)| index)
        .collect();
    let edited: Vec<usize> = if class == ImpactClass::Adaptation {
        analysis
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.tags.changed && listed(item))
            .map(|(index, _)| index)
            .collect()
    } else {
        Vec::new()
    };

    if open.is_empty() {
        let narrowed =
            !filter.text.trim().is_empty() || filter.min_confidence != Confidence::Possible;
        let note = match (block, count > 0 && narrowed) {
            (_, true) => "Nothing matches the filter.",
            (Block::NeedsEditing, false) => "Nothing left to edit.",
            (_, false) => "Nothing else may behave differently.",
        };
        rows.push(Row::Note(note.to_owned()));
    }
    let shown = filter.limit(block).min(open.len());
    rows.extend(open[..shown].iter().map(|index| Row::Item {
        index: *index,
        dim: false,
    }));
    if !edited.is_empty() {
        rows.push(Row::Edited(edited.len()));
        rows.extend(
            edited
                .into_iter()
                .map(|index| Row::Item { index, dim: true }),
        );
    }
    if open.len() > shown {
        rows.push(Row::More {
            block,
            hidden: open.len() - shown,
        });
    }
    rows.push(Row::Gap);
}

/// One row of the tests pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestRow {
    /// The header of a package; the index is into the commands of the
    /// selection and names the package's first command.
    Package(usize),
    /// A test target; the index is into the commands of the selection.
    Target {
        /// Index of the [`TestCommand`].
        command: usize,
        /// Whether its tests are listed below it.
        expanded: bool,
    },
    /// A selected test; the index is into the selected tests.
    Test(usize),
    /// The command line that runs a target's selected tests.
    Command(usize),
    /// A sentence in place of rows.
    Note(String),
}

impl TestRow {
    /// The height of the row in pixels.
    pub fn height(&self) -> f32 {
        match self {
            TestRow::Package(_) => 22.0,
            TestRow::Target { .. } => 28.0,
            TestRow::Test(_) => 22.0,
            TestRow::Command(_) => 30.0,
            TestRow::Note(text) => note_height(text).max(26.0),
        }
    }
}

/// The package a command belongs to, split for display: its name, and the
/// version when the command line carries one because more than one package of
/// the project has that name.
pub fn package_parts(package: &str) -> (&str, Option<&str>) {
    match package.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (package, None),
    }
}

/// The flattened tests pane: packages, their targets, and under an expanded
/// target its command and its tests.
pub fn test_rows(analysis: &Analysis, filter: &Filter, expanded: &[usize]) -> Vec<TestRow> {
    let selection = &analysis.tests;
    if selection.selected.is_empty() {
        let note = if selection.total == 0 {
            "This project has no tests."
        } else {
            "No test reaches the changed code."
        };
        return vec![TestRow::Note(note.to_owned())];
    }

    let mut rows = Vec::new();
    let mut package: Option<&str> = None;
    for (index, command) in selection.commands.iter().enumerate() {
        let tests: Vec<usize> = selection
            .selected
            .iter()
            .enumerate()
            .filter(|(_, test)| {
                test_target(test, &selection.commands) == Some(index)
                    && filter.matches(&test.symbol.path)
            })
            .map(|(position, _)| position)
            .collect();
        if !filter.text.trim().is_empty() && tests.is_empty() {
            continue;
        }
        if package != Some(command.package.as_str()) {
            package = Some(command.package.as_str());
            rows.push(TestRow::Package(index));
        }
        let is_expanded = expanded.contains(&index);
        rows.push(TestRow::Target {
            command: index,
            expanded: is_expanded,
        });
        if is_expanded {
            rows.push(TestRow::Command(index));
            rows.extend(tests.into_iter().map(TestRow::Test));
        }
    }
    if rows.is_empty() {
        rows.push(TestRow::Note("No test matches the filter.".to_owned()));
    }
    rows
}

/// The command a selected test belongs to: the target whose crate holds it.
pub fn test_target(test: &SelectedTest, commands: &[TestCommand]) -> Option<usize> {
    let krate = test.symbol.crate_name();
    commands
        .iter()
        .position(|command| command.target_name.replace('-', "_") == krate)
        .or_else(|| {
            commands
                .iter()
                .position(|command| command.package.replace('-', "_") == krate)
        })
}

/// The tests that reach `symbol`: selected tests that are affected items with
/// a path through it, or the symbol itself when it is a selected test.
pub fn covering_tests(analysis: &Analysis, symbol: &SymbolPath) -> Vec<usize> {
    analysis
        .tests
        .selected
        .iter()
        .enumerate()
        .filter(|(_, test)| {
            if &test.symbol == symbol {
                return true;
            }
            analysis
                .items
                .iter()
                .filter(|item| item.symbol == test.symbol)
                .flat_map(|item| item.paths.iter())
                .any(|path| path.steps.iter().any(|step| &step.from == symbol))
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        AnalysisParams, Certainty, ChangeDetail, ChangedFile, Comparison, EdgeDetail, EdgeKind,
        EvidencePath, EvidenceStep, ImpactLimits, ImpactTags, Location, Reason, Source, Span,
        SymbolKind, TargetKind, TestReason, TestSelection,
    };

    use super::*;

    fn location(file: &str, line: u32) -> Location {
        Location {
            file: RelPath::new(file),
            span: Span {
                start: 0,
                end: 1,
                start_line: line,
                end_line: line,
            },
        }
    }

    fn symbol(path: &str) -> SymbolPath {
        SymbolPath::new(path, SymbolKind::Function)
    }

    fn change(kind: ChangeKind, path: &str, file: &str) -> Change {
        Change {
            kind,
            symbol: symbol(path),
            old: None,
            new: Some(location(file, 1)),
            detail: ChangeDetail::None,
        }
    }

    fn step(from: &str, to: &str) -> EvidenceStep {
        EvidenceStep {
            from: symbol(from),
            to: symbol(to),
            edge: EdgeKind::Calls,
            certainty: Certainty::Resolved,
            detail: EdgeDetail::None,
            site: location("src/lib.rs", 3),
        }
    }

    fn item(path: &str, class: ImpactClass, confidence: Confidence, changed: bool) -> ImpactItem {
        ImpactItem {
            symbol: symbol(path),
            location: location("src/lib.rs", 9),
            class,
            confidence,
            score: 0.5,
            distance: 1,
            paths: vec![EvidencePath {
                change: 0,
                reason: Reason::CallsChanged,
                class,
                steps: vec![step(path, "k::changed")],
                score: 0.5,
            }],
            path_count: 1,
            tags: ImpactTags {
                changed,
                ..ImpactTags::default()
            },
        }
    }

    fn analysis(changes: Vec<Change>, items: Vec<ImpactItem>) -> Analysis {
        Analysis {
            diff: DiffResult {
                comparison: Comparison {
                    base: Source::Worktree,
                    head: Source::Worktree,
                },
                base_key: None,
                head_key: None,
                changed_files: vec![ChangedFile {
                    path: RelPath::new("src/a.rs"),
                    old_path: None,
                    status: FileStatus::Modified,
                }],
                changes,
                limits: Vec::new(),
            },
            items,
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        }
    }

    fn sample() -> Analysis {
        analysis(
            vec![
                change(ChangeKind::Signature, "k::apply", "src/a.rs"),
                change(ChangeKind::Body, "k::preview", "src/a.rs"),
                change(ChangeKind::NonSemantic, "k::fmt", "src/a.rs"),
                change(ChangeKind::Body, "k::vat", "src/b.rs"),
            ],
            vec![
                item(
                    "k::total",
                    ImpactClass::Adaptation,
                    Confidence::Definite,
                    false,
                ),
                item(
                    "k::summary",
                    ImpactClass::Behaviour,
                    Confidence::Likely,
                    false,
                ),
                item(
                    "k::preview",
                    ImpactClass::Adaptation,
                    Confidence::Definite,
                    true,
                ),
                item(
                    "k::dispatch",
                    ImpactClass::Behaviour,
                    Confidence::Possible,
                    false,
                ),
                item("k::vat", ImpactClass::Behaviour, Confidence::Likely, true),
            ],
        )
    }

    fn names(outline: &Outline, analysis: &Analysis) -> Vec<String> {
        outline
            .rows
            .iter()
            .map(|row| match row {
                Row::Head { block, count, .. } => format!("[{block:?} {count}]"),
                Row::ImpactGroup => "--impact--".to_owned(),
                Row::File { path, .. } => format!("file {path}"),
                Row::Change(index) => {
                    format!("change {}", analysis.diff.changes[*index].symbol.name())
                }
                Row::Item { index, dim } => {
                    let name = analysis.items[*index].symbol.name();
                    if *dim {
                        format!("dim {name}")
                    } else {
                        format!("item {name}")
                    }
                }
                Row::More { hidden, .. } => format!("more {hidden}"),
                Row::Edited(count) => format!("edited {count}"),
                Row::Note(text) => format!("note {text}"),
                Row::Skeleton => "skeleton".to_owned(),
                Row::Gap => "gap".to_owned(),
            })
            .collect()
    }

    #[test]
    fn the_cause_comes_first_and_each_effect_has_its_own_block() {
        let analysis = sample();
        let outline = Outline::build(None, Some(&analysis), false, &Filter::default());
        assert_eq!(
            names(&outline, &analysis),
            vec![
                "[Changes 3]",
                "file src/a.rs",
                "change apply",
                "change preview",
                "file src/b.rs",
                "change vat",
                "gap",
                "--impact--",
                "[NeedsEditing 1]",
                "item total",
                "edited 1",
                "dim preview",
                "gap",
                "[MayBehave 2]",
                "item summary",
                "item dispatch",
                "gap",
            ]
        );
        assert_eq!(
            outline.counts,
            Counts {
                changes: 3,
                formatting_only: 1,
                needs_editing: 1,
                edited: 1,
                may_behave: 2,
            }
        );
    }

    #[test]
    fn changes_are_listed_while_the_impact_is_still_traced() {
        let analysis = sample();
        let outline = Outline::build(Some(&analysis.diff), None, true, &Filter::default());
        let names = names(&outline, &analysis);
        assert_eq!(names[0], "[Changes 3]");
        assert!(names.contains(&"change vat".to_owned()));
        assert_eq!(
            names.iter().filter(|name| *name == "skeleton").count(),
            6,
            "{names:?}"
        );
    }

    #[test]
    fn nothing_known_yet_shows_placeholders_only_while_loading() {
        let loading = Outline::build(None, None, true, &Filter::default());
        assert!(loading.rows.contains(&Row::Skeleton));
        let idle = Outline::build(None, None, false, &Filter::default());
        assert!(!idle.rows.contains(&Row::Skeleton));
        assert_eq!(idle.counts, Counts::default());
    }

    #[test]
    fn the_confidence_filter_hides_weaker_items_but_keeps_the_counts() {
        let analysis = sample();
        let filter = Filter {
            min_confidence: Confidence::Likely,
            ..Filter::default()
        };
        let outline = Outline::build(None, Some(&analysis), false, &filter);
        let names = names(&outline, &analysis);
        assert!(names.contains(&"item summary".to_owned()));
        assert!(!names.contains(&"item dispatch".to_owned()));
        assert!(names.contains(&"[MayBehave 2]".to_owned()));
    }

    #[test]
    fn the_text_filter_applies_to_changes_and_items() {
        let analysis = sample();
        let filter = Filter {
            text: "VAT".to_owned(),
            ..Filter::default()
        };
        let outline = Outline::build(None, Some(&analysis), false, &filter);
        assert_eq!(
            names(&outline, &analysis),
            vec![
                "[Changes 3]",
                "file src/b.rs",
                "change vat",
                "gap",
                "--impact--",
                "[NeedsEditing 1]",
                "note Nothing matches the filter.",
                "gap",
                "[MayBehave 2]",
                "note Nothing matches the filter.",
                "gap",
            ]
        );
    }

    #[test]
    fn a_collapsed_block_keeps_its_header() {
        let analysis = sample();
        let mut filter = Filter::default();
        filter.toggle(Block::Changes);
        filter.toggle(Block::MayBehave);
        let outline = Outline::build(None, Some(&analysis), false, &filter);
        let names = names(&outline, &analysis);
        assert!(!names.iter().any(|name| name.starts_with("change ")));
        assert!(!names.contains(&"item summary".to_owned()));
        assert!(names.contains(&"item total".to_owned()));
        filter.toggle(Block::Changes);
        assert!(!filter.is_collapsed(Block::Changes));
    }

    #[test]
    fn formatting_only_comparisons_say_so() {
        let analysis = analysis(
            vec![change(ChangeKind::NonSemantic, "k::fmt", "src/a.rs")],
            Vec::new(),
        );
        let outline = Outline::build(None, Some(&analysis), false, &Filter::default());
        let names = names(&outline, &analysis);
        assert_eq!(
            names[1],
            "note No structural changes. 1 file differs in formatting or comments only."
        );
        assert!(names.contains(&"note Nothing left to edit.".to_owned()));
        assert!(names.contains(&"note Nothing else may behave differently.".to_owned()));
    }

    #[test]
    fn stepping_moves_between_selectable_rows_only() {
        let analysis = sample();
        let outline = Outline::build(None, Some(&analysis), false, &Filter::default());
        let first = outline.step(None, true).expect("a first row");
        assert_eq!(outline.rows[first], Row::Change(0));
        let second = outline.step(Some(first), true).expect("a second row");
        assert_eq!(outline.rows[second], Row::Change(1));
        let third = outline.step(Some(second), true).expect("a third row");
        assert_eq!(
            outline.rows[third],
            Row::Change(3),
            "the file row is skipped"
        );
        assert_eq!(outline.step(Some(third), false), Some(second));
        let last = outline.step(None, false).expect("a last row");
        assert_eq!(
            outline.rows[last],
            Row::Item {
                index: 3,
                dim: false
            }
        );
        assert_eq!(outline.step(Some(last), true), None);
        assert_eq!(outline.step(Some(first), false), None);
    }

    #[test]
    fn a_symbol_is_found_in_the_block_that_lists_it() {
        let analysis = sample();
        let outline = Outline::build(None, Some(&analysis), false, &Filter::default());
        let position = outline
            .position(&symbol("k::summary"), None, Some(&analysis))
            .expect("the item row");
        assert_eq!(
            outline.rows[position],
            Row::Item {
                index: 1,
                dim: false
            }
        );
        // A changed symbol is found in the changes block first.
        let position = outline
            .position(&symbol("k::preview"), Some(&analysis.diff), Some(&analysis))
            .expect("the change row");
        assert_eq!(outline.rows[position], Row::Change(1));
    }

    #[test]
    fn the_confidence_filter_cycles() {
        let mut filter = Filter::default();
        filter.min_confidence = filter.next_confidence();
        assert_eq!(filter.min_confidence, Confidence::Likely);
        filter.min_confidence = filter.next_confidence();
        assert_eq!(filter.min_confidence, Confidence::Definite);
        filter.min_confidence = filter.next_confidence();
        assert_eq!(filter.min_confidence, Confidence::Possible);
    }

    #[test]
    fn case_is_ignored_when_matching() {
        assert!(contains_ignoring_case("billing::Tax", "tax"));
        assert!(contains_ignoring_case("billing::Tax", ""));
        assert!(!contains_ignoring_case("tax", "billing::tax"));
        assert!(!contains_ignoring_case("billing", "tax"));
    }

    #[test]
    fn a_long_block_shows_fifty_rows_then_more_in_steps_of_a_hundred() {
        let items: Vec<ImpactItem> = (0..156)
            .map(|index| {
                item(
                    &format!("k::beh{index}"),
                    ImpactClass::Behaviour,
                    Confidence::Likely,
                    false,
                )
            })
            .collect();
        let analysis = analysis(vec![change(ChangeKind::Body, "k::c", "src/a.rs")], items);
        let mut filter = Filter::default();

        let outline = Outline::build(None, Some(&analysis), false, &filter);
        let listed = |outline: &Outline| {
            outline
                .rows
                .iter()
                .filter(|row| matches!(row, Row::Item { .. }))
                .count()
        };
        assert_eq!(listed(&outline), BLOCK_LIMIT);
        assert!(outline.rows.contains(&Row::More {
            block: Block::MayBehave,
            hidden: 156 - BLOCK_LIMIT,
        }));

        filter.show_more(Block::MayBehave);
        let outline = Outline::build(None, Some(&analysis), false, &filter);
        assert_eq!(listed(&outline), BLOCK_LIMIT + BLOCK_STEP);
        assert!(outline.rows.contains(&Row::More {
            block: Block::MayBehave,
            hidden: 156 - BLOCK_LIMIT - BLOCK_STEP,
        }));

        filter.show_more(Block::MayBehave);
        let outline = Outline::build(None, Some(&analysis), false, &filter);
        assert_eq!(listed(&outline), 156);
        assert!(
            !outline
                .rows
                .iter()
                .any(|row| matches!(row, Row::More { .. }))
        );
    }

    #[test]
    fn a_note_is_as_tall_as_it_wraps() {
        assert_eq!(note_height("Nothing left to edit."), 25.0);
        assert!(note_height(&"word ".repeat(40)) > note_height("Nothing left to edit."));
        assert_eq!(
            Row::Note("x".repeat(500)).height(),
            note_height(&"x".repeat(500))
        );
        assert_eq!(
            Row::Note("x".repeat(500)).height(),
            59.0,
            "three lines at most"
        );
    }

    #[test]
    fn a_two_line_impact_row_is_taller_than_a_one_line_row() {
        let item = Row::Item {
            index: 0,
            dim: false,
        };
        assert_eq!(item.height(), 38.0);
        assert!(item.height() > Row::Change(0).height());
        assert!(
            item.height()
                > Row::Head {
                    block: Block::MayBehave,
                    count: 1,
                    collapsed: false,
                }
                .height()
        );
    }

    fn with_tests(mut analysis: Analysis) -> Analysis {
        let test = |path: &str| SelectedTest {
            symbol: symbol(path),
            location: location("src/lib.rs", 40),
            reason: TestReason::Reached,
            filter: path
                .split_once("::")
                .map_or(path, |(_, rest)| rest)
                .to_owned(),
        };
        let command = |package: &str, kind: TargetKind, name: &str, tests: u32| TestCommand {
            package: package.to_owned(),
            target_kind: kind,
            target_name: name.to_owned(),
            filters: Vec::new(),
            tests,
            total: 10,
        };
        analysis.tests = TestSelection {
            selected: vec![
                test("k::tests::totals"),
                test("k::tests::rounding"),
                test("smoke_http::served"),
            ],
            total: 30,
            commands: vec![
                command("k", TargetKind::Lib, "k", 2),
                command("k", TargetKind::Test, "smoke-http", 1),
            ],
        };
        analysis
    }

    #[test]
    fn tests_are_grouped_under_their_package_and_target() {
        let analysis = with_tests(sample());
        let collapsed = test_rows(&analysis, &Filter::default(), &[]);
        assert_eq!(
            collapsed,
            vec![
                TestRow::Package(0),
                TestRow::Target {
                    command: 0,
                    expanded: false
                },
                TestRow::Target {
                    command: 1,
                    expanded: false
                },
            ]
        );

        let expanded = test_rows(&analysis, &Filter::default(), &[0]);
        assert_eq!(
            expanded,
            vec![
                TestRow::Package(0),
                TestRow::Target {
                    command: 0,
                    expanded: true
                },
                TestRow::Command(0),
                TestRow::Test(0),
                TestRow::Test(1),
                TestRow::Target {
                    command: 1,
                    expanded: false
                },
            ]
        );
    }

    #[test]
    fn each_package_gets_one_header() {
        let mut analysis = with_tests(sample());
        analysis.tests.commands.push(TestCommand {
            package: "other".to_owned(),
            target_kind: TargetKind::Test,
            target_name: "smoke_http".to_owned(),
            filters: Vec::new(),
            tests: 1,
            total: 4,
        });
        let rows = test_rows(&analysis, &Filter::default(), &[]);
        assert_eq!(
            rows,
            vec![
                TestRow::Package(0),
                TestRow::Target {
                    command: 0,
                    expanded: false
                },
                TestRow::Target {
                    command: 1,
                    expanded: false
                },
                TestRow::Package(2),
                TestRow::Target {
                    command: 2,
                    expanded: false
                },
            ]
        );
    }

    #[test]
    fn a_package_shows_its_version_only_when_the_command_carries_one() {
        assert_eq!(package_parts("serde_json"), ("serde_json", None));
        assert_eq!(
            package_parts("serde_json@1.0.155"),
            ("serde_json", Some("1.0.155"))
        );
    }

    #[test]
    fn the_text_filter_hides_targets_without_a_matching_test() {
        let analysis = with_tests(sample());
        let filter = Filter {
            text: "served".to_owned(),
            ..Filter::default()
        };
        assert_eq!(
            test_rows(&analysis, &filter, &[]),
            vec![
                TestRow::Package(1),
                TestRow::Target {
                    command: 1,
                    expanded: false
                }
            ]
        );
        let none = Filter {
            text: "zzz".to_owned(),
            ..Filter::default()
        };
        assert_eq!(
            test_rows(&analysis, &none, &[]),
            vec![TestRow::Note("No test matches the filter.".to_owned())]
        );
    }

    #[test]
    fn a_selection_without_tests_says_why() {
        let mut analysis = sample();
        assert_eq!(
            test_rows(&analysis, &Filter::default(), &[]),
            vec![TestRow::Note("This project has no tests.".to_owned())]
        );
        analysis.tests.total = 12;
        assert_eq!(
            test_rows(&analysis, &Filter::default(), &[]),
            vec![TestRow::Note(
                "No test reaches the changed code.".to_owned()
            )]
        );
    }

    #[test]
    fn covering_tests_are_those_whose_path_runs_through_the_symbol() {
        let mut analysis = with_tests(sample());
        let mut test_item = item(
            "k::tests::totals",
            ImpactClass::Behaviour,
            Confidence::Likely,
            false,
        );
        test_item.paths[0].steps = vec![
            step("k::total", "k::changed"),
            step("k::tests::totals", "k::total"),
        ];
        analysis.items.push(test_item);

        assert_eq!(covering_tests(&analysis, &symbol("k::total")), vec![0]);
        assert_eq!(
            covering_tests(&analysis, &symbol("k::tests::rounding")),
            vec![1]
        );
        assert!(covering_tests(&analysis, &symbol("k::nothing")).is_empty());
    }
}
