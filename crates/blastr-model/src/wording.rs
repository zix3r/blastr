//! The words every front end uses for a result.
//!
//! The window and a later agent interface describe the same analysis, so the
//! short phrases live here once: how a symbol path is shortened, what a
//! reason, a limit or a test target is called, and where an item is.
//! Everything is a pure function of the data it is given.

use crate::{
    Analysis, AnalysisOptions, Certainty, Change, ChangeDetail, ChangeKind, Delta, EdgeKind,
    EvidencePath, EvidenceStep, ImpactClass, ImpactItem, ImpactTags, Limit, Location, Reason,
    TargetKind, TestCommand, TestReason, Visibility,
};

/// Widest a symbol may get before its path is shortened from the left.
pub const SYMBOL_WIDTH: usize = 64;

/// What is said about a path that runs through an uncertain step.
pub const CANDIDATE_PATH_LIMIT: &str = "this path goes through a step that could not be resolved \
                                        to one target, so the item may not be affected.";

/// Shortens a symbol path to at most [`SYMBOL_WIDTH`] characters.
///
/// The end of a path says the most, so the function first simplifies the
/// `<impl …>` segments of a long path, then replaces leading module names with
/// a `…::` marker, and only cuts characters as a last resort.
pub fn shorten(symbol: &str) -> String {
    if symbol.chars().count() <= SYMBOL_WIDTH {
        return symbol.to_owned();
    }

    let segments: Vec<String> = path_segments(symbol)
        .into_iter()
        .map(simplified_segment)
        .collect();
    let simplified = segments.join("::");
    if simplified.chars().count() <= SYMBOL_WIDTH {
        return simplified;
    }

    // Drop leading module names one at a time. The final item name stays, and
    // so does the segment before it, which names the type the item belongs to.
    let mut shortest = simplified;
    if segments.len() > 2 {
        for start in 1..=segments.len() - 2 {
            let candidate = format!("…::{}", segments[start..].join("::"));
            if candidate.chars().count() <= SYMBOL_WIDTH {
                return candidate;
            }
            shortest = candidate;
        }
    }

    // Nothing fits: cut characters from the left, keeping the end.
    let body = shortest.strip_prefix('…').unwrap_or(&shortest);
    let keep = SYMBOL_WIDTH.saturating_sub(1);
    let length = body.chars().count();
    let tail: String = body.chars().skip(length.saturating_sub(keep)).collect();
    format!("…{tail}")
}

/// Splits a path at the `::` separators that are not inside angle brackets.
///
/// An `<impl …>` segment holds separators of its own, which must stay inside
/// that segment.
pub fn path_segments(path: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let bytes = path.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => {
                segments.push(&path[start..index]);
                index += 2;
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    segments.push(&path[start..]);
    segments
}

/// A symbol path split for compact display: the part a reader recognises,
/// and the module it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortName {
    /// The item's name, prefixed by the type it belongs to when there is one:
    /// `StreamDeserializer::next`, `Event::Pause`, `parse_whitespace`.
    pub name: String,
    /// The module path before it, without generics: `serde_json::de`. Empty
    /// for an item at the crate root of a one-segment path.
    pub module: String,
}

/// Splits `path` into the short name a list row shows and the module shown
/// beside it in a quieter colour.
///
/// Generic arguments are dropped and an `<impl Trait for Type>` segment is
/// replaced by `Type`, so `serde_json::de::<impl Iterator for
/// StreamDeserializer<'de, R, T>>::next` becomes `StreamDeserializer::next`
/// in `serde_json::de`. An associated item of a type keeps the type in front:
/// `de::Deserializer::parse_whitespace` becomes `Deserializer::parse_whitespace`
/// in `de`.
pub fn short_name(path: &str) -> ShortName {
    let segments = path_segments(path);
    let Some(last) = segments.len().checked_sub(1) else {
        return ShortName {
            name: path.to_owned(),
            module: String::new(),
        };
    };
    let clean = |segment: &str| strip_generics(segment).trim().to_owned();

    // The last `<impl …>` segment that has items after it names the owner.
    if let Some(at) = (0..last)
        .rev()
        .find(|&index| segments[index].starts_with("<impl "))
    {
        let owner = impl_owner(segments[at]);
        let rest: Vec<String> = segments[at + 1..].iter().map(|s| clean(s)).collect();
        let module: Vec<String> = segments[..at].iter().map(|s| clean(s)).collect();
        return ShortName {
            name: format!("{owner}::{}", rest.join("::")),
            module: module.join("::"),
        };
    }

    let name = clean(segments[last]);
    let owner = last
        .checked_sub(1)
        .map(|index| clean(segments[index]))
        .filter(|segment| segment.chars().next().is_some_and(char::is_uppercase));
    let module_end = if owner.is_some() { last - 1 } else { last };
    let module: Vec<String> = segments[..module_end].iter().map(|s| clean(s)).collect();
    ShortName {
        name: match owner {
            Some(owner) => format!("{owner}::{name}"),
            None => name,
        },
        module: module.join("::"),
    }
}

