//! Rendering of results for the command line.
//!
//! Everything here is a pure function of the data it is given, so the
//! formatting can be tested without a repository or a cache.

use blastr_core::CacheStatus;
use blastr_model::{
    AnalysisOptions, Change, ChangeDetail, ChangeKind, Delta, DiffResult, Limit, Visibility,
};

/// The schema version the JSON output carries.
pub const SCHEMA_VERSION: u32 = 1;

/// The width the labels of the cache listing are padded to.
const LABEL_WIDTH: usize = 12;

/// Widest the symbol column may get before paths are shortened from the left.
const SYMBOL_WIDTH: usize = 64;

/// How many changes the human-readable diff lists before summarising the rest.
const MAX_LISTED: usize = 20;

/// Renders a diff result as the human-readable report.
///
/// `list_all` lifts the cap on the number of listed changes.
pub fn render_diff(result: &DiffResult, options: &AnalysisOptions, list_all: bool) -> String {
    let listed: Vec<&Change> = result
        .changes
        .iter()
        .filter(|change| change.kind != ChangeKind::NonSemantic)
        .collect();
    let hidden = result.changes.len() - listed.len();

    let mut blocks = Vec::new();
    let base = &result.comparison.base;
    let head = &result.comparison.head;
    let features = features_text(options);
    blocks.push(format!("Comparing {base} → {head} · features: {features}"));
    blocks.push(changes_block(&listed, list_all));
    if !result.limits.is_empty() {
        let items: Vec<String> = result.limits.iter().map(limit_text).collect();
        let items = items.join(" · ");
        blocks.push(format!("LIMITS  {items}"));
    }
    if hidden > 0 {
        let noun = if hidden == 1 { "change" } else { "changes" };
        blocks.push(format!("{hidden} formatting-only {noun} not shown"));
    }

    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

/// Renders the `CHANGES` section, with its columns aligned.
fn changes_block(changes: &[&Change], list_all: bool) -> String {
    let count = changes.len();
    if changes.is_empty() {
        return format!("CHANGES ({count})\n  no structural changes");
    }

    let shown = if list_all || count <= MAX_LISTED {
        count
    } else {
        MAX_LISTED
    };
    let rows: Vec<Row> = changes.iter().take(shown).copied().map(row).collect();
    let label_width = rows
        .iter()
        .map(|row| row.label.chars().count())
        .max()
        .unwrap_or(0);
    let symbol_width = rows
        .iter()
        .map(|row| shorten(&row.symbol).chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!("CHANGES ({count})");
    for Row {
        label,
        symbol,
        location,
    } in rows
    {
        out.push('\n');
        out.push_str("  ");
        let symbol = shorten(&symbol);
        if location.is_empty() {
            // No third column: do not leave the padding as trailing spaces.
            let line = format!("{label:<label_width$}  {symbol}");
            out.push_str(line.trim_end());
        } else {
            out.push_str(&format!(
                "{label:<label_width$}  {symbol:<symbol_width$}  {location}"
            ));
        }
    }
    if shown < count {
        out.push_str(&format!("\n  … and {} more (use --all)", count - shown));
    }
    out
}

/// Shortens a symbol path to at most [`SYMBOL_WIDTH`] characters, keeping
/// the end, which is the most specific part.
fn shorten(symbol: &str) -> String {
    let length = symbol.chars().count();
    if length <= SYMBOL_WIDTH {
        return symbol.to_owned();
    }
    let keep = SYMBOL_WIDTH - 1;
    let tail: String = symbol.chars().skip(length - keep).collect();
    format!("…{tail}")
}

/// One rendered line of the `CHANGES` section.
struct Row {
    /// The kind of change, with a member marker when there is one.
    label: String,
    /// The symbol path, with the extra detail the kind needs.
    symbol: String,
    /// `file:line`, empty when the change has no location.
    location: String,
}

/// Builds the row for one change.
fn row(change: &Change) -> Row {
    let mut label = change.kind.as_str().to_owned();
    if let ChangeDetail::Member(delta) = &change.detail {
        label.push(' ');
        label.push(match delta {
            Delta::Added => '+',
            Delta::Removed => '-',
            Delta::Modified => '~',
        });
    }

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

    Row {
        label,
        symbol,
        location: location_text(change),
    }
}

/// Where a change is written: the head side, or the base side for a removal.
fn location_text(change: &Change) -> String {
    let Some(location) = change.new.as_ref().or(change.old.as_ref()) else {
        return String::new();
    };
    let file = &location.file;
    let line = location.span.start_line;
    format!("{file}:{line}")
}

/// How a visibility is spelled in output.
fn visibility_text(visibility: &Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "private",
        Visibility::Restricted => "restricted",
        Visibility::Crate => "crate",
        Visibility::Public => "public",
    }
}

/// How the analysis features are spelled in the header.
fn features_text(options: &AnalysisOptions) -> String {
    if options.all_features {
        return "all".to_owned();
    }
    if options.features.is_empty() {
        return "default".to_owned();
    }
    options.features.join(", ")
}

/// Describes one limit of a result.
fn limit_text(limit: &Limit) -> String {
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
    }
}

