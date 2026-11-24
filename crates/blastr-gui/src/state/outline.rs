//! The rows of the left column: the changes, then their impact in two blocks,
//! and the tests in a list of their own.
//!
//! Rows are rebuilt from scratch whenever the result or the filter changes;
//! the list draws only the rows on screen.

use blastr_model::{
    Analysis, Change, ChangeKind, Confidence, DiffResult, FileStatus, ImpactClass, ImpactItem,
    RelPath, SelectedTest, SymbolPath, TestCommand,
};

/// One block of the outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    Changes,
    NeedsEditing,
    MayBehave,
}

/// One row of the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The header of a block, with its count once it is known.
    Head {
        block: Block,
        count: Option<usize>,
        collapsed: bool,
    },
    /// The divider between the changes and their impact.
    ImpactGroup,
    /// The file the changes under it are in.
    File {
        path: RelPath,
        status: Option<FileStatus>,
    },
    /// A change, by its index in the changes.
    Change(usize),
    /// An impact item, by its index in the items; `dim` when it was edited
    /// together with the change.
    Item { index: usize, dim: bool },
    /// The divider above the items that were edited with the change.
    Edited(usize),
    /// A sentence that stands in for an empty block.
    Note(String),
    /// A placeholder while the result is on its way.
    Skeleton,
    /// Air between blocks.
    Gap,
}

/// The height of each kind of row, in pixels.
pub mod height {
    pub const HEAD: f32 = 32.0;
    pub const GROUP_HEAD: f32 = 28.0;
    pub const FILE: f32 = 22.0;
    pub const CHANGE: f32 = 28.0;
    pub const ITEM: f32 = 38.0;
    pub const EDITED: f32 = 22.0;
    pub const SKELETON: f32 = 26.0;
    pub const GAP: f32 = 10.0;
}

impl Row {
    /// How tall the row is drawn.
    pub fn height(&self) -> f32 {
        match self {
            Row::Head { .. } => height::HEAD,
            Row::ImpactGroup => height::GROUP_HEAD,
            Row::File { .. } => height::FILE,
            Row::Change(_) => height::CHANGE,
            Row::Item { .. } => height::ITEM,
            Row::Edited(_) => height::EDITED,
            Row::Note(text) => note_height(text),
            Row::Skeleton => height::SKELETON,
            Row::Gap => height::GAP,
        }
    }

    /// The symbol a row selects.
    pub fn symbol<'a>(
        &self,
        diff: Option<&'a DiffResult>,
        analysis: Option<&'a Analysis>,
    ) -> Option<&'a SymbolPath> {
        match self {
            Row::Change(index) => diff?.changes.get(*index).map(|change| &change.symbol),
            Row::Item { index, .. } => analysis?.items.get(*index).map(|item| &item.symbol),
            _ => None,
        }
    }
}

/// The height of a note, which wraps over up to three lines.
fn note_height(text: &str) -> f32 {
    let lines = text.chars().count().div_ceil(38).clamp(1, 3);
    8.0 + 17.0 * lines as f32
}

/// What the outline lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Only symbols whose path contains this text, ignoring case.
    pub text: String,
    /// Items below this confidence are not listed.
    pub min_confidence: Confidence,
    /// The blocks that are folded.
    pub collapsed: Vec<Block>,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            text: String::new(),
            min_confidence: Confidence::Possible,
            collapsed: Vec::new(),
        }
    }
}

impl Filter {
    /// Whether `symbol` passes the text filter.
    pub fn matches(&self, symbol: &str) -> bool {
        let needle = self.text.trim();
        needle.is_empty() || symbol.to_lowercase().contains(&needle.to_lowercase())
    }

    /// Whether `item` is listed.
    pub fn lists(&self, item: &ImpactItem) -> bool {
        item.confidence >= self.min_confidence && self.matches(&item.symbol.path)
    }

    /// Folds or unfolds `block`.
    pub fn toggle(&mut self, block: Block) {
        if let Some(position) = self.collapsed.iter().position(|entry| *entry == block) {
            self.collapsed.remove(position);
        } else {
            self.collapsed.push(block);
        }
    }

    fn is_collapsed(&self, block: Block) -> bool {
        self.collapsed.contains(&block)
    }
}