/// The type an `<impl …>` segment is for: `<impl Iterator for
/// StreamDeserializer<'de, R, T>>` gives `StreamDeserializer`, `<impl Foo>`
/// gives `Foo`, and a reference type loses its `&` and `mut`.
fn impl_owner(segment: &str) -> String {
    let inner = segment
        .strip_prefix("<impl ")
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(segment);
    let inner = strip_generics(inner);
    let target = match inner.rfind(" for ") {
        Some(at) => &inner[at + " for ".len()..],
        None => inner.as_str(),
    };
    let target = target.trim().trim_start_matches('&').trim();
    let target = target.strip_prefix("mut ").unwrap_or(target).trim();
    target.rsplit("::").next().unwrap_or(target).to_owned()
}

/// Simplifies one path segment.
///
/// An `<impl …>` segment keeps its brackets but loses the generic arguments
/// and the module paths of the trait and the type it names.
fn simplified_segment(segment: &str) -> String {
    let Some(rest) = segment.strip_prefix("<impl ") else {
        return segment.to_owned();
    };
    let Some(inner) = rest.strip_suffix('>') else {
        return segment.to_owned();
    };
    let inner = strip_generics(inner);
    let words: Vec<&str> = inner
        .split_whitespace()
        .map(|word| word.rsplit("::").next().unwrap_or(word))
        .collect();
    format!("<impl {}>", words.join(" "))
}

/// Removes generic argument lists from text, counting nested brackets.
fn strip_generics(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for character in text.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    out
}

/// One change as three cells: what happened, to what, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRow {
    /// The kind of change, with a member marker when there is one.
    pub label: String,
    /// The symbol path, with the extra detail the kind needs.
    pub symbol: String,
    /// `file:line`, empty when the change has no location.
    pub location: String,
}

/// The kind of a change with its member marker: `variant +`, `field -`.
pub fn change_label(change: &Change) -> String {
    let mut label = change.kind.as_str().to_owned();
    if let ChangeDetail::Member(delta) = &change.detail {
        label.push(' ');
        label.push(match delta {
            Delta::Added => '+',
            Delta::Removed => '-',
            Delta::Modified => '~',
        });
    }
    label
}

/// Builds the row for one change.
pub fn change_row(change: &Change) -> ChangeRow {
    let mut symbol = match (&change.kind, &change.detail) {
        (ChangeKind::Manifest, ChangeDetail::Manifest(text)) => text.clone(),
        _ => change.symbol.path.clone(),
    };
    match &change.detail {
        ChangeDetail::Renamed { from } => {
            let from = &from.path;
            symbol.push_str(&format!(" (was {from})"));
        }
        ChangeDetail::Visibility { from, to } => {
            let from = visibility_text(from);
            let to = visibility_text(to);
            symbol.push_str(&format!(" ({from} → {to})"));
        }
        _ => {}
    }

    ChangeRow {
        label: change_label(change),
        symbol,
        location: location_text(change),
    }
}

/// Where a change is written: the head side, or the base side for a removal.
pub fn location_text(change: &Change) -> String {
    let Some(location) = change.new.as_ref().or(change.old.as_ref()) else {
        return String::new();
    };
    site_text(location)
}

/// How a visibility is spelled in output.
pub fn visibility_text(visibility: &Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "private",
        Visibility::Restricted => "restricted",
        Visibility::Crate => "crate",
        Visibility::Public => "public",
    }
}

/// How the analysed features are spelled in a header.
pub fn features_text(options: &AnalysisOptions) -> String {
    if options.all_features {
        return "all".to_owned();
    }
    if options.features.is_empty() {
        return "default".to_owned();
    }
    options.features.join(", ")
}

/// Describes one limit of a result.
pub fn limit_text(limit: &Limit) -> String {
    match limit {
        Limit::ChangedButInactive(location) => {
            let file = &location.file;
            let line = location.span.start_line;
            format!("change in inactive code at {file}:{line}")
        }
        Limit::Restricted => "build scripts and procedural macros were not run".to_owned(),
        Limit::Submodules => "submodules are not analysed".to_owned(),
        Limit::Lfs => "Git LFS content is not analysed".to_owned(),
        Limit::Unresolved { count } => {
            let noun = if *count == 1 {
                "reference"
            } else {
                "references"
            };
            format!("{count} unresolved {noun}")
        }
        Limit::MacrosNotExpanded { crates } => {
            let names = crates.join(", ");
            format!(
                "procedural macros of {names} could not be loaded; code they generate is missing \
                 from the analysis"
            )
        }
    }
}

/// What limits an analysis, one short phrase per limit.
///
/// Empty when nothing is worth saying.
pub fn limit_parts(analysis: &Analysis) -> Vec<String> {
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
            other => parts.push(limit_text(other)),
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
    parts
}