/// Renders a diff result as pretty JSON, with `schema_version` first.
pub fn render_json(result: &DiffResult) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(result)?;
    let body = serde_json::to_string_pretty(&value)?;
    // `serde_json` keeps object keys sorted in a `BTreeMap`, so inserting the
    // version cannot put it first. Render the body and prepend the version to
    // the text instead.
    let inner = body
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(body.as_str());
    if inner.is_empty() {
        return Ok(format!("{{\n  \"schema_version\": {SCHEMA_VERSION}\n}}\n"));
    }
    let mut out = format!("{{\n  \"schema_version\": {SCHEMA_VERSION},");
    out.push_str(inner);
    out.push_str("}\n");
    Ok(out)
}

/// Renders what the cache contains.
pub fn render_cache_status(status: &CacheStatus) -> String {
    let mut out = String::from("CACHE\n");
    labelled(
        &mut out,
        "graphs",
        &graph_count(status.graphs, status.graph_bytes),
    );
    labelled(
        &mut out,
        "snapshots",
        &graph_count(status.snapshots, status.snapshot_bytes),
    );
    labelled(&mut out, "database", &status.database.display().to_string());
    labelled(
        &mut out,
        "snapshot dir",
        &status.snapshots_dir.display().to_string(),
    );
    out
}

/// Renders what `cache clear` removed.
pub fn render_cache_cleared(removed: &CacheStatus) -> String {
    let mut out = String::from("CACHE CLEARED\n");
    labelled(
        &mut out,
        "graphs",
        &graph_count(removed.graphs, removed.graph_bytes),
    );
    labelled(
        &mut out,
        "snapshots",
        &graph_count(removed.snapshots, removed.snapshot_bytes),
    );
    out
}

/// Number of entries and their size, in one cell.
fn graph_count(count: u64, bytes: u64) -> String {
    let size = format_size(bytes);
    format!("{count} · {size}")
}

/// Appends one `label  value` line, with the labels aligned.
fn labelled(out: &mut String, label: &str, value: &str) {
    // At least one space, so that a label as wide as the column still separates.
    let padding = LABEL_WIDTH.saturating_sub(label.len()).max(1);
    out.push_str("  ");
    out.push_str(label);
    for _ in 0..padding {
        out.push(' ');
    }
    out.push_str(value);
    out.push('\n');
}