/// The rows of the outline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outline {
    pub rows: Vec<Row>,
}

impl Outline {
    /// Builds the rows from the changes, the analysis when it has arrived,
    /// and the filter.
    pub fn build(
        diff: Option<&DiffResult>,
        analysis: Option<&Analysis>,
        loading: bool,
        filter: &Filter,
    ) -> Outline {
        let diff = analysis.map(|analysis| &analysis.diff).or(diff);
        let mut rows = Vec::new();
        let semantic = diff.map_or(0, |diff| {
            diff.changes
                .iter()
                .filter(|change| change.kind != ChangeKind::NonSemantic)
                .count()
        });
        let collapsed = filter.is_collapsed(Block::Changes);
        rows.push(Row::Head {
            block: Block::Changes,
            count: diff.map(|_| semantic),
            collapsed,
        });
        if !collapsed {
            match diff {
                Some(diff) => change_rows(diff, semantic, filter, &mut rows),
                None if loading => rows.extend([Row::Skeleton, Row::Skeleton]),
                None => {}
            }
        }
        rows.push(Row::Gap);
        rows.push(Row::ImpactGroup);
        for block in [Block::NeedsEditing, Block::MayBehave] {
            item_block(block, analysis, loading, filter, &mut rows);
        }
        Outline { rows }
    }

    /// The row that selects `symbol`.
    pub fn position(
        &self,
        symbol: &SymbolPath,
        diff: Option<&DiffResult>,
        analysis: Option<&Analysis>,
    ) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.symbol(diff, analysis) == Some(symbol))
    }

    /// The next selectable row after `from`, or before it.
    pub fn step(&self, from: Option<usize>, forward: bool) -> Option<usize> {
        let selectable =
            |(_, row): &(usize, &Row)| matches!(row, Row::Change(_) | Row::Item { .. });
        let mut rows = self.rows.iter().enumerate();
        match (from, forward) {
            (Some(from), true) => rows.skip(from + 1).find(selectable),
            (Some(from), false) => rows.take(from).rev().find(selectable),
            (None, true) => rows.find(selectable),
            (None, false) => rows.rev().find(selectable),
        }
        .map(|(index, _)| index)
    }
}

/// The changes, grouped under the file they are in.
fn change_rows(diff: &DiffResult, semantic: usize, filter: &Filter, rows: &mut Vec<Row>) {
    let mut listed = 0usize;
    let mut current: Option<&RelPath> = None;
    for (index, change) in diff.changes.iter().enumerate() {
        if change.kind == ChangeKind::NonSemantic || !filter.matches(&change.symbol.path) {
            continue;
        }
        let file = change_file(change);
        if file != current
            && let Some(path) = file
        {
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
        rows.push(Row::Change(index));
        listed += 1;
    }
    if listed > 0 {
        return;
    }
    let note = if semantic > 0 {
        "No change matches the filter.".to_owned()
    } else if diff.changes.is_empty() {
        "No structural changes in this comparison.".to_owned()
    } else {
        let files = diff.changed_files.len();
        let noun = if files == 1 {
            "file differs"
        } else {
            "files differ"
        };
        format!("No structural changes. {files} {noun} in formatting or comments only.")
    };
    rows.push(Row::Note(note));
}

/// The file a change is in.
fn change_file(change: &Change) -> Option<&RelPath> {
    change
        .new
        .as_ref()
        .or(change.old.as_ref())
        .map(|location| &location.file)
}

/// One block of impact items: the open ones, then those edited with the change.
fn item_block(
    block: Block,
    analysis: Option<&Analysis>,
    loading: bool,
    filter: &Filter,
    rows: &mut Vec<Row>,
) {
    let class = match block {
        Block::NeedsEditing => ImpactClass::Adaptation,
        _ => ImpactClass::Behaviour,
    };
    let open: Vec<usize> = analysis.map_or_else(Vec::new, |analysis| {
        indices(analysis, |item| item.class == class && !item.tags.changed)
    });
    let collapsed = filter.is_collapsed(block);
    rows.push(Row::Head {
        block,
        count: analysis.map(|_| open.len()),
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
    let listed: Vec<usize> = open
        .iter()
        .copied()
        .filter(|index| filter.lists(&analysis.items[*index]))
        .collect();
    let edited = if class == ImpactClass::Adaptation {
        indices(analysis, |item| {
            item.class == class && item.tags.changed && filter.lists(item)
        })
    } else {
        Vec::new()
    };
    if listed.is_empty() {
        let note = match block {
            _ if !open.is_empty() => "Nothing matches the filter.",
            Block::NeedsEditing => "Nothing left to edit.",
            _ => "Nothing else may behave differently.",
        };
        rows.push(Row::Note(note.to_owned()));
    }
    rows.extend(
        listed
            .into_iter()
            .map(|index| Row::Item { index, dim: false }),
    );
    if !edited.is_empty() {
        rows.push(Row::Edited(edited.len()));
        rows.extend(
            edited
                .into_iter()
                .map(|index| Row::Item { index, dim: true }),
        );
    }
    rows.push(Row::Gap);
}

/// The indices of the items `keep` accepts.
fn indices(analysis: &Analysis, keep: impl Fn(&ImpactItem) -> bool) -> Vec<usize> {
    analysis
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| keep(item))
        .map(|(index, _)| index)
        .collect()
}

/// One row of the tests list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestRow {
    /// A package, by the first of its commands.
    Package(usize),
    /// A test target, by its command.
    Target { command: usize, expanded: bool },
    /// The `cargo test` line of an expanded target.
    Command(usize),
    /// One selected test.
    Test(usize),
    /// A sentence that stands in for an empty list.
    Note(String),
}

impl TestRow {
    /// How tall the row is drawn.
    pub fn height(&self) -> f32 {
        match self {
            TestRow::Package(_) | TestRow::Test(_) => 22.0,
            TestRow::Target { .. } => 28.0,
            TestRow::Command(_) => 30.0,
            TestRow::Note(text) => note_height(text).max(26.0),
        }
    }
}

/// A package name and the version a command carries after `@`.
pub fn package_parts(package: &str) -> (&str, Option<&str>) {
    match package.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (package, None),
    }
}