/// The limits sentence of a report, if anything is worth saying.
pub fn limits_text(analysis: &Analysis) -> Option<String> {
    let parts = limit_parts(analysis);
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// How an impact class is shown to people.
pub fn class_text(class: ImpactClass) -> &'static str {
    match class {
        ImpactClass::Adaptation => "needs editing",
        ImpactClass::Behaviour => "may behave differently",
    }
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
        Reason::ParameterBecomesUnused => {
            format!("drops the parameter only field {name} used")
        }
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
pub fn best_path(item: &ImpactItem) -> Option<&EvidencePath> {
    item.paths
        .iter()
        .find(|path| path.class == item.class)
        .or_else(|| item.paths.first())
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

/// What one step of an evidence path does, in a few words.
///
/// The first step spells out the rule that reached it; later steps name the
/// reference. An uncertain step is marked as a candidate.
pub fn step_text(
    step: &EvidenceStep,
    path: &EvidencePath,
    analysis: &Analysis,
    first: bool,
) -> String {
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
pub fn edge_phrase(edge: EdgeKind) -> &'static str {
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

/// The tags of an item as words, in a fixed order.
pub fn tag_names(tags: &ImpactTags) -> Vec<&'static str> {
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

/// How a selected test's reason reads.
pub fn test_reason_text(reason: TestReason) -> &'static str {
    match reason {
        TestReason::Changed => "changed",
        TestReason::Reached => "reached",
    }
}

/// `file:line` of a location.
pub fn site_text(location: &Location) -> String {
    let file = &location.file;
    let line = location.span.start_line;
    format!("{file}:{line}")
}

/// The singular or plural form of a counted noun.
pub fn count_noun(count: u32, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

#[cfg(test)]
mod short_name_tests {
    use super::*;

    fn split(path: &str) -> (String, String) {
        let short = short_name(path);
        (short.name, short.module)
    }

    #[test]
    fn an_impl_segment_is_replaced_by_its_type() {
        assert_eq!(
            split("serde_json::de::<impl Iterator for StreamDeserializer<'de, R, T>>::next"),
            ("StreamDeserializer::next".into(), "serde_json::de".into())
        );
        assert_eq!(
            split(
                "serde_json::de::<impl de::Deserializer<'de> for &mut Deserializer<R>>::deserialize_any"
            ),
            (
                "Deserializer::deserialize_any".into(),
                "serde_json::de".into()
            )
        );
        assert_eq!(
            split("billing::<impl Invoice>::total"),
            ("Invoice::total".into(), "billing".into())
        );
    }

    #[test]
    fn an_associated_item_keeps_its_type() {
        assert_eq!(
            split("serde_json::de::Deserializer::parse_whitespace"),
            (
                "Deserializer::parse_whitespace".into(),
                "serde_json::de".into()
            )
        );
        assert_eq!(
            split("billing::PaymentState::Refunded"),
            ("PaymentState::Refunded".into(), "billing".into())
        );
    }

    #[test]
    fn a_free_item_keeps_only_its_name() {
        assert_eq!(
            split("billing::tax::vat_rate"),
            ("vat_rate".into(), "billing::tax".into())
        );
        assert_eq!(split("main"), ("main".into(), String::new()));
        assert_eq!(
            split("stream::test_error"),
            ("test_error".into(), "stream".into())
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::{RelPath, Span, SymbolKind, SymbolPath};

    use super::*;

    fn change(kind: ChangeKind, path: &str, detail: ChangeDetail) -> Change {
        Change {
            kind,
            symbol: SymbolPath::new(path, SymbolKind::Function),
            old: None,
            new: Some(Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 1,
                    start_line: 7,
                    end_line: 7,
                },
            }),
            detail,
        }
    }

    #[test]
    fn short_paths_are_left_alone_and_long_ones_keep_their_end() {
        assert_eq!(shorten("billing::tax::vat_rate"), "billing::tax::vat_rate");
        let long = format!("crate::{}tail", "segment::".repeat(12));
        let short = shorten(&long);
        assert!(short.chars().count() <= SYMBOL_WIDTH, "{short}");
        assert!(short.starts_with("…::"), "{short}");
        assert!(short.ends_with("::tail"), "{short}");
    }

    #[test]
    fn segments_do_not_split_inside_angle_brackets() {
        assert_eq!(
            path_segments("a::<impl b::T for c::S>::run"),
            vec!["a", "<impl b::T for c::S>", "run"]
        );
    }

    #[test]
    fn a_change_row_carries_the_member_marker_and_the_location() {
        let row = change_row(&change(
            ChangeKind::Variant,
            "billing::State::Refunded",
            ChangeDetail::Member(Delta::Added),
        ));
        assert_eq!(row.label, "variant +");
        assert_eq!(row.symbol, "billing::State::Refunded");
        assert_eq!(row.location, "src/lib.rs:7");
    }

    #[test]
    fn classes_use_the_words_shown_to_people() {
        assert_eq!(class_text(ImpactClass::Adaptation), "needs editing");
        assert_eq!(class_text(ImpactClass::Behaviour), "may behave differently");
    }

    #[test]
    fn a_reason_names_the_changed_thing_by_its_last_segment() {
        let body = change(
            ChangeKind::Body,
            "billing::tax::vat_rate",
            ChangeDetail::None,
        );
        assert_eq!(reason_text(Reason::CallsChanged, &body), "calls vat_rate");
    }
}
