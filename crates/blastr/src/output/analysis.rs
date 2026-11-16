//! The human-readable report of an impact analysis.
//!
//! The functions here turn an [`Analysis`] into the text `blastr analyze`,
//! `blastr tests` and `blastr explain` print. Nothing is written to a
//! terminal: colour, line width and how much is listed are decided by the
//! [`Style`] the caller passes in, so the same result always renders the same
//! text.

use blastr_model::{
    Analysis, AnalysisOptions, Certainty, Change, ChangeDetail, ChangeKind, Confidence, Delta,
    EdgeKind, EvidencePath, EvidenceStep, ImpactClass, ImpactItem, ImpactTags, Limit, Location,
    Reason, SelectedTest, TargetKind, TestCommand, TestReason, TestSelection,
};

/// How much a report shows and how it is decorated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    /// Emit ANSI escape codes around the class and confidence of an item.
    pub colour: bool,
    /// Widest line to aim for, in characters; 0 means no limit.
    pub width: usize,
    /// List every affected item instead of the first few.
    pub list_all: bool,
}

/// Heading of the items that have to be edited.
const ADAPTATION_HEADING: &str = "NEEDS EDITING";

/// Heading of the adaptations the change already made itself.
const EDITED_HEADING: &str = "EDITED WITH THE CHANGE";

/// Heading of the items whose behaviour may differ.
const BEHAVIOUR_HEADING: &str = "MAY BEHAVE DIFFERENTLY";

/// Heading of the selected tests in the report of `blastr tests`.
const TESTS_HEADING: &str = "Tests";

/// Most items one section lists unless everything is asked for.
const MAX_ITEMS: usize = 10;

/// Most selected tests `blastr tests` lists unless everything is asked for.
const MAX_TESTS: usize = 30;

/// Narrowest the reason column may become when the line width forces a cut.
const MIN_REASON_WIDTH: usize = 8;

/// What the report points at when a reader wants tests or commands.
const TESTS_HINT: &str = "Run `blastr tests` to see their names, or `blastr tests --format cargo` \
                          for the commands.";

/// What a report says where the command lines are a format away.
pub const COMMANDS_HINT: &str = "Run `blastr tests --format cargo` for the commands.";

/// What the analysis says when nothing besides the changes is affected.
const NOTHING_ELSE: &str = "Nothing else is affected.";

/// What the tests section says when no test reaches the changes.
const NO_REACHING_TEST: &str = "no test reaches the changed code";

/// What `blastr tests` says when the project has no tests at all.
const NO_TESTS: &str = "this project has no tests";

/// What an explain report says about an uncertain path.
const EXPLAIN_LIMIT: &str = "Limits: this path goes through a step that could not be resolved \
                             to one target, so the item may not be affected.";

/// Bold, used for section headings.
const BOLD: &str = "1";

/// Red, used for adaptation items and definite behaviour.
const RED: &str = "31";

/// Yellow, used for likely behaviour.
const YELLOW: &str = "33";

/// Dim, used for possible behaviour.
const DIM: &str = "2";

/// Ends an escape sequence.
const RESET: &str = "0";

/// Renders the whole analysis as the human-readable report.
///
/// The caller decides the features shown in the header through `options`, and
/// the amount of colour, width and detail through `style`.
pub fn render_analysis(analysis: &Analysis, options: &AnalysisOptions, style: &Style) -> String {
    let mut blocks: Vec<String> = Vec::new();

    let comparison = &analysis.diff.comparison;
    let features = super::features_text(options);
    if comparison.base == comparison.head {
        // A what-if query: nothing is compared, one change is assumed.
        blocks.push(format!(
            "Assuming a change in the {} · features: {features}",
            comparison.head
        ));
    } else {
        blocks.push(format!(
            "Comparing {} → {} · features: {features}",
            comparison.base, comparison.head
        ));
    }

    let listed: Vec<&Change> = analysis
        .diff
        .changes
        .iter()
        .filter(|change| change.kind != ChangeKind::NonSemantic)
        .collect();
    blocks.push(super::changes_block(&listed, style.list_all));
    let hidden = analysis.diff.changes.len() - listed.len();
    if hidden > 0 {
        let noun = if hidden == 1 { "change" } else { "changes" };
        blocks.push(format!("{hidden} formatting-only {noun} not shown"));
    }

    // Adaptations the diff left alone are work still to do; the ones it edited
    // itself are only counted, or listed when everything is asked for.
    let adaptations: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| item.class == ImpactClass::Adaptation && !item.tags.changed)
        .collect();
    let edited: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| item.class == ImpactClass::Adaptation && item.tags.changed)
        .collect();
    let behaviours: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| item.class == ImpactClass::Behaviour && !item.tags.changed)
        .collect();

    let nothing_listed = adaptations.is_empty() && edited.is_empty() && behaviours.is_empty();
    if nothing_listed && !listed.is_empty() {
        blocks.push(NOTHING_ELSE.to_owned());
    }

    let mut listed_items = 0usize;
    let mut cut = false;
    if !adaptations.is_empty() {
        let rows = adaptation_rows(&adaptations, analysis, style);
        cut |= rows.len() < adaptations.len();
        listed_items += rows.len();
        blocks.push(detail_section(
            ADAPTATION_HEADING,
            rows,
            adaptations.len(),
            style,
        ));
    }
    if !edited.is_empty() {
        if style.list_all {
            let rows = adaptation_rows(&edited, analysis, style);
            listed_items += rows.len();
            blocks.push(detail_section(EDITED_HEADING, rows, edited.len(), style));
        } else {
            blocks.push(edited_summary(edited.len()));
        }
    }
    if !behaviours.is_empty() {
        let rows = behaviour_rows(&behaviours, analysis, style);
        cut |= rows.len() < behaviours.len();
        listed_items += rows.len();
        blocks.push(detail_section(
            BEHAVIOUR_HEADING,
            rows,
            behaviours.len(),
            style,
        ));
    }

    if let Some(section) = tests_section(analysis, style) {
        blocks.push(section);
    }
    if let Some(text) = limits_text(analysis) {
        let heading = paint("LIMITS", BOLD, style);
        blocks.push(format!("{heading}  {text}"));
    }
    if listed_items > 0 {
        blocks.push(hint_text(cut));
    }

    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

/// Renders the tests of an analysis: the targets they are in, and their names.
pub fn render_tests(analysis: &Analysis, style: &Style) -> String {
    let selection = &analysis.tests;
    let selected = selection.selected.len();
    let total = selection.total;

    let mut out = paint(
        &format!("TESTS TO RUN ({selected} of {total})"),
        BOLD,
        style,
    );
    if selected == 0 {
        out.push_str("\n  ");
        out.push_str(if total == 0 {
            NO_TESTS
        } else {
            NO_REACHING_TEST
        });
        out.push('\n');
        return out;
    }

    for line in target_lines(selection) {
        out.push('\n');
        out.push_str(&line);
    }

    let shown = if style.list_all {
        selected
    } else {
        selected.min(MAX_TESTS)
    };
    let listed: Vec<&SelectedTest> = selection.selected.iter().take(shown).collect();
    let symbol_width = listed
        .iter()
        .map(|test| super::shorten(&test.symbol.path).chars().count())
        .max()
        .unwrap_or(0);
    let reason_width = listed
        .iter()
        .map(|test| test_reason_text(test.reason).chars().count())
        .max()
        .unwrap_or(0);

    out.push_str("\n\n");
    out.push_str(TESTS_HEADING);
    for test in listed {
        let mut line = String::from("\n  ");
        line.push_str(&pad(&super::shorten(&test.symbol.path), symbol_width));
        line.push_str("  ");
        line.push_str(&pad(test_reason_text(test.reason), reason_width));
        line.push_str("  ");
        line.push_str(&site_text(&test.location));
        out.push_str(line.trim_end());
    }
    if shown < selected {
        out.push_str(&format!("\n  … and {} more (use --all)", selected - shown));
    }

    out.push('\n');
    out.push_str(COMMANDS_HINT);
    out.push('\n');
    out
}

/// Renders why one item is affected, or a message saying it is not.
///
/// `Err` holds the message for the user, without a trailing newline.
pub fn render_explain(analysis: &Analysis, query: &str, style: &Style) -> Result<String, String> {
    let item = find_item(analysis, query)?;

    let mut blocks: Vec<String> = Vec::new();
    blocks.push(explain_header(item, style));
    for (index, path) in item.paths.iter().enumerate() {
        blocks.push(path_block(item, path, index + 1, analysis));
    }
    let more = (item.path_count as usize).saturating_sub(item.paths.len());
    if more > 0 {
        let noun = if more == 1 { "path is" } else { "paths are" };
        blocks.push(format!("{more} more {noun} not shown."));
    }
    if best_path(item).is_some_and(|path| {
        path.steps
            .iter()
            .any(|step| step.certainty == Certainty::Candidate)
    }) {
        blocks.push(EXPLAIN_LIMIT.to_owned());
    }

    let mut out = blocks.join("\n\n");
    out.push('\n');
    Ok(out)
}