/// Formats a byte count in powers of 1000 with one decimal above 999 bytes.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    let unit = UNITS[unit];
    format!("{value:.1} {unit}")
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        ChangeKind, Comparison, Location, RelPath, Source, Span, SymbolKind, SymbolPath,
    };

    use super::*;

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

    fn symbol_path(path: &str) -> SymbolPath {
        SymbolPath::new(path, SymbolKind::Function)
    }

    fn comparison() -> Comparison {
        Comparison {
            base: Source::Tree {
                id: "0f2a91c000000000000000000000000000000000".to_owned(),
                label: "4f2a91c".to_owned(),
            },
            head: Source::Tree {
                id: "0be07d3000000000000000000000000000000000".to_owned(),
                label: "8be07d3".to_owned(),
            },
        }
    }

    fn result_with(changes: Vec<Change>, limits: Vec<Limit>) -> DiffResult {
        DiffResult {
            comparison: comparison(),
            base_key: None,
            head_key: None,
            changed_files: Vec::new(),
            changes,
            limits,
        }
    }

    fn change(
        kind: ChangeKind,
        path: &str,
        detail: ChangeDetail,
        old: Option<Location>,
        new: Option<Location>,
    ) -> Change {
        Change {
            kind,
            symbol: symbol_path(path),
            old,
            new,
            detail,
        }
    }

    #[test]
    fn many_changes_are_capped_and_the_rest_counted() {
        let changes: Vec<Change> = (0..MAX_LISTED + 5)
            .map(|index| {
                change(
                    ChangeKind::Body,
                    &format!("crate::module::function_{index}"),
                    ChangeDetail::None,
                    None,
                    Some(location("src/lib.rs", index as u32 + 1)),
                )
            })
            .collect();
        let text = render_diff(
            &result_with(changes.clone(), Vec::new()),
            &AnalysisOptions::default(),
            false,
        );
        assert!(text.contains(&format!("CHANGES ({})", MAX_LISTED + 5)));
        assert!(text.contains("  … and 5 more (use --all)"));
        assert_eq!(text.matches("  body  ").count(), MAX_LISTED);

        let all = render_diff(
            &result_with(changes, Vec::new()),
            &AnalysisOptions::default(),
            true,
        );
        assert!(!all.contains("more (use --all)"));
        assert_eq!(all.matches("  body  ").count(), MAX_LISTED + 5);
    }

    #[test]
    fn long_symbols_are_shortened_from_the_left() {
        let long = format!("crate::{}::tail", "segment::".repeat(12));
        let text = render_diff(
            &result_with(
                vec![change(
                    ChangeKind::Body,
                    &long,
                    ChangeDetail::None,
                    None,
                    Some(location("src/lib.rs", 3)),
                )],
                Vec::new(),
            ),
            &AnalysisOptions::default(),
            true,
        );
        assert!(!text.contains(&long), "the full path must not be printed");
        let shortened: String = long
            .chars()
            .skip(long.chars().count() - (SYMBOL_WIDTH - 1))
            .collect();
        assert!(text.contains(&format!("…{shortened}")));
        for line in text.lines().skip(2) {
            let symbol_column = line.split("  ").nth(2).unwrap_or_default();
            assert!(
                symbol_column.chars().count() <= SYMBOL_WIDTH,
                "symbol column is {} wide: {line}",
                symbol_column.chars().count()
            );
        }
    }

    #[test]
    fn several_kinds_of_change_are_rendered() {
        let result = result_with(
            vec![
                change(
                    ChangeKind::Signature,
                    "billing::discount::apply_discount",
                    ChangeDetail::None,
                    Some(location("src/billing/discount.rs", 38)),
                    Some(location("src/billing/discount.rs", 41)),
                ),
                change(
                    ChangeKind::Variant,
                    "billing::PaymentState::Refunded",
                    ChangeDetail::Member(Delta::Added),
                    None,
                    Some(location("src/billing/payment.rs", 8)),
                ),
                change(
                    ChangeKind::Body,
                    "billing::tax::vat_rate",
                    ChangeDetail::None,
                    Some(location("src/billing/tax.rs", 10)),
                    Some(location("src/billing/tax.rs", 15)),
                ),
                change(
                    ChangeKind::Renamed,
                    "billing::tax::rate",
                    ChangeDetail::Renamed {
                        from: symbol_path("billing::tax::vat"),
                    },
                    Some(location("src/billing/tax.rs", 2)),
                    Some(location("src/billing/tax.rs", 15)),
                ),
                change(
                    ChangeKind::Visibility,
                    "billing::Tax",
                    ChangeDetail::Visibility {
                        from: Visibility::Private,
                        to: Visibility::Public,
                    },
                    Some(location("src/billing/tax.rs", 1)),
                    Some(location("src/billing/tax.rs", 1)),
                ),
                change(
                    ChangeKind::Manifest,
                    "Cargo.toml",
                    ChangeDetail::Manifest("feature `fast` removed".to_owned()),
                    Some(location("Cargo.toml", 3)),
                    Some(location("Cargo.toml", 3)),
                ),
            ],
            Vec::new(),
        );

        let text = render_diff(&result, &AnalysisOptions::default(), true);
        assert!(
            text.starts_with("Comparing 4f2a91c → 8be07d3 · features: default\n\n"),
            "{text}"
        );
        assert!(text.contains("CHANGES (6)\n"), "{text}");
        assert!(
            text.contains("  signature   billing::discount::apply_discount"),
            "{text}"
        );
        assert!(
            text.contains("  variant +   billing::PaymentState::Refunded"),
            "{text}"
        );
        assert!(
            text.contains("  body        billing::tax::vat_rate"),
            "{text}"
        );
        assert!(
            text.contains("  renamed     billing::tax::rate (was billing::tax::vat)"),
            "{text}"
        );
        assert!(
            text.contains("  visibility  billing::Tax (private → public)"),
            "{text}"
        );
        assert!(
            text.contains("  manifest    feature `fast` removed"),
            "{text}"
        );
        assert!(text.contains("src/billing/discount.rs:41\n"), "{text}");
        assert!(text.contains("src/billing/payment.rs:8\n"), "{text}");
        assert!(!text.contains("inactive"), "{text}");
        assert!(!text.contains("LIMITS"), "{text}");
    }

    #[test]
    fn columns_are_aligned_with_two_spaces() {
        let result = result_with(
            vec![
                change(
                    ChangeKind::Body,
                    "a::b",
                    ChangeDetail::None,
                    None,
                    Some(location("src/a.rs", 3)),
                ),
                change(
                    ChangeKind::Variant,
                    "a::Long::Variant",
                    ChangeDetail::Member(Delta::Added),
                    None,
                    Some(location("src/c.rs", 9)),
                ),
            ],
            Vec::new(),
        );

        let text = render_diff(&result, &AnalysisOptions::default(), true);
        let lines: Vec<&str> = text.lines().collect();
        let first = lines[3];
        let second = lines[4];
        assert!(first.starts_with("  "), "{first}");
        let first_symbol = first.find("a::b").expect("the symbol is printed");
        let second_symbol = second
            .find("a::Long::Variant")
            .expect("the symbol is printed");
        assert_eq!(first_symbol, second_symbol, "symbol columns differ");
        assert_eq!(
            first_symbol,
            2 + "variant +".len() + 2,
            "label column width"
        );
        let first_location = first.find("src/a.rs:3").expect("the location is printed");
        let second_location = second.find("src/c.rs:9").expect("the location is printed");
        assert_eq!(first_location, second_location, "location columns differ");
        assert_eq!(
            second_location,
            second_symbol + "a::Long::Variant".len() + 2
        );
    }

    #[test]
    fn removed_symbols_use_the_base_location() {
        let result = result_with(
            vec![change(
                ChangeKind::Removed,
                "a::gone",
                ChangeDetail::None,
                Some(location("src/old.rs", 7)),
                None,
            )],
            Vec::new(),
        );

        let text = render_diff(&result, &AnalysisOptions::default(), true);
        assert!(text.contains("  removed  a::gone  src/old.rs:7"), "{text}");
    }

    #[test]
    fn empty_result_says_there_were_no_changes() {
        let result = result_with(Vec::new(), Vec::new());
        let text = render_diff(&result, &AnalysisOptions::default(), true);
        assert!(
            text.contains("CHANGES (0)\n  no structural changes\n"),
            "{text}"
        );
        assert!(!text.contains("not shown"), "{text}");
    }

    #[test]
    fn non_semantic_changes_are_hidden_and_counted() {
        let result = result_with(
            vec![
                change(
                    ChangeKind::NonSemantic,
                    "a::one",
                    ChangeDetail::None,
                    Some(location("src/a.rs", 1)),
                    Some(location("src/a.rs", 1)),
                ),
                change(
                    ChangeKind::NonSemantic,
                    "a::two",
                    ChangeDetail::None,
                    Some(location("src/a.rs", 9)),
                    Some(location("src/a.rs", 9)),
                ),
                change(
                    ChangeKind::Body,
                    "a::three",
                    ChangeDetail::None,
                    Some(location("src/b.rs", 4)),
                    Some(location("src/b.rs", 5)),
                ),
            ],
            Vec::new(),
        );

        let text = render_diff(&result, &AnalysisOptions::default(), true);
        assert!(text.contains("CHANGES (1)\n"), "{text}");
        assert!(!text.contains("a::one"), "{text}");
        assert!(
            text.contains("2 formatting-only changes not shown"),
            "{text}"
        );
        assert!(
            text.ends_with("2 formatting-only changes not shown\n"),
            "{text}"
        );
    }

    #[test]
    fn one_non_semantic_change_is_counted_in_the_singular() {
        let result = result_with(
            vec![change(
                ChangeKind::NonSemantic,
                "a::one",
                ChangeDetail::None,
                None,
                Some(location("src/a.rs", 1)),
            )],
            Vec::new(),
        );

        let text = render_diff(&result, &AnalysisOptions::default(), true);
        assert!(
            text.contains("CHANGES (0)\n  no structural changes\n"),
            "{text}"
        );
        assert!(
            text.contains("1 formatting-only change not shown"),
            "{text}"
        );
    }

    #[test]
    fn limits_are_listed_after_the_changes() {
        let result = result_with(
            Vec::new(),
            vec![
                Limit::Unresolved { count: 2 },
                Limit::Restricted,
                Limit::Submodules,
                Limit::Lfs,
                Limit::ChangedButInactive(location("src/a.rs", 4)),
            ],
        );

        let text = render_diff(&result, &AnalysisOptions::default(), true);
        assert!(
            text.contains(
                "LIMITS  2 unresolved references · build scripts and procedural macros were not run \
                 · submodules are not analysed · Git LFS content is not analysed · change in \
                 inactive code at src/a.rs:4\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn header_shows_the_features() {
        let result = result_with(Vec::new(), Vec::new());

        assert!(
            render_diff(&result, &AnalysisOptions::default(), true).contains("features: default\n"),
            "default features"
        );

        let listed = AnalysisOptions {
            features: vec!["a".to_owned(), "b".to_owned()],
            ..AnalysisOptions::default()
        };
        assert!(
            render_diff(&result, &listed, true).contains("features: a, b\n"),
            "listed features"
        );

        let all = AnalysisOptions {
            all_features: true,
            ..AnalysisOptions::default()
        };
        assert!(
            render_diff(&result, &all, true).contains("features: all\n"),
            "all features"
        );
    }

    #[test]
    fn sources_are_shown_by_label() {
        let result = DiffResult {
            comparison: Comparison {
                base: Source::Tree {
                    id: "aaaa".to_owned(),
                    label: "4f2a91c".to_owned(),
                },
                head: Source::Worktree,
            },
            base_key: None,
            head_key: None,
            changed_files: Vec::new(),
            changes: Vec::new(),
            limits: Vec::new(),
        };

        let text = render_diff(&result, &AnalysisOptions::default(), true);
        assert!(
            text.starts_with("Comparing 4f2a91c → working tree · features: default\n"),
            "{text}"
        );
    }

    #[test]
    fn json_has_the_schema_version_first() {
        let result = result_with(
            vec![change(
                ChangeKind::Body,
                "a::b",
                ChangeDetail::None,
                Some(location("src/a.rs", 3)),
                Some(location("src/a.rs", 4)),
            )],
            vec![Limit::Unresolved { count: 1 }],
        );

        let text = render_json(&result).expect("the result serialises");
        assert!(
            text.starts_with("{\n  \"schema_version\": 1,\n"),
            "the schema version is not first: {text}"
        );
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(value["schema_version"], serde_json::json!(1));
        assert_eq!(value["changes"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["changes"][0]["kind"], serde_json::json!("body"));
        assert!(text.ends_with("}\n"), "{text}");
    }

    #[test]
    fn sizes_use_powers_of_a_thousand() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1000), "1.0 kB");
        assert_eq!(format_size(12_345), "12.3 kB");
        assert_eq!(format_size(4_500_000), "4.5 MB");
        assert_eq!(format_size(1_200_000_000), "1.2 GB");
        assert_eq!(format_size(2_000_000_000_000), "2.0 TB");
    }
}