/// The tests to run, under their packages and targets.
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

/// The command that runs `test`: the one of its target, else of its package.
pub fn test_target(test: &SelectedTest, commands: &[TestCommand]) -> Option<usize> {
    let krate = test.symbol.crate_name();
    let named = |name: &str| name.replace('-', "_") == krate;
    commands
        .iter()
        .position(|command| named(&command.target_name))
        .or_else(|| commands.iter().position(|command| named(&command.package)))
}

/// The selected tests whose evidence runs through `symbol`, or that are it.
pub fn covering_tests(analysis: &Analysis, symbol: &SymbolPath) -> Vec<usize> {
    analysis
        .tests
        .selected
        .iter()
        .enumerate()
        .filter(|(_, test)| {
            &test.symbol == symbol
                || analysis
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

    fn item(path: &str, class: ImpactClass, confidence: Confidence, changed: bool) -> ImpactItem {
        ImpactItem {
            symbol: symbol(path),
            location: location("src/lib.rs", 9),
            class,
            confidence,
            distance: 1,
            paths: vec![EvidencePath {
                change: 0,
                reason: Reason::CallsChanged,
                class,
                steps: vec![EvidenceStep {
                    from: symbol(path),
                    to: symbol("k::apply"),
                    edge: EdgeKind::Calls,
                    certainty: Certainty::Resolved,
                    detail: EdgeDetail::None,
                    site: location("src/lib.rs", 3),
                }],
            }],
            path_count: 1,
            tags: ImpactTags {
                changed,
                ..ImpactTags::default()
            },
        }
    }

    fn sample() -> Analysis {
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
                changes: vec![
                    change(ChangeKind::Signature, "k::apply", "src/a.rs"),
                    change(ChangeKind::Body, "k::preview", "src/a.rs"),
                    change(ChangeKind::NonSemantic, "k::fmt", "src/a.rs"),
                    change(ChangeKind::Body, "k::vat", "src/b.rs"),
                ],
                limits: Vec::new(),
            },
            items: vec![
                item(
                    "k::total",
                    ImpactClass::Adaptation,
                    Confidence::Definite,
                    false,
                ),
                item(
                    "k::edited",
                    ImpactClass::Adaptation,
                    Confidence::Definite,
                    true,
                ),
                item(
                    "k::summary",
                    ImpactClass::Behaviour,
                    Confidence::Likely,
                    false,
                ),
                item(
                    "k::dispatch",
                    ImpactClass::Behaviour,
                    Confidence::Possible,
                    false,
                ),
            ],
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        }
    }

    /// The rows in words, for comparing whole outlines.
    fn names(outline: &Outline, analysis: &Analysis) -> Vec<String> {
        outline
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Head { block, count, .. } => {
                    Some(format!("[{block:?} {}]", count.unwrap_or(0)))
                }
                Row::File { path, .. } => Some(format!("file {path}")),
                Row::Change(index) => Some(format!(
                    "change {}",
                    analysis.diff.changes[*index].symbol.name()
                )),
                Row::Item { index, dim } => Some(format!(
                    "{} {}",
                    if *dim { "edited" } else { "item" },
                    analysis.items[*index].symbol.name()
                )),
                Row::Note(text) => Some(format!("note {text}")),
                _ => None,
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
                "[NeedsEditing 1]",
                "item total",
                "edited edited",
                "[MayBehave 2]",
                "item summary",
                "item dispatch",
            ]
        );
    }

    #[test]
    fn placeholders_show_only_while_loading() {
        let loading = Outline::build(None, None, true, &Filter::default());
        assert!(loading.rows.contains(&Row::Skeleton));
        let idle = Outline::build(None, None, false, &Filter::default());
        assert!(!idle.rows.contains(&Row::Skeleton));
    }

    #[test]
    fn the_filters_hide_rows_but_keep_the_counts() {
        let analysis = sample();
        let filter = Filter {
            text: "SUM".to_owned(),
            min_confidence: Confidence::Likely,
            collapsed: vec![Block::NeedsEditing],
        };
        let outline = Outline::build(None, Some(&analysis), false, &filter);
        assert_eq!(
            names(&outline, &analysis),
            vec![
                "[Changes 3]",
                "note No change matches the filter.",
                "[NeedsEditing 1]",
                "[MayBehave 2]",
                "item summary",
            ]
        );
    }

    #[test]
    fn stepping_moves_between_selectable_rows_only() {
        let analysis = sample();
        let outline = Outline::build(None, Some(&analysis), false, &Filter::default());
        let first = outline.step(None, true).expect("a row");
        assert_eq!(outline.rows[first], Row::Change(0));
        let second = outline.step(Some(first), true).expect("a row");
        assert_eq!(outline.rows[second], Row::Change(1));
        assert_eq!(outline.step(Some(second), false), Some(first));
        assert_eq!(outline.step(Some(first), false), None);
        let last = outline.step(None, false).expect("a row");
        assert_eq!(outline.step(Some(last), true), None);
        let position = outline
            .position(&symbol("k::summary"), None, Some(&analysis))
            .expect("listed");
        assert_eq!(
            outline.rows[position],
            Row::Item {
                index: 2,
                dim: false
            }
        );
    }

    #[test]
    fn tests_are_grouped_under_their_package_and_target() {
        let mut analysis = sample();
        let test = |path: &str| SelectedTest {
            symbol: symbol(path),
            location: location("tests/a.rs", 1),
            reason: TestReason::Reached,
            filter: path.to_owned(),
        };
        let command = |name: &str, kind| TestCommand {
            package: "k@1.0.0".to_owned(),
            target_kind: kind,
            target_name: name.to_owned(),
            filters: Vec::new(),
            tests: 1,
            total: 3,
        };
        analysis.tests = TestSelection {
            selected: vec![test("k::tests::one"), test("smoke::two")],
            total: 6,
            commands: vec![
                command("k", TargetKind::Lib),
                command("smoke", TargetKind::Test),
            ],
        };
        let rows = test_rows(&analysis, &Filter::default(), &[1]);
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
                    expanded: true
                },
                TestRow::Command(1),
                TestRow::Test(1),
            ]
        );
        assert_eq!(package_parts("k@1.0.0"), ("k", Some("1.0.0")));
        let none = test_rows(&sample(), &Filter::default(), &[]);
        assert_eq!(
            none,
            vec![TestRow::Note("This project has no tests.".to_owned())]
        );
    }
}