/// One short phrase saying why a change reaches an item.
///
/// The changed thing is named by the last segment of its path.
pub fn reason_text(reason: Reason, change: &Change) -> String {
    let name = change.symbol.name();
    match reason {
        Reason::CallsChanged => format!("calls {name}"),
        Reason::TakesChangedAsValue => format!("passes {name} as a value"),
        Reason::DispatchesToChanged => format!("may dispatch to {name}"),
        Reason::SignatureChanged => format!("calls {name}, whose signature changed"),
        Reason::DeclarationChanged => format!("uses {name}, whose declaration changed"),
        Reason::NoLongerVisible => format!("can no longer see {name}"),
        Reason::RefersToRemoved => format!("uses removed {name}"),
        Reason::RefersToRenamed => match &change.detail {
            ChangeDetail::Renamed { from } => format!("uses {}, now {name}", from.name()),
            _ => format!("uses {name} by its old name"),
        },
        Reason::UsesChangedField => format!("uses field {name}"),
        Reason::ConstructsWithoutField => format!("builds the struct without {name}"),
        Reason::PatternMissesField => format!("pattern does not name {name}"),
        Reason::ExhaustiveMatchMissesVariant => format!("match without a `_` arm misses {name}"),
        Reason::WildcardCatchesNewVariant => format!("`_` arm now also catches {name}"),
        Reason::UsesChangedVariant => format!("uses variant {name}"),
        Reason::MustImplementTraitItem => format!("must implement {name}"),
        Reason::UsesChangedTraitItem => format!("uses trait item {name}"),
        Reason::InheritsChangedDefault => format!("inherits the default {name}"),
        Reason::ImplResolutionChanged => match &change.detail {
            ChangeDetail::Member(Delta::Added) => {
                "calls a method of an impl that was added".to_owned()
            }
            ChangeDetail::Member(Delta::Removed) => {
                "calls a method of an impl that was removed".to_owned()
            }
            _ => "calls a method of an impl that changed".to_owned(),
        },
        Reason::UsesChangedValue => format!("uses {name}"),
        Reason::UsesRetypedValue => format!("uses {name}, whose type changed"),
        Reason::UsesChangedAlias => format!("uses alias {name}"),
        Reason::InvokesChangedMacro => format!("invokes {name}!"),
        Reason::AttributeChanged => format!("calls {name}, whose attributes changed"),
    }
}

/// Why an item is listed, in one short phrase.
///
/// A path of one step spells out the rule that reached the item; a longer
/// path says how far it is and through what kind of indirection it runs.
pub fn path_summary(item: &ImpactItem, analysis: &Analysis) -> String {
    let Some(path) = best_path(item) else {
        return String::new();
    };
    if path.steps.len() <= 1 {
        let Some(change) = analysis.diff.changes.get(path.change as usize) else {
            return String::new();
        };
        return reason_text(path.reason, change);
    }

    let start = match analysis.diff.changes.get(path.change as usize) {
        Some(change) => change.symbol.name(),
        None => path.steps.first().map_or("", |step| step.to.name()),
    };
    let mut text = format!("{} steps from {start}", path.steps.len());
    if path
        .steps
        .iter()
        .any(|step| step.edge == EdgeKind::MayDispatchTo)
    {
        text.push_str(" through a trait object");
    } else if path
        .steps
        .iter()
        .any(|step| step.edge == EdgeKind::TakesAsValue)
    {
        text.push_str(" through a function pointer");
    }
    text
}

/// The path that gives an item its class: the best path of that class.
fn best_path(item: &ImpactItem) -> Option<&EvidencePath> {
    item.paths
        .iter()
        .find(|path| path.class == item.class)
        .or_else(|| item.paths.first())
}

/// How many items a section lists.
fn shown_count(total: usize, style: &Style) -> usize {
    if style.list_all {
        total
    } else {
        total.min(MAX_ITEMS)
    }
}

/// One line of a section: aligned cells of which the last is the location.
struct Row {
    /// The cells, without colour codes.
    cells: Vec<String>,
    /// Colour of the first cell.
    colour: &'static str,
}

/// The rows of the adaptation section.
fn adaptation_rows(items: &[&ImpactItem], analysis: &Analysis, style: &Style) -> Vec<Row> {
    let shown = shown_count(items.len(), style);
    items
        .iter()
        .take(shown)
        .map(|item| Row {
            cells: vec![
                super::shorten(&item.symbol.path),
                path_summary(item, analysis),
                item_site(item),
            ],
            colour: RED,
        })
        .collect()
}

/// The rows of the behaviour section.
fn behaviour_rows(items: &[&ImpactItem], analysis: &Analysis, style: &Style) -> Vec<Row> {
    let shown = shown_count(items.len(), style);
    items
        .iter()
        .take(shown)
        .map(|item| {
            let mut reason = path_summary(item, analysis);
            let tags = section_tags(&item.tags);
            if !tags.is_empty() {
                reason.push(' ');
                reason.push_str(&tags);
            }
            Row {
                cells: vec![
                    item.confidence.to_string(),
                    super::shorten(&item.symbol.path),
                    reason,
                    item_site(item),
                ],
                colour: confidence_colour(item.confidence),
            }
        })
        .collect()
}

/// Renders a section heading and its aligned rows.
fn detail_section(heading: &str, rows: Vec<Row>, total: usize, style: &Style) -> String {
    let shown = rows.len();
    let title = if shown < total {
        format!("{heading} ({total}, showing {shown})")
    } else {
        format!("{heading} ({total})")
    };
    let mut out = paint(&title, BOLD, style);
    if rows.is_empty() {
        return out;
    }

    let columns = rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or_default();
    let mut widths = vec![0usize; columns];
    for row in &rows {
        for (index, cell) in row.cells.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    // Make room by shortening the reason column; the location never shrinks.
    let reason = columns.saturating_sub(2);
    if columns >= 3 && style.width > 0 {
        let longest = rows
            .iter()
            .map(|row| line_width(row, &widths))
            .max()
            .unwrap_or_default();
        let excess = longest.saturating_sub(style.width);
        if excess > 0 {
            widths[reason] = widths[reason].saturating_sub(excess).max(MIN_REASON_WIDTH);
        }
    }

    for row in &rows {
        let last = row.cells.len().saturating_sub(1);
        let mut line = String::from("  ");
        for (index, cell) in row.cells.iter().enumerate() {
            if index > 0 {
                line.push_str("  ");
            }
            let width = widths.get(index).copied().unwrap_or_default();
            let text = if columns >= 3 && index == reason {
                truncate(cell, width)
            } else {
                cell.clone()
            };
            if index == last {
                line.push_str(&text);
            } else if index == 0 {
                line.push_str(&paint(&text, row.colour, style));
                line.push_str(&spaces(width.saturating_sub(text.chars().count())));
            } else {
                line.push_str(&pad(&text, width));
            }
        }
        out.push('\n');
        out.push_str(line.trim_end());
    }
    out
}

/// Length of a row once every column is padded to its width.
fn line_width(row: &Row, widths: &[usize]) -> usize {
    let last = row.cells.len().saturating_sub(1);
    let mut width = 2;
    for (index, cell) in row.cells.iter().enumerate() {
        if index > 0 {
            width += 2;
        }
        width += if index == last {
            cell.chars().count()
        } else {
            widths.get(index).copied().unwrap_or_default()
        };
    }
    width
}

/// The tests section of the report: one aligned line per target, no commands.
fn tests_section(analysis: &Analysis, style: &Style) -> Option<String> {
    let selection = &analysis.tests;
    let selected = selection.selected.len();
    let total = selection.total;
    if selected == 0 {
        if total == 0 {
            return None;
        }
        let heading = paint(&format!("TESTS TO RUN (0 of {total})"), BOLD, style);
        return Some(format!("{heading}\n  {NO_REACHING_TEST}"));
    }

    let mut lines = vec![paint(
        &format!("TESTS TO RUN ({selected} of {total})"),
        BOLD,
        style,
    )];
    lines.extend(target_lines(selection));
    lines.push(TESTS_HINT.to_owned());
    Some(lines.join("\n"))
}

/// How a target reads in a tests listing.
///
/// A library holds its unit tests; the other targets are named by the file or
/// the binary the tests live in.
pub fn target_label(command: &TestCommand) -> String {
    let name = &command.target_name;
    match command.target_kind {
        TargetKind::Lib | TargetKind::ProcMacro => "unit tests".to_owned(),
        TargetKind::Bin => format!("bin {name}"),
        TargetKind::Test => format!("tests/{name}.rs"),
        TargetKind::Example => format!("examples/{name}.rs"),
        TargetKind::Bench => format!("benches/{name}.rs"),
        TargetKind::BuildScript | TargetKind::Other => name.clone(),
    }
}

/// The target lines of a tests listing: the package and target, then how many
/// of the target's tests are selected.
fn target_lines(selection: &TestSelection) -> Vec<String> {
    let rows: Vec<(String, u32, u32)> = selection
        .commands
        .iter()
        .map(|command| {
            let label = format!("{} · {}", command.package, target_label(command));
            (label, command.tests, command.total)
        })
        .collect();
    let label_width = rows
        .iter()
        .map(|(label, _, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let count_width = rows
        .iter()
        .map(|(_, tests, _)| tests.to_string().chars().count())
        .max()
        .unwrap_or(0);

    rows.iter()
        .map(|(label, tests, total)| {
            let mut line = String::from("  ");
            line.push_str(&pad(label, label_width));
            line.push_str("  ");
            line.push_str(&right_aligned(&tests.to_string(), count_width));
            line.push_str(&format!(" of {total}"));
            line
        })
        .collect()
}

/// One sentence saying how many adaptations the change already made itself.
pub fn edited_summary(count: usize) -> String {
    if count == 1 {
        "1 dependant was edited together with the change (--all lists it).".to_owned()
    } else {
        format!("{count} dependants were edited together with the change (--all lists them).")
    }
}

/// The limits line of the report, if anything is worth saying.
fn limits_text(analysis: &Analysis) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    let inactive: Vec<&Limit> = analysis
        .diff
        .limits
        .iter()
        .filter(|limit| matches!(limit, Limit::ChangedButInactive(_)))
        .collect();
    for limit in &analysis.diff.limits {
        match limit {
            Limit::Unresolved { .. } => {}
            // Several of these are counted on one entry instead of listed.
            Limit::ChangedButInactive(_) if inactive.len() > 1 => {}
            other => parts.push(super::limit_text(other)),
        }
    }
    if inactive.len() > 1 {
        let count = inactive.len();
        parts.push(format!("{count} changes in inactive code"));
    }

    let limits = analysis.limits;
    if limits.unresolved_total > 0 {
        let total = limits.unresolved_total;
        let near = limits.unresolved_in_reach;
        let noun = count_noun(total, "reference", "references");
        let mut text = format!("{total} unresolved {noun}");
        if near > 0 {
            text.push_str(&format!(" ({near} near the changes)"));
        }
        parts.push(text);
    }
    if limits.candidate_steps > 0 {
        let count = limits.candidate_steps;
        let noun = count_noun(count, "candidate step", "candidate steps");
        parts.push(format!("{count} {noun}"));
    }
    if limits.coarse_rules > 0 {
        let count = limits.coarse_rules;
        let noun = count_noun(count, "coarse rule", "coarse rules");
        parts.push(format!("{count} {noun}"));
    }
    if limits.beyond_depth > 0 {
        let count = limits.beyond_depth;
        let depth = analysis.params.depth;
        let noun = count_noun(count, "item", "items");
        parts.push(format!("{count} {noun} beyond depth {depth}"));
    }
    if limits.below_floor > 0 {
        let count = limits.below_floor;
        let noun = count_noun(count, "item", "items");
        parts.push(format!("{count} {noun} below the score floor"));
    }
    if limits.below_confidence > 0 {
        let count = limits.below_confidence;
        let level = analysis.params.min_confidence;
        let noun = count_noun(count, "item", "items");
        parts.push(format!("{count} {noun} below {level} confidence"));
    }
    match limits.ignored {
        0 => {}
        1 => parts.push("1 item in an ignored file".to_owned()),
        count => parts.push(format!("{count} items in ignored files")),
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// The closing hint of the report.
fn hint_text(cut: bool) -> String {
    let hint = "Run `blastr explain <symbol>` to see why an item is listed";
    if cut {
        format!("{hint}, or add --all for the full list.")
    } else {
        format!("{hint}.")
    }
}

/// The header block of an explain report.
fn explain_header(item: &ImpactItem, style: &Style) -> String {
    let class = match item.class {
        ImpactClass::Adaptation => "needs editing",
        ImpactClass::Behaviour => "may behave differently",
    };
    let confidence = paint(
        item.confidence.as_str(),
        confidence_colour(item.confidence),
        style,
    );
    let steps = if item.distance == 1 { "step" } else { "steps" };
    let score = item.score;
    let distance = item.distance;

    let mut out = item.symbol.path.clone();
    out.push_str("\n  ");
    out.push_str(class);
    out.push_str(" · ");
    out.push_str(&confidence);
    out.push_str(" · ");
    out.push_str(&format!("score {score:.2}"));
    out.push_str(" · ");
    out.push_str(&format!("{distance} {steps}"));
    out.push_str("\n  ");
    out.push_str(&site_text(&item.location));
    let tags = tag_names(&item.tags);
    if !tags.is_empty() {
        out.push_str("\n  tags: ");
        out.push_str(&tags.join(", "));
    }
    out
}

/// One line of an explain report: a symbol, why it is reached, and where.
struct StepRow {
    /// The symbol the step starts at, or the changed symbol for the first row.
    symbol: String,
    /// Why the step reaches it.
    phrase: String,
    /// `file:line` of the reference, empty when unknown.
    location: String,
}

/// One evidence path of an explain report, one line per step.
fn path_block(item: &ImpactItem, path: &EvidencePath, index: usize, analysis: &Analysis) -> String {
    let count = item.paths.len();
    let origin = origin_text(analysis, path);
    let mut header = format!("Path {index} of {count} · {origin}");

    let change = analysis.diff.changes.get(path.change as usize);
    let mut rows: Vec<StepRow> = Vec::new();
    match change {
        Some(change) => {
            let kind = change.kind;
            rows.push(StepRow {
                symbol: change.symbol.path.clone(),
                phrase: format!("changed ({kind})"),
                location: super::location_text(change),
            });
        }
        None => {
            let symbol = path
                .steps
                .first()
                .map_or(String::new(), |step| step.to.path.clone());
            rows.push(StepRow {
                symbol,
                phrase: "changed".to_owned(),
                location: String::new(),
            });
        }
    }
    for (position, step) in path.steps.iter().enumerate() {
        rows.push(StepRow {
            symbol: step.from.path.clone(),
            phrase: step_text(step, path, analysis, position == 0),
            location: site_text(&step.site),
        });
    }

    let symbol_width = rows
        .iter()
        .map(|row| row.symbol.chars().count())
        .max()
        .unwrap_or_default();
    let phrase_width = rows
        .iter()
        .map(|row| row.phrase.chars().count())
        .max()
        .unwrap_or_default();

    for row in &rows {
        let mut line = String::from("  ");
        line.push_str(&pad(&row.symbol, symbol_width));
        line.push_str("  ");
        line.push_str(&pad(&row.phrase, phrase_width));
        if !row.location.is_empty() {
            line.push_str("  ");
            line.push_str(&row.location);
        }
        header.push('\n');
        header.push_str(line.trim_end());
    }
    header
}

/// The sentence naming the change a path starts at.
fn origin_text(analysis: &Analysis, path: &EvidencePath) -> String {
    match analysis.diff.changes.get(path.change as usize) {
        Some(change) => format!(
            "starts at the {} change of {}",
            change.kind, change.symbol.path
        ),
        None => {
            let symbol = path.steps.first().map_or("", |step| step.to.path.as_str());
            format!("starts at a change of {symbol}")
        }
    }
}

/// The middle cell of one step line.
fn step_text(step: &EvidenceStep, path: &EvidencePath, analysis: &Analysis, first: bool) -> String {
    let change = analysis.diff.changes.get(path.change as usize);
    let mut text = match change {
        Some(change) if first => reason_text(path.reason, change),
        _ => format!("{} {}", edge_phrase(step.edge), step.to.name()),
    };
    if step.certainty == Certainty::Candidate {
        text.push_str(" (candidate)");
    }
    text
}

/// How an edge kind reads in a step line.
fn edge_phrase(edge: EdgeKind) -> &'static str {
    match edge {
        EdgeKind::Calls => "calls",
        EdgeKind::TakesAsValue => "passes as a value",
        EdgeKind::UsesType => "uses type",
        EdgeKind::ReadsField => "reads",
        EdgeKind::WritesField => "writes",
        EdgeKind::Constructs => "constructs",
        EdgeKind::Matches => "matches on",
        EdgeKind::Implements => "implements",
        EdgeKind::MayDispatchTo => "may dispatch to",
        EdgeKind::UsesValue => "uses",
        EdgeKind::Contains => "contains",
        EdgeKind::Expands => "invokes",
    }
}

/// The item the query names, or the message saying why there is none.
fn find_item<'a>(analysis: &'a Analysis, query: &str) -> Result<&'a ImpactItem, String> {
    if let Some(item) = analysis.items.iter().find(|item| item.symbol.path == query) {
        return Ok(item);
    }

    let suffix = format!("::{query}");
    let matches: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| item.symbol.path.ends_with(&suffix))
        .collect();
    if matches.is_empty() {
        return Err(not_an_item(analysis, query));
    }
    if matches.len() == 1 {
        return Ok(matches[0]);
    }

    let mut paths: Vec<&str> = matches
        .iter()
        .map(|item| item.symbol.path.as_str())
        .collect();
    paths.sort_unstable();
    Err(format!(
        "`{query}` matches several items: {}",
        paths.join(", ")
    ))
}

/// The message for a query that names something other than an item.
fn not_an_item(analysis: &Analysis, query: &str) -> String {
    let suffix = format!("::{query}");
    let change = analysis
        .diff
        .changes
        .iter()
        .find(|change| change.symbol.path == query)
        .or_else(|| {
            analysis
                .diff
                .changes
                .iter()
                .find(|change| change.symbol.path.ends_with(&suffix))
        });
    match change {
        Some(change) => format!(
            "`{}` is one of the changes ({}); it is where the impact starts, not an affected item",
            change.symbol.path, change.kind
        ),
        None => format!("`{query}` is not affected by these changes"),
    }
}

/// The tags of an item as words, in a fixed order.
fn tag_names(tags: &ImpactTags) -> Vec<&'static str> {
    let mut names = Vec::new();
    if tags.is_test {
        names.push("test");
    }
    if tags.is_main {
        names.push("main");
    }
    if tags.is_pub_api {
        names.push("public API");
    }
    names
}

/// The tags of an item as the bracketed suffixes of a section row.
fn section_tags(tags: &ImpactTags) -> String {
    let names: Vec<String> = tag_names(tags)
        .iter()
        .map(|name| format!("[{name}]"))
        .collect();
    names.join(" ")
}

/// The colour of a confidence level.
fn confidence_colour(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Definite => RED,
        Confidence::Likely => YELLOW,
        Confidence::Possible => DIM,
    }
}

/// How a selected test's reason reads.
fn test_reason_text(reason: TestReason) -> &'static str {
    match reason {
        TestReason::Changed => "changed",
        TestReason::Reached => "reached",
    }
}

/// `file:line` of where an item is.
fn item_site(item: &ImpactItem) -> String {
    best_path(item)
        .and_then(|path| path.steps.last())
        .map_or(String::new(), |step| site_text(&step.site))
}

/// `file:line` of a location.
fn site_text(location: &Location) -> String {
    let file = &location.file;
    let line = location.span.start_line;
    format!("{file}:{line}")
}

/// The singular or plural form of a counted noun.
fn count_noun(count: u32, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

/// Wraps text in an ANSI escape sequence when colour is wanted.
fn paint(text: &str, code: &str, style: &Style) -> String {
    if style.colour {
        format!("\x1b[{code}m{text}\x1b[{RESET}m")
    } else {
        text.to_owned()
    }
}

/// Pads text with spaces to `width` characters.
fn pad(text: &str, width: usize) -> String {
    let mut out = String::from(text);
    out.push_str(&spaces(width.saturating_sub(text.chars().count())));
    out
}

/// Puts text at the end of a field of `width` characters.
fn right_aligned(text: &str, width: usize) -> String {
    let mut out = spaces(width.saturating_sub(text.chars().count()));
    out.push_str(text);
    out
}

/// A run of `count` spaces.
fn spaces(count: usize) -> String {
    std::iter::repeat_n(' ', count).collect()
}

/// Shortens text to `width` characters, marking the cut with an ellipsis.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        Analysis, AnalysisOptions, AnalysisParams, Certainty, Change, ChangeDetail, ChangeKind,
        Comparison, Confidence, Delta, DiffResult, EdgeDetail, EdgeKind, EvidencePath,
        EvidenceStep, ImpactClass, ImpactItem, ImpactLimits, ImpactTags, Limit, Location, Reason,
        RelPath, SelectedTest, Source, Span, SymbolKind, SymbolPath, TargetKind, TestCommand,
        TestReason, TestSelection,
    };

    use super::*;

    /// A location in `file` on the given line.
    fn location(file: &str, line: u32) -> Location {
        Location {
            file: RelPath::new(file),
            span: Span {
                start: 0,
                end: 10,
                start_line: line,
                end_line: line,
            },
        }
    }

    /// A symbol path of the given text.
    fn symbol(path: &str) -> SymbolPath {
        SymbolPath::new(path, SymbolKind::Function)
    }

    /// A change of `kind` to the symbol at `path`.
    fn change(kind: ChangeKind, path: &str, file: &str, line: u32) -> Change {
        Change {
            kind,
            symbol: symbol(path),
            old: None,
            new: Some(location(file, line)),
            detail: ChangeDetail::None,
        }
    }

    /// A change to a member of a type.
    fn member_change(kind: ChangeKind, path: &str, file: &str, line: u32, delta: Delta) -> Change {
        Change {
            detail: ChangeDetail::Member(delta),
            ..change(kind, path, file, line)
        }
    }

    /// One reference from `from` to `to`.
    fn step(from: &str, to: &str, edge: EdgeKind, file: &str, line: u32) -> EvidenceStep {
        EvidenceStep {
            from: symbol(from),
            to: symbol(to),
            edge,
            certainty: Certainty::Resolved,
            detail: EdgeDetail::None,
            site: location(file, line),
        }
    }

    /// One evidence path starting at the change with index `change`.
    fn evidence(
        change: u32,
        reason: Reason,
        class: ImpactClass,
        steps: Vec<EvidenceStep>,
        score: f32,
    ) -> EvidencePath {
        EvidencePath {
            change,
            reason,
            class,
            steps,
            score,
        }
    }

    /// An affected item at `site` with the given paths, the first one best.
    fn item(
        path: &str,
        site: Location,
        class: ImpactClass,
        confidence: Confidence,
        paths: Vec<EvidencePath>,
    ) -> ImpactItem {
        let distance = paths.first().map_or(0, |path| path.steps.len() as u32);
        let score = paths.first().map_or(0.0, |path| path.score);
        let path_count = paths.len() as u32;
        ImpactItem {
            symbol: symbol(path),
            location: site,
            class,
            confidence,
            score,
            distance,
            paths,
            path_count,
            tags: ImpactTags::default(),
        }
    }

    /// An analysis of `changes` that reached `items`.
    fn analysis_of(changes: Vec<Change>, items: Vec<ImpactItem>) -> Analysis {
        Analysis {
            diff: DiffResult {
                comparison: Comparison {
                    base: Source::Tree {
                        id: "0f2a91c000000000000000000000000000000000".to_owned(),
                        label: "HEAD".to_owned(),
                    },
                    head: Source::Worktree,
                },
                base_key: None,
                head_key: None,
                changed_files: Vec::new(),
                changes,
                limits: Vec::new(),
            },
            items,
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        }
    }

    /// The analysis compared against the layout: three changes of which one is
    /// formatting only, three adaptations of which one was edited, two
    /// behaviour items, three selected tests and two limits.
    fn mixed_analysis() -> Analysis {
        let changes = vec![
            change(
                ChangeKind::Signature,
                "billing::discount::apply_discount",
                "src/billing/discount.rs",
                41,
            ),
            member_change(
                ChangeKind::Variant,
                "billing::PaymentState::Refunded",
                "src/billing/payment.rs",
                8,
                Delta::Added,
            ),
            change(
                ChangeKind::Body,
                "billing::tax::vat_rate",
                "src/billing/tax.rs",
                15,
            ),
            change(
                ChangeKind::NonSemantic,
                "billing::util::helper",
                "src/billing/util.rs",
                3,
            ),
        ];

        let calculate_total = item(
            "billing::total::calculate_total",
            location("src/billing/total.rs", 27),
            ImpactClass::Adaptation,
            Confidence::Definite,
            vec![evidence(
                0,
                Reason::SignatureChanged,
                ImpactClass::Adaptation,
                vec![step(
                    "billing::total::calculate_total",
                    "billing::discount::apply_discount",
                    EdgeKind::Calls,
                    "src/billing/total.rs",
                    27,
                )],
                0.9,
            )],
        );
        let describe = item(
            "billing::payment::describe",
            location("src/billing/payment.rs", 63),
            ImpactClass::Adaptation,
            Confidence::Definite,
            vec![evidence(
                1,
                Reason::ExhaustiveMatchMissesVariant,
                ImpactClass::Adaptation,
                vec![step(
                    "billing::payment::describe",
                    "billing::PaymentState",
                    EdgeKind::Matches,
                    "src/billing/payment.rs",
                    63,
                )],
                0.8,
            )],
        );

        let mut generate = item(
            "billing::invoice::generate",
            location("src/billing/invoice.rs", 58),
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![evidence(
                2,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![
                    step(
                        "billing::tax::apply",
                        "billing::tax::vat_rate",
                        EdgeKind::Calls,
                        "src/billing/tax.rs",
                        40,
                    ),
                    step(
                        "billing::invoice::generate",
                        "billing::tax::apply",
                        EdgeKind::Calls,
                        "src/billing/invoice.rs",
                        61,
                    ),
                ],
                0.48,
            )],
        );
        generate.tags.is_test = true;
        generate.tags.is_pub_api = true;

        let mut summarise = item(
            "billing::report::summarise",
            location("src/billing/report.rs", 77),
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![evidence(
                2,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![step(
                    "billing::report::summarise",
                    "billing::tax::vat_rate",
                    EdgeKind::Calls,
                    "src/billing/report.rs",
                    77,
                )],
                0.4,
            )],
        );
        summarise.tags.changed = true;

        let dispatch = item(
            "api::handlers::dispatch",
            location("src/api/handlers.rs", 112),
            ImpactClass::Behaviour,
            Confidence::Possible,
            vec![evidence(
                1,
                Reason::DispatchesToChanged,
                ImpactClass::Behaviour,
                vec![step(
                    "api::handlers::dispatch",
                    "billing::PaymentState::Refunded",
                    EdgeKind::MayDispatchTo,
                    "src/api/handlers.rs",
                    112,
                )],
                0.2,
            )],
        );

        let mut recalculate = item(
            "billing::total::recalculate",
            location("src/billing/total.rs", 88),
            ImpactClass::Adaptation,
            Confidence::Definite,
            vec![evidence(
                0,
                Reason::SignatureChanged,
                ImpactClass::Adaptation,
                vec![step(
                    "billing::total::recalculate",
                    "billing::discount::apply_discount",
                    EdgeKind::Calls,
                    "src/billing/total.rs",
                    88,
                )],
                0.7,
            )],
        );
        recalculate.tags.changed = true;

        let mut analysis = analysis_of(
            changes,
            vec![
                calculate_total,
                describe,
                generate,
                summarise,
                dispatch,
                recalculate,
            ],
        );
        analysis.diff.limits = vec![Limit::Unresolved { count: 2 }, Limit::Submodules];
        analysis.tests = TestSelection {
            selected: vec![
                SelectedTest {
                    symbol: symbol("billing::tax::tests::rounds"),
                    location: location("src/billing/tax.rs", 120),
                    reason: TestReason::Changed,
                    filter: "tax::tests::rounds".to_owned(),
                },
                SelectedTest {
                    symbol: symbol("billing::invoice::tests::totals"),
                    location: location("src/billing/invoice.rs", 200),
                    reason: TestReason::Reached,
                    filter: "invoice::tests::totals".to_owned(),
                },
                SelectedTest {
                    symbol: symbol("api::tests::checkout"),
                    location: location("src/api/mod.rs", 40),
                    reason: TestReason::Reached,
                    filter: "tests::checkout".to_owned(),
                },
            ],
            total: 212,
            commands: vec![
                TestCommand {
                    package: "billing".to_owned(),
                    target_kind: TargetKind::Lib,
                    target_name: "billing".to_owned(),
                    filters: vec![
                        "tax::".to_owned(),
                        "invoice::tests::totals".to_owned(),
                        "report::".to_owned(),
                    ],
                    tests: 2,
                    total: 98,
                },
                TestCommand {
                    package: "api".to_owned(),
                    target_kind: TargetKind::Lib,
                    target_name: "api".to_owned(),
                    filters: vec!["checkout::".to_owned()],
                    tests: 1,
                    total: 1,
                },
            ],
        };
        analysis.limits.unresolved_total = 2;
        analysis.limits.unresolved_in_reach = 1;
        analysis.limits.candidate_steps = 1;
        analysis
    }

    /// One command that runs the tests of one target.
    fn command(package: &str, kind: TargetKind, name: &str, tests: u32, total: u32) -> TestCommand {
        TestCommand {
            package: package.to_owned(),
            target_kind: kind,
            target_name: name.to_owned(),
            filters: Vec::new(),
            tests,
            total,
        }
    }

    /// A selection of `count` reached tests in a project of `total` tests, run
    /// by the given commands.
    fn selection(total: u32, commands: Vec<TestCommand>, count: usize) -> TestSelection {
        let selected = (0..count)
            .map(|index| SelectedTest {
                symbol: symbol(&format!("crate::tests::case_{index}")),
                location: location("src/lib.rs", index as u32 + 1),
                reason: TestReason::Reached,
                filter: format!("tests::case_{index}"),
            })
            .collect();
        TestSelection {
            selected,
            total,
            commands,
        }
    }

    /// `count` adaptation items, to fill a section.
    fn adaptations(count: usize) -> Vec<ImpactItem> {
        (0..count)
            .map(|index| {
                let path = format!("crate::module::item_{index}");
                let line = index as u32 + 1;
                item(
                    &path,
                    location("src/lib.rs", line),
                    ImpactClass::Adaptation,
                    Confidence::Definite,
                    vec![evidence(
                        0,
                        Reason::CallsChanged,
                        ImpactClass::Adaptation,
                        vec![step(
                            &path,
                            "crate::changed",
                            EdgeKind::Calls,
                            "src/lib.rs",
                            line,
                        )],
                        0.5,
                    )],
                )
            })
            .collect()
    }

    /// The text with every ANSI escape sequence removed.
    fn without_colour(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut in_escape = false;
        for character in text.chars() {
            if in_escape {
                if character == 'm' {
                    in_escape = false;
                }
            } else if character == '\x1b' {
                in_escape = true;
            } else {
                out.push(character);
            }
        }
        out
    }

    #[test]
    fn the_full_report_lays_out_every_section() {
        let text = render_analysis(
            &mixed_analysis(),
            &AnalysisOptions::default(),
            &Style::default(),
        );

        let expected = r"Comparing HEAD → working tree · features: default

CHANGES (3)
  signature  billing::discount::apply_discount  src/billing/discount.rs:41
  variant +  billing::PaymentState::Refunded    src/billing/payment.rs:8
  body       billing::tax::vat_rate             src/billing/tax.rs:15

1 formatting-only change not shown

NEEDS EDITING (2)
  billing::total::calculate_total  calls apply_discount, whose signature changed  src/billing/total.rs:27
  billing::payment::describe       match without a `_` arm misses Refunded        src/billing/payment.rs:63

1 dependant was edited together with the change (--all lists it).

MAY BEHAVE DIFFERENTLY (2)
  likely    billing::invoice::generate  2 steps from vat_rate [test] [public API]  src/billing/invoice.rs:61
  possible  api::handlers::dispatch     may dispatch to Refunded                   src/api/handlers.rs:112

TESTS TO RUN (3 of 212)
  billing · unit tests  2 of 98
  api · unit tests      1 of 1
Run `blastr tests` to see their names, or `blastr tests --format cargo` for the commands.

LIMITS  submodules are not analysed · 2 unresolved references (1 near the changes) · 1 candidate step

Run `blastr explain <symbol>` to see why an item is listed.
";
        assert_eq!(text, expected);
    }

    #[test]
    fn the_changes_section_keeps_its_own_cap() {
        let changes: Vec<Change> = (0..super::super::MAX_LISTED + 5)
            .map(|index| {
                change(
                    ChangeKind::Body,
                    &format!("crate::module::function_{index}"),
                    "src/lib.rs",
                    index as u32 + 1,
                )
            })
            .collect();
        let analysis = analysis_of(changes, Vec::new());

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(text.contains("CHANGES (25)\n"), "{text}");
        assert!(text.contains("  … and 5 more (use --all)"), "{text}");
        assert_eq!(text.matches("  body  ").count(), super::super::MAX_LISTED);
        assert!(text.contains("Nothing else is affected."), "{text}");
    }

    #[test]
    fn a_section_lists_at_most_ten_items_and_says_how_many_there_are() {
        let analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 1)],
            adaptations(12),
        );
        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());

        assert!(text.contains("NEEDS EDITING (12, showing 10)\n"), "{text}");
        assert_eq!(text.matches("crate::module::item_").count(), 10, "{text}");
        assert!(text.contains("or add --all for the full list."), "{text}");
        assert!(!text.contains("Nothing else is affected."), "{text}");
    }

    #[test]
    fn listing_everything_lifts_the_cap() {
        let analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 1)],
            adaptations(12),
        );
        let style = Style {
            list_all: true,
            ..Style::default()
        };
        let text = render_analysis(&analysis, &AnalysisOptions::default(), &style);

        assert!(text.contains("NEEDS EDITING (12)\n"), "{text}");
        assert!(!text.contains("showing"), "{text}");
        assert_eq!(text.matches("crate::module::item_").count(), 12, "{text}");
        assert!(!text.contains("--all"), "{text}");
        assert!(
            text.contains("Run `blastr explain <symbol>` to see why an item is listed.\n"),
            "{text}"
        );
    }

    #[test]
    fn changed_behaviour_items_are_left_to_the_changes_section() {
        let mut changed = item(
            "billing::report::summarise",
            location("src/billing/report.rs", 77),
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![evidence(
                0,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![step(
                    "billing::report::summarise",
                    "billing::tax::vat_rate",
                    EdgeKind::Calls,
                    "src/billing/report.rs",
                    77,
                )],
                0.4,
            )],
        );
        changed.tags.changed = true;
        let untouched = item(
            "billing::invoice::generate",
            location("src/billing/invoice.rs", 58),
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![evidence(
                0,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![step(
                    "billing::invoice::generate",
                    "billing::tax::vat_rate",
                    EdgeKind::Calls,
                    "src/billing/invoice.rs",
                    58,
                )],
                0.3,
            )],
        );
        let analysis = analysis_of(
            vec![change(
                ChangeKind::Body,
                "billing::tax::vat_rate",
                "src/billing/tax.rs",
                15,
            )],
            vec![changed, untouched],
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(text.contains("MAY BEHAVE DIFFERENTLY (1)\n"), "{text}");
        assert!(text.contains("billing::invoice::generate"), "{text}");
        assert!(!text.contains("billing::report::summarise"), "{text}");
    }

    /// One adaptation item of the module, edited in the same diff or not.
    fn module_adaptation(index: u32, edited: bool) -> ImpactItem {
        let path = format!("crate::module::item_{index}");
        let line = index + 1;
        let mut item = item(
            &path,
            location("src/lib.rs", line),
            ImpactClass::Adaptation,
            Confidence::Definite,
            vec![evidence(
                0,
                Reason::CallsChanged,
                ImpactClass::Adaptation,
                vec![step(
                    &path,
                    "crate::changed",
                    EdgeKind::Calls,
                    "src/lib.rs",
                    line,
                )],
                0.9,
            )],
        );
        item.tags.changed = edited;
        item
    }

    #[test]
    fn edited_adaptations_are_summarised_after_the_section() {
        let analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 9)],
            vec![module_adaptation(0, true), module_adaptation(1, false)],
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(text.contains("NEEDS EDITING (1)\n"), "{text}");
        assert!(text.contains("crate::module::item_1"), "{text}");
        assert!(!text.contains("crate::module::item_0"), "{text}");
        assert!(
            text.contains("1 dependant was edited together with the change (--all lists it)."),
            "{text}"
        );
    }

    #[test]
    fn the_edited_summary_counts_in_the_plural() {
        let analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 9)],
            vec![
                module_adaptation(0, true),
                module_adaptation(1, true),
                module_adaptation(2, false),
            ],
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(text.contains("NEEDS EDITING (1)\n"), "{text}");
        assert!(
            text.contains("2 dependants were edited together with the change (--all lists them)."),
            "{text}"
        );
    }

    #[test]
    fn the_edited_summary_takes_the_place_of_an_empty_section() {
        let analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 9)],
            vec![module_adaptation(0, true)],
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(!text.contains("NEEDS EDITING"), "{text}");
        assert!(!text.contains("Nothing else is affected."), "{text}");
        assert!(
            text.contains("1 dependant was edited together with the change (--all lists it)."),
            "{text}"
        );
    }

    #[test]
    fn asking_for_everything_lists_the_edited_adaptations() {
        let analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 9)],
            vec![module_adaptation(0, true), module_adaptation(1, true)],
        );
        let style = Style {
            list_all: true,
            ..Style::default()
        };

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &style);
        assert!(text.contains("EDITED WITH THE CHANGE (2)\n"), "{text}");
        assert!(text.contains("crate::module::item_0"), "{text}");
        assert!(text.contains("crate::module::item_1"), "{text}");
        assert!(text.contains("calls changed"), "{text}");
        assert!(!text.contains("dependant"), "{text}");
        assert!(!text.contains("Nothing else is affected."), "{text}");
    }

    #[test]
    fn nothing_else_is_affected_when_no_item_is_listed() {
        let analysis = analysis_of(
            vec![change(
                ChangeKind::Body,
                "billing::tax::vat_rate",
                "src/billing/tax.rs",
                15,
            )],
            Vec::new(),
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(text.contains("\n\nNothing else is affected.\n"), "{text}");
        assert!(!text.contains("NEEDS EDITING"), "{text}");
        assert!(!text.contains("MAY BEHAVE DIFFERENTLY"), "{text}");
        assert!(!text.contains("blastr explain"), "{text}");
    }

    #[test]
    fn a_formatting_only_comparison_says_nothing_about_impact() {
        let analysis = analysis_of(
            vec![change(
                ChangeKind::NonSemantic,
                "billing::tax::vat_rate",
                "src/billing/tax.rs",
                15,
            )],
            Vec::new(),
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(
            text.contains("CHANGES (0)\n  no structural changes\n"),
            "{text}"
        );
        assert!(!text.contains("Nothing else is affected."), "{text}");
        assert!(
            text.contains("1 formatting-only change not shown"),
            "{text}"
        );
    }

    #[test]
    fn the_tests_section_is_omitted_when_the_project_has_no_tests() {
        let analysis = analysis_of(Vec::new(), Vec::new());

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(!text.contains("TESTS TO RUN"), "{text}");
    }

    #[test]
    fn a_project_whose_tests_are_not_reached_says_so() {
        let mut analysis = analysis_of(Vec::new(), Vec::new());
        analysis.tests.total = 212;

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(
            text.contains("TESTS TO RUN (0 of 212)\n  no test reaches the changed code\n"),
            "{text}"
        );
    }

    #[test]
    fn the_limits_line_lists_every_entry_in_order() {
        let mut analysis = analysis_of(Vec::new(), Vec::new());
        analysis.diff.limits = vec![
            Limit::Unresolved { count: 5 },
            Limit::Restricted,
            Limit::Submodules,
            Limit::Lfs,
            Limit::ChangedButInactive(location("src/a.rs", 4)),
        ];
        analysis.limits.unresolved_total = 3;
        analysis.limits.unresolved_in_reach = 2;
        analysis.limits.candidate_steps = 4;
        analysis.limits.coarse_rules = 2;
        analysis.limits.beyond_depth = 2;
        analysis.limits.below_floor = 5;
        analysis.limits.below_confidence = 6;
        analysis.limits.ignored = 7;
        analysis.params.depth = 4;
        analysis.params.min_confidence = Confidence::Likely;

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(
            text.contains(
                "LIMITS  build scripts and procedural macros were not run · submodules are not \
                 analysed · Git LFS content is not analysed · change in inactive code at \
                 src/a.rs:4 · 3 unresolved references (2 near the changes) · 4 candidate steps · \
                 2 coarse rules · 2 items beyond depth 4 · 5 items below the score floor · 6 items \
                 below likely confidence · 7 items in ignored files\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn limits_use_the_singular_for_one() {
        let mut analysis = analysis_of(Vec::new(), Vec::new());
        analysis.limits.unresolved_total = 1;
        analysis.limits.candidate_steps = 1;
        analysis.limits.coarse_rules = 1;
        analysis.limits.beyond_depth = 1;
        analysis.limits.below_floor = 1;
        analysis.limits.below_confidence = 1;
        analysis.limits.ignored = 1;

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(
            text.contains(
                "LIMITS  1 unresolved reference · 1 candidate step · 1 coarse rule · 1 item beyond \
                 depth 6 · 1 item below the score floor · 1 item below possible confidence · 1 item \
                 in an ignored file\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn unresolved_references_without_reach_omit_the_parenthesis() {
        let mut analysis = analysis_of(Vec::new(), Vec::new());
        analysis.limits.unresolved_total = 4;

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(text.contains("LIMITS  4 unresolved references\n"), "{text}");
        assert!(!text.contains("near the changes"), "{text}");
    }

    #[test]
    fn the_limits_line_is_omitted_when_nothing_limits_the_result() {
        let analysis = analysis_of(Vec::new(), Vec::new());

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(!text.contains("LIMITS"), "{text}");
    }

    #[test]
    fn a_long_reason_is_truncated_before_the_location() {
        let changes = vec![change(
            ChangeKind::Body,
            "billing::discount::apply_discount",
            "src/billing/discount.rs",
            41,
        )];
        let likely = item(
            "billing::invoice::generate",
            location("src/billing/invoice.rs", 58),
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![evidence(
                0,
                Reason::SignatureChanged,
                ImpactClass::Behaviour,
                vec![step(
                    "billing::invoice::generate",
                    "billing::discount::apply_discount",
                    EdgeKind::Calls,
                    "src/billing/invoice.rs",
                    61,
                )],
                0.48,
            )],
        );
        let possible = item(
            "api::handlers::dispatch",
            location("src/api/handlers.rs", 112),
            ImpactClass::Behaviour,
            Confidence::Possible,
            vec![evidence(
                0,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![step(
                    "api::handlers::dispatch",
                    "billing::discount::apply_discount",
                    EdgeKind::Calls,
                    "src/api/handlers.rs",
                    112,
                )],
                0.2,
            )],
        );
        let analysis = analysis_of(changes, vec![likely, possible]);
        let style = Style {
            width: 100,
            ..Style::default()
        };

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &style);
        assert!(
            text.contains(
                "  likely    billing::invoice::generate  calls apply_discount, whose sign…  \
                 src/billing/invoice.rs:61\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "  possible  api::handlers::dispatch     calls apply_discount               \
                 src/api/handlers.rs:112\n"
            ),
            "{text}"
        );
        for line in text.lines() {
            assert!(
                line.chars().count() <= 100,
                "line is {} characters wide: {line}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn a_narrow_limit_keeps_the_location_whole() {
        let analysis = mixed_analysis();
        let style = Style {
            width: 40,
            ..Style::default()
        };

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &style);
        assert!(text.contains("src/billing/total.rs:27"), "{text}");
        assert!(text.contains("src/billing/payment.rs:63"), "{text}");
        assert!(text.contains("calls a…"), "{text}");
        assert!(
            !text.contains("calls apply_discount, whose signature changed"),
            "{text}"
        );
    }

    #[test]
    fn colour_codes_do_not_change_the_alignment() {
        let analysis = mixed_analysis();
        let plain = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        let colourful = Style {
            colour: true,
            ..Style::default()
        };
        let coloured = render_analysis(&analysis, &AnalysisOptions::default(), &colourful);

        assert_eq!(without_colour(&coloured), plain);
        assert!(!plain.contains('\x1b'), "{plain}");
        assert!(
            coloured.contains("\x1b[1mNEEDS EDITING (2)\x1b[0m"),
            "{coloured}"
        );
        assert!(
            coloured.contains("\x1b[31mbilling::total::calculate_total\x1b[0m"),
            "{coloured}"
        );
        assert!(coloured.contains("\x1b[33mlikely\x1b[0m"), "{coloured}");
        assert!(coloured.contains("\x1b[2mpossible\x1b[0m"), "{coloured}");
        assert!(
            coloured.contains("\x1b[1mTESTS TO RUN (3 of 212)\x1b[0m"),
            "{coloured}"
        );
        assert!(coloured.contains("\x1b[1mLIMITS\x1b[0m  "), "{coloured}");
    }

    #[test]
    fn long_symbols_are_shortened_in_the_sections() {
        let long = format!("crate::{}tail", "segment::".repeat(12));
        let analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 1)],
            vec![item(
                &long,
                location("src/lib.rs", 3),
                ImpactClass::Adaptation,
                Confidence::Definite,
                vec![evidence(
                    0,
                    Reason::CallsChanged,
                    ImpactClass::Adaptation,
                    vec![step(
                        &long,
                        "crate::changed",
                        EdgeKind::Calls,
                        "src/lib.rs",
                        3,
                    )],
                    0.5,
                )],
            )],
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(
            !text.contains(&long),
            "the full path must not be printed:\n{text}"
        );
        let shortened = super::super::shorten(&long);
        assert!(text.contains(&shortened), "{text}");
        assert!(
            shortened.chars().count() <= super::super::SYMBOL_WIDTH,
            "{shortened}"
        );
    }

    #[test]
    fn every_reason_reads_as_a_short_phrase() {
        let body = change(
            ChangeKind::Body,
            "billing::tax::vat_rate",
            "src/billing/tax.rs",
            15,
        );
        assert_eq!(reason_text(Reason::CallsChanged, &body), "calls vat_rate");
        assert_eq!(
            reason_text(Reason::TakesChangedAsValue, &body),
            "passes vat_rate as a value"
        );
        assert_eq!(
            reason_text(Reason::DispatchesToChanged, &body),
            "may dispatch to vat_rate"
        );
        assert_eq!(
            reason_text(Reason::SignatureChanged, &body),
            "calls vat_rate, whose signature changed"
        );
        assert_eq!(
            reason_text(Reason::DeclarationChanged, &body),
            "uses vat_rate, whose declaration changed"
        );
        assert_eq!(
            reason_text(Reason::NoLongerVisible, &body),
            "can no longer see vat_rate"
        );
        assert_eq!(
            reason_text(Reason::RefersToRemoved, &body),
            "uses removed vat_rate"
        );
        assert_eq!(
            reason_text(Reason::RefersToRenamed, &body),
            "uses vat_rate by its old name"
        );
        assert_eq!(
            reason_text(Reason::UsesChangedField, &body),
            "uses field vat_rate"
        );
        assert_eq!(
            reason_text(Reason::ConstructsWithoutField, &body),
            "builds the struct without vat_rate"
        );
        assert_eq!(
            reason_text(Reason::PatternMissesField, &body),
            "pattern does not name vat_rate"
        );
        assert_eq!(
            reason_text(Reason::ExhaustiveMatchMissesVariant, &body),
            "match without a `_` arm misses vat_rate"
        );
        assert_eq!(
            reason_text(Reason::WildcardCatchesNewVariant, &body),
            "`_` arm now also catches vat_rate"
        );
        assert_eq!(
            reason_text(Reason::UsesChangedVariant, &body),
            "uses variant vat_rate"
        );
        assert_eq!(
            reason_text(Reason::MustImplementTraitItem, &body),
            "must implement vat_rate"
        );
        assert_eq!(
            reason_text(Reason::UsesChangedTraitItem, &body),
            "uses trait item vat_rate"
        );
        assert_eq!(
            reason_text(Reason::InheritsChangedDefault, &body),
            "inherits the default vat_rate"
        );
        assert_eq!(
            reason_text(Reason::UsesChangedValue, &body),
            "uses vat_rate"
        );
        assert_eq!(
            reason_text(Reason::UsesRetypedValue, &body),
            "uses vat_rate, whose type changed"
        );
        assert_eq!(
            reason_text(Reason::UsesChangedAlias, &body),
            "uses alias vat_rate"
        );
        assert_eq!(
            reason_text(Reason::InvokesChangedMacro, &body),
            "invokes vat_rate!"
        );
        assert_eq!(
            reason_text(Reason::AttributeChanged, &body),
            "calls vat_rate, whose attributes changed"
        );
    }

    #[test]
    fn a_rename_names_the_old_and_the_new_paths() {
        let renamed = Change {
            detail: ChangeDetail::Renamed {
                from: symbol("billing::tax::vat"),
            },
            ..change(
                ChangeKind::Renamed,
                "billing::tax::rate",
                "src/billing/tax.rs",
                15,
            )
        };
        assert_eq!(
            reason_text(Reason::RefersToRenamed, &renamed),
            "uses vat, now rate"
        );
    }

    #[test]
    fn an_impl_change_says_whether_it_was_added_or_removed() {
        let added = member_change(
            ChangeKind::Impl,
            "billing::Point::double",
            "src/billing/point.rs",
            9,
            Delta::Added,
        );
        assert_eq!(
            reason_text(Reason::ImplResolutionChanged, &added),
            "calls a method of an impl that was added"
        );

        let removed = member_change(
            ChangeKind::Impl,
            "billing::Point::double",
            "src/billing/point.rs",
            9,
            Delta::Removed,
        );
        assert_eq!(
            reason_text(Reason::ImplResolutionChanged, &removed),
            "calls a method of an impl that was removed"
        );

        let modified = member_change(
            ChangeKind::Impl,
            "billing::Point::double",
            "src/billing/point.rs",
            9,
            Delta::Modified,
        );
        assert_eq!(
            reason_text(Reason::ImplResolutionChanged, &modified),
            "calls a method of an impl that changed"
        );
    }

    #[test]
    fn a_path_of_one_step_is_summarised_by_its_reason() {
        let analysis = mixed_analysis();

        assert_eq!(
            path_summary(&analysis.items[0], &analysis),
            "calls apply_discount, whose signature changed"
        );
    }

    #[test]
    fn a_longer_path_counts_its_steps_from_the_change() {
        let analysis = mixed_analysis();

        assert_eq!(
            path_summary(&analysis.items[2], &analysis),
            "2 steps from vat_rate"
        );
    }

    #[test]
    fn a_path_through_a_trait_object_says_so() {
        let dispatch = item(
            "api::handlers::dispatch",
            location("src/api/handlers.rs", 112),
            ImpactClass::Behaviour,
            Confidence::Possible,
            vec![evidence(
                0,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![
                    step(
                        "api::handlers::Call",
                        "billing::tax::vat_rate",
                        EdgeKind::MayDispatchTo,
                        "src/api/handlers.rs",
                        20,
                    ),
                    step(
                        "api::handlers::dispatch",
                        "api::handlers::Call",
                        EdgeKind::Calls,
                        "src/api/handlers.rs",
                        112,
                    ),
                ],
                0.2,
            )],
        );
        let analysis = analysis_of(
            vec![change(
                ChangeKind::Body,
                "billing::tax::vat_rate",
                "src/billing/tax.rs",
                15,
            )],
            vec![dispatch],
        );

        assert_eq!(
            path_summary(&analysis.items[0], &analysis),
            "2 steps from vat_rate through a trait object"
        );
    }

    #[test]
    fn a_path_through_a_function_pointer_says_so() {
        let pointer = item(
            "api::handlers::dispatch",
            location("src/api/handlers.rs", 112),
            ImpactClass::Behaviour,
            Confidence::Possible,
            vec![evidence(
                0,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![
                    step(
                        "api::handlers::CALLBACK",
                        "billing::tax::vat_rate",
                        EdgeKind::TakesAsValue,
                        "src/api/handlers.rs",
                        20,
                    ),
                    step(
                        "api::handlers::dispatch",
                        "api::handlers::CALLBACK",
                        EdgeKind::Calls,
                        "src/api/handlers.rs",
                        112,
                    ),
                ],
                0.2,
            )],
        );
        let analysis = analysis_of(
            vec![change(
                ChangeKind::Body,
                "billing::tax::vat_rate",
                "src/billing/tax.rs",
                15,
            )],
            vec![pointer],
        );

        assert_eq!(
            path_summary(&analysis.items[0], &analysis),
            "2 steps from vat_rate through a function pointer"
        );
    }

    #[test]
    fn the_tests_report_lists_the_targets_and_the_tests() {
        let analysis = mixed_analysis();

        let expected = r"TESTS TO RUN (3 of 212)
  billing · unit tests  2 of 98
  api · unit tests      1 of 1

Tests
  billing::tax::tests::rounds      changed  src/billing/tax.rs:120
  billing::invoice::tests::totals  reached  src/billing/invoice.rs:200
  api::tests::checkout             reached  src/api/mod.rs:40
Run `blastr tests --format cargo` for the commands.
";
        assert_eq!(render_tests(&analysis, &Style::default()), expected);
    }

    #[test]
    fn every_kind_of_target_has_a_label() {
        let cases = [
            (TargetKind::Lib, "unit tests"),
            (TargetKind::ProcMacro, "unit tests"),
            (TargetKind::Bin, "bin smoke"),
            (TargetKind::Test, "tests/smoke.rs"),
            (TargetKind::Example, "examples/smoke.rs"),
            (TargetKind::Bench, "benches/smoke.rs"),
            (TargetKind::BuildScript, "smoke"),
            (TargetKind::Other, "smoke"),
        ];

        for (kind, label) in cases {
            assert_eq!(
                target_label(&command("billing", kind, "smoke", 1, 2)),
                label,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn the_tests_section_aligns_one_line_per_target() {
        let mut analysis = analysis_of(
            vec![change(ChangeKind::Body, "crate::changed", "src/lib.rs", 1)],
            Vec::new(),
        );
        analysis.tests = selection(
            144,
            vec![
                command("serde_json", TargetKind::Test, "test", 46, 98),
                command("serde_json", TargetKind::Test, "stream", 12, 12),
                command("serde_json", TargetKind::Test, "regression", 5, 9),
                command("serde_json", TargetKind::Lib, "serde_json", 5, 25),
            ],
            68,
        );

        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        let expected = r"TESTS TO RUN (68 of 144)
  serde_json · tests/test.rs        46 of 98
  serde_json · tests/stream.rs      12 of 12
  serde_json · tests/regression.rs   5 of 9
  serde_json · unit tests            5 of 25
Run `blastr tests` to see their names, or `blastr tests --format cargo` for the commands.
";
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains("cargo test"), "{text}");
    }

    #[test]
    fn a_command_that_runs_its_whole_target_reports_the_selected_count() {
        let mut analysis = analysis_of(Vec::new(), Vec::new());
        analysis.tests = selection(
            144,
            vec![command("serde_json", TargetKind::Test, "stream", 5, 98)],
            5,
        );

        let text = render_tests(&analysis, &Style::default());
        assert!(
            text.contains("  serde_json · tests/stream.rs  5 of 98\n"),
            "{text}"
        );
        assert!(!text.contains("cargo test"), "{text}");
    }

    #[test]
    fn the_tests_report_caps_the_list_and_points_at_all() {
        let mut analysis = analysis_of(Vec::new(), Vec::new());
        analysis.tests = selection(
            144,
            vec![command("serde_json", TargetKind::Test, "test", 35, 98)],
            35,
        );

        let text = render_tests(&analysis, &Style::default());
        assert!(text.starts_with("TESTS TO RUN (35 of 144)\n"), "{text}");
        assert_eq!(text.matches("  crate::tests::case_").count(), MAX_TESTS);
        assert!(text.contains("  … and 5 more (use --all)\n"), "{text}");
        assert!(
            text.ends_with("Run `blastr tests --format cargo` for the commands.\n"),
            "{text}"
        );

        let style = Style {
            list_all: true,
            ..Style::default()
        };
        let all = render_tests(&analysis, &style);
        assert_eq!(all.matches("  crate::tests::case_").count(), 35);
        assert!(!all.contains("--all"), "{all}");
    }

    #[test]
    fn the_tests_report_says_when_no_test_is_reached() {
        let mut analysis = analysis_of(Vec::new(), Vec::new());
        analysis.tests.total = 30;

        assert_eq!(
            render_tests(&analysis, &Style::default()),
            "TESTS TO RUN (0 of 30)\n  no test reaches the changed code\n"
        );
    }

    #[test]
    fn the_tests_report_says_when_there_are_no_tests() {
        let analysis = analysis_of(Vec::new(), Vec::new());

        assert_eq!(
            render_tests(&analysis, &Style::default()),
            "TESTS TO RUN (0 of 0)\n  this project has no tests\n"
        );
    }

    #[test]
    fn explain_shows_the_item_and_its_paths() {
        let changes = vec![change(
            ChangeKind::Body,
            "billing::tax::vat_rate",
            "src/billing/tax.rs",
            15,
        )];
        let mut candidate = step(
            "billing::invoice::generate",
            "billing::tax::apply",
            EdgeKind::Calls,
            "src/billing/invoice.rs",
            61,
        );
        candidate.certainty = Certainty::Candidate;
        let mut generate = item(
            "billing::invoice::generate",
            location("src/billing/invoice.rs", 58),
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![
                evidence(
                    0,
                    Reason::CallsChanged,
                    ImpactClass::Behaviour,
                    vec![
                        step(
                            "billing::tax::apply",
                            "billing::tax::vat_rate",
                            EdgeKind::Calls,
                            "src/billing/tax.rs",
                            40,
                        ),
                        candidate,
                    ],
                    0.48,
                ),
                evidence(
                    0,
                    Reason::CallsChanged,
                    ImpactClass::Behaviour,
                    vec![step(
                        "billing::invoice::generate",
                        "billing::tax::vat_rate",
                        EdgeKind::Calls,
                        "src/billing/invoice.rs",
                        70,
                    )],
                    0.3,
                ),
            ],
        );
        generate.tags.is_test = true;
        generate.tags.is_pub_api = true;
        generate.path_count = 4;
        let analysis = analysis_of(changes, vec![generate]);

        let text = render_explain(&analysis, "billing::invoice::generate", &Style::default())
            .expect("the item is in the result");
        let expected = r"billing::invoice::generate
  may behave differently · likely · score 0.48 · 2 steps
  src/billing/invoice.rs:58
  tags: test, public API

Path 1 of 2 · starts at the body change of billing::tax::vat_rate
  billing::tax::vat_rate      changed (body)           src/billing/tax.rs:15
  billing::tax::apply         calls vat_rate           src/billing/tax.rs:40
  billing::invoice::generate  calls apply (candidate)  src/billing/invoice.rs:61

Path 2 of 2 · starts at the body change of billing::tax::vat_rate
  billing::tax::vat_rate      changed (body)  src/billing/tax.rs:15
  billing::invoice::generate  calls vat_rate  src/billing/invoice.rs:70

2 more paths are not shown.

Limits: this path goes through a step that could not be resolved to one target, so the item may not be affected.
";
        assert_eq!(text, expected);
    }

    #[test]
    fn explain_finds_an_item_by_the_end_of_its_path() {
        let analysis = mixed_analysis();

        let text = render_explain(&analysis, "generate", &Style::default())
            .expect("the suffix names one item");
        assert!(text.starts_with("billing::invoice::generate\n"), "{text}");
        assert!(text.contains("tags: test, public API"), "{text}");
    }

    #[test]
    fn explain_says_how_an_adaptation_reads() {
        let analysis = mixed_analysis();

        let text = render_explain(&analysis, "calculate_total", &Style::default())
            .expect("the suffix names one item");
        assert!(
            text.contains("  needs editing · definite · score 0.90 · 1 step\n"),
            "{text}"
        );
        assert!(text.contains("  src/billing/total.rs:27"), "{text}");
    }

    #[test]
    fn explain_omits_the_tags_line_without_tags() {
        let analysis = mixed_analysis();

        let text = render_explain(&analysis, "api::handlers::dispatch", &Style::default())
            .expect("the item is in the result");
        assert!(!text.contains("tags:"), "{text}");
        assert!(text.contains("· possible · score 0.20 · 1 step"), "{text}");
    }

    #[test]
    fn explain_reports_an_ambiguous_suffix() {
        let first = item(
            "billing::tax::apply",
            location("src/billing/tax.rs", 40),
            ImpactClass::Adaptation,
            Confidence::Definite,
            Vec::new(),
        );
        let second = item(
            "billing::invoice::apply",
            location("src/billing/invoice.rs", 10),
            ImpactClass::Adaptation,
            Confidence::Definite,
            Vec::new(),
        );
        let analysis = analysis_of(Vec::new(), vec![first, second]);

        let error = render_explain(&analysis, "apply", &Style::default())
            .expect_err("the query matches several items");
        assert_eq!(
            error,
            "`apply` matches several items: billing::invoice::apply, billing::tax::apply"
        );
    }

    #[test]
    fn explain_reports_a_change_as_the_start_of_the_impact() {
        let analysis = mixed_analysis();

        let error = render_explain(&analysis, "vat_rate", &Style::default())
            .expect_err("the change is not an item");
        assert_eq!(
            error,
            "`billing::tax::vat_rate` is one of the changes (body); it is where the impact starts, \
             not an affected item"
        );
    }

    #[test]
    fn explain_reports_a_symbol_the_changes_do_not_touch() {
        let analysis = mixed_analysis();

        let error = render_explain(&analysis, "other::thing", &Style::default())
            .expect_err("nothing matches");
        assert_eq!(error, "`other::thing` is not affected by these changes");
    }

    #[test]
    fn an_item_without_paths_still_renders() {
        let bare = item(
            "billing::tax::apply",
            location("src/billing/tax.rs", 40),
            ImpactClass::Adaptation,
            Confidence::Definite,
            Vec::new(),
        );
        let analysis = analysis_of(Vec::new(), vec![bare]);

        assert_eq!(path_summary(&analysis.items[0], &analysis), "");
        let text = render_analysis(&analysis, &AnalysisOptions::default(), &Style::default());
        assert!(text.contains("NEEDS EDITING (1)"), "{text}");

        let explained = render_explain(&analysis, "apply", &Style::default())
            .expect("the item is in the result");
        assert!(
            explained.starts_with(
                "billing::tax::apply
"
            ),
            "{explained}"
        );
    }

    #[test]
    fn a_path_whose_change_is_missing_is_summarised_as_nothing() {
        let orphan = item(
            "billing::tax::apply",
            location("src/billing/tax.rs", 40),
            ImpactClass::Behaviour,
            Confidence::Possible,
            vec![evidence(
                9,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                Vec::new(),
                0.1,
            )],
        );
        let analysis = analysis_of(Vec::new(), vec![orphan]);

        assert_eq!(path_summary(&analysis.items[0], &analysis), "");
    }

    #[test]
    fn truncation_marks_the_end_of_the_kept_text() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abcdef", 1), "…");
        assert_eq!(truncate("abcdef", 0), "");
        assert_eq!(truncate("héllo wörld", 4), "hél…");
    }

    #[test]
    fn padding_counts_characters_not_bytes() {
        assert_eq!(pad("é", 3), "é  ");
        assert_eq!(pad("abc", 2), "abc");
        assert_eq!(spaces(0), "");
        assert_eq!(spaces(2).len(), 2);
    }

    #[test]
    fn counted_nouns_agree_with_their_number() {
        assert_eq!(count_noun(1, "item", "items"), "item");
        assert_eq!(count_noun(0, "item", "items"), "items");
        assert_eq!(count_noun(2, "item", "items"), "items");
    }

    #[test]
    fn a_three_step_path_counts_three_steps() {
        let deep = item(
            "billing::report::summarise",
            location("src/billing/report.rs", 77),
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![evidence(
                0,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                vec![
                    step(
                        "billing::tax::apply",
                        "billing::tax::vat_rate",
                        EdgeKind::Calls,
                        "src/billing/tax.rs",
                        40,
                    ),
                    step(
                        "billing::report::build",
                        "billing::tax::apply",
                        EdgeKind::Calls,
                        "src/billing/report.rs",
                        50,
                    ),
                    step(
                        "billing::report::summarise",
                        "billing::report::build",
                        EdgeKind::Calls,
                        "src/billing/report.rs",
                        77,
                    ),
                ],
                0.3,
            )],
        );
        let analysis = analysis_of(
            vec![change(
                ChangeKind::Body,
                "billing::tax::vat_rate",
                "src/billing/tax.rs",
                15,
            )],
            vec![deep],
        );

        assert_eq!(
            path_summary(&analysis.items[0], &analysis),
            "3 steps from vat_rate"
        );
        assert_eq!(analysis.items[0].distance, 3);
    }
}
