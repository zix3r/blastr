//! The answers in the shape agents read: a short text and the same data as
//! JSON.
//!
//! The words come from [`blastr_model::wording`], so an agent and the window
//! describe a result alike. Every answer drawn from an analysis carries the
//! comparison and the limits.

use std::fmt::Write as _;

use blastr_model::bridge::{Status, SymbolHit};
use blastr_model::wording::{
    best_path, change_note, change_row, class_text, explanation, limit_parts, path_summary,
    site_text, step_text, tag_names, target_label, test_reason_text,
};
use blastr_model::{
    Analysis, Change, ChangeKind, Confidence, ImpactClass, ImpactItem, SelectedTest, SymbolPath,
    TestCommand, TestReason,
};
use serde_json::{Value, json};

/// How many items a summary lists.
const TOP_ITEMS: usize = 10;

/// How many changes a summary lists.
const TOP_CHANGES: usize = 20;

/// How many changed files a summary names.
const TOP_FILES: usize = 5;

/// How many items of each class the explanation of a change lists.
const TOP_REACHED: usize = 5;

/// How many selected tests an answer lists.
const MAX_TESTS: usize = 100;

/// A text for the model and the same data as JSON.
pub struct Shaped {
    /// What the agent reads.
    pub text: String,
    /// The data, as `structuredContent`.
    pub data: Value,
}

/// What the window has open, for `status` and `blastr://status`.
pub fn status(status: &Status) -> Shaped {
    let mut text = String::new();
    match &status.repository {
        None => text.push_str("Blastr is open on its start screen; no repository is open.\n"),
        Some(repository) => {
            let _ = write!(text, "Repository: {}", repository.display());
            if let Some(branch) = &status.branch {
                let _ = write!(text, " (branch {branch})");
            }
            text.push('\n');
        }
    }
    if status.repository.is_some() {
        text.push_str(&comparison_line(status, None));
        text.push('\n');
    }
    if let Some(options) = &status.options {
        let mut parts = Vec::new();
        if options.all_features {
            parts.push("all features".to_owned());
        } else if !options.features.is_empty() {
            parts.push(format!("features {}", options.features.join(", ")));
        }
        if let Some(target) = &options.target {
            parts.push(format!("target {target}"));
        }
        if options.exec == blastr_model::ExecMode::Restricted {
            parts.push("restricted (build scripts and proc macros do not run)".to_owned());
        }
        if !status.ignore.is_empty() {
            parts.push(format!("ignoring {}", status.ignore.join(", ")));
        }
        if !parts.is_empty() {
            let _ = writeln!(text, "Options: {}", parts.join("; "));
        }
    }
    if let Some(failure) = &status.failure {
        let _ = writeln!(text, "Failed: {failure}");
    }
    Shaped {
        text: text.trim_end().to_owned(),
        data: serde_json::to_value(status).unwrap_or(Value::Null),
    }
}

/// The summary of the current result, for `get_analysis`, `compare`,
/// `what_if` and `blastr://analysis`.
pub fn summary(status: &Status, analysis: Option<&Analysis>) -> Shaped {
    let Some(analysis) = analysis else {
        let mut text = String::from("Nothing has been compared yet.");
        if status.repository.is_none() {
            text.push_str(" Open a repository with `open_repository`.");
        } else {
            text.push_str(" Choose a comparison with `compare`.");
        }
        if let Some(failure) = &status.failure {
            let _ = write!(text, "\nThe last run failed: {failure}");
        }
        return Shaped {
            text,
            data: json!({ "comparison": comparison_data(status, None), "analysis": null }),
        };
    };

    let mut text = comparison_line(status, Some(analysis));
    text.push('\n');

    let changes = &analysis.diff.changes;
    let files = &analysis.diff.changed_files;
    if changes.is_empty() {
        if files.is_empty() {
            text.push_str("0 changes; no files differ.\n");
        } else {
            let _ = writeln!(
                text,
                "No Rust items changed; only these files differ: {}.",
                files_text(analysis)
            );
        }
    } else {
        if !files.is_empty() {
            let _ = writeln!(
                text,
                "Files: {} changed ({})",
                files.len(),
                files_text(analysis)
            );
        }
        let _ = writeln!(text, "Changes: {}", kind_counts(changes).join(", "));
        let _ = writeln!(text, "{} {}:", changes.len(), noun(changes.len(), "change"));
        for (index, change) in changes.iter().enumerate().take(TOP_CHANGES) {
            let _ = writeln!(text, "{}", change_line(index, change));
        }
        if changes.len() > TOP_CHANGES {
            let _ = writeln!(
                text,
                "… and {} more; `list_changes` pages them.",
                changes.len() - TOP_CHANGES
            );
        }
    }

    let editing = count_class(analysis, ImpactClass::Adaptation);
    let behaviour = count_class(analysis, ImpactClass::Behaviour);
    let by_confidence = |level: Confidence| {
        analysis
            .items
            .iter()
            .filter(|item| item.confidence == level)
            .count()
    };
    let (definite, likely, possible) = (
        by_confidence(Confidence::Definite),
        by_confidence(Confidence::Likely),
        by_confidence(Confidence::Possible),
    );
    let _ = writeln!(
        text,
        "Impact: {editing} needs editing, {behaviour} may behave differently \
         ({definite} definite, {likely} likely, {possible} possible)."
    );
    let tests = &analysis.tests;
    let _ = writeln!(
        text,
        "Tests: {} of {} selected; `tests_to_run` lists the commands.",
        tests.selected.len(),
        tests.total
    );
    let top: Vec<&ImpactItem> = analysis.items.iter().take(TOP_ITEMS).collect();
    if !top.is_empty() {
        text.push_str("Top items:\n");
        for item in &top {
            text.push_str(&item_line(item, analysis));
            text.push('\n');
        }
        if analysis.items.len() > top.len() {
            let _ = writeln!(
                text,
                "… {} more; `list_impact` pages through them.",
                analysis.items.len() - top.len()
            );
        }
    }
    text.push_str(&limits_line(analysis));

    let data = json!({
        "comparison": comparison_data(status, Some(analysis)),
        "files": {
            "changed": files.len(),
            "paths": files.iter().take(TOP_FILES).map(|file| file.path.as_str()).collect::<Vec<_>>(),
        },
        "change_counts": kind_counts(changes),
        "changes": changes
            .iter()
            .enumerate()
            .take(TOP_CHANGES)
            .map(|(index, change)| change_data(index, change))
            .collect::<Vec<_>>(),
        "more_changes": changes.len().saturating_sub(TOP_CHANGES),
        "counts": {
            "needs_editing": editing,
            "may_behave_differently": behaviour,
            "definite": definite,
            "likely": likely,
            "possible": possible,
        },
        "tests": {
            "selected": tests.selected.len(),
            "total": tests.total,
            "commands": tests.commands.iter().map(TestCommand::command_line).collect::<Vec<_>>(),
        },
        "top_items": top.iter().map(|item| item_data(item, analysis)).collect::<Vec<_>>(),
        "limits": limit_parts(analysis),
    });
    Shaped { text, data }
}

/// A filter for [`impact_list`].
pub struct ImpactFilter {
    /// Only items reached from this change.
    pub change: Option<u32>,
    /// Only items of this class.
    pub class: Option<ImpactClass>,
    /// Only items at least this confident.
    pub min_confidence: Option<Confidence>,
}

/// One page of affected items, for `list_impact`.
pub fn impact_list(
    status: &Status,
    analysis: &Analysis,
    filter: &ImpactFilter,
    offset: usize,
    limit: usize,
) -> Shaped {
    let matching: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| filter.class.is_none_or(|class| item.class == class))
        .filter(|item| {
            filter
                .min_confidence
                .is_none_or(|level| item.confidence >= level)
        })
        .filter(|item| {
            filter
                .change
                .is_none_or(|change| item.paths.iter().any(|path| path.change == change))
        })
        .collect();
    let page: Vec<&ImpactItem> = matching.iter().skip(offset).take(limit).copied().collect();
    let next = (offset + page.len() < matching.len()).then_some(offset + page.len());

    let mut text = comparison_line(status, Some(analysis));
    text.push('\n');
    if page.is_empty() {
        let _ = writeln!(text, "No items here ({} match the filter).", matching.len());
    } else {
        let _ = writeln!(
            text,
            "Items {}–{} of {}:",
            offset + 1,
            offset + page.len(),
            matching.len()
        );
        for item in &page {
            text.push_str(&item_line(item, analysis));
            text.push('\n');
        }
    }
    if let Some(next) = next {
        let _ = writeln!(text, "More: call again with offset {next}.");
    }
    text.push_str(&limits_line(analysis));

    let data = json!({
        "comparison": comparison_data(status, Some(analysis)),
        "total": matching.len(),
        "offset": offset,
        "items": page.iter().map(|item| item_data(item, analysis)).collect::<Vec<_>>(),
        "next_offset": next,
        "limits": limit_parts(analysis),
    });
    Shaped { text, data }
}

/// One page of changes, for `list_changes`: those of `kind` (as
/// [`ChangeKind::as_str`] spells it) in files whose path contains `file`.
pub fn change_list(
    status: &Status,
    analysis: &Analysis,
    kind: Option<&str>,
    file: Option<&str>,
    offset: usize,
    limit: usize,
) -> Shaped {
    let matching: Vec<(usize, &Change)> = analysis
        .diff
        .changes
        .iter()
        .enumerate()
        .filter(|(_, change)| kind.is_none_or(|kind| change.kind.as_str() == kind))
        .filter(|(_, change)| {
            file.is_none_or(|file| {
                [&change.new, &change.old]
                    .into_iter()
                    .flatten()
                    .any(|location| location.file.as_str().contains(file))
            })
        })
        .collect();
    let page: Vec<(usize, &Change)> = matching.iter().skip(offset).take(limit).copied().collect();
    let next = (offset + page.len() < matching.len()).then_some(offset + page.len());

    let mut text = comparison_line(status, Some(analysis));
    text.push('\n');
    if page.is_empty() {
        let _ = writeln!(
            text,
            "No changes here ({} match the filter).",
            matching.len()
        );
    } else {
        let _ = writeln!(
            text,
            "Changes {}–{} of {}:",
            offset + 1,
            offset + page.len(),
            matching.len()
        );
        for (index, change) in &page {
            let _ = writeln!(text, "{}", change_line(*index, change));
        }
    }
    if let Some(next) = next {
        let _ = writeln!(text, "More: call again with offset {next}.");
    }
    text.push_str(&limits_line(analysis));

    let data = json!({
        "comparison": comparison_data(status, Some(analysis)),
        "total": matching.len(),
        "offset": offset,
        "changes": page.iter().map(|(index, change)| change_data(*index, change)).collect::<Vec<_>>(),
        "next_offset": next,
        "limits": limit_parts(analysis),
    });
    Shaped { text, data }
}

/// What the changes numbered `indices` are and what they reach, for
/// `explain_impact` on a changed symbol.
pub fn explain_changes(status: &Status, analysis: &Analysis, indices: &[usize]) -> Shaped {
    let mut text = comparison_line(status, Some(analysis));
    text.push('\n');
    let mut explained = Vec::new();
    for &index in indices {
        let Some(change) = analysis.diff.changes.get(index) else {
            continue;
        };
        let row = change_row(change);
        let note = change_note(change);
        let _ = writeln!(
            text,
            "Change #{index}: {} {} — {note}",
            row.label, row.symbol
        );
        let before = change.old.as_ref().map(site_text);
        let now = change.new.as_ref().map(site_text);
        if let Some(before) = &before {
            let _ = writeln!(text, "Before: {before}");
        }
        if let Some(now) = &now {
            let _ = writeln!(text, "Now: {now}");
        }

        // Each item counts in the class of its best path from this change.
        let change_number = u32::try_from(index).unwrap_or(u32::MAX);
        let reached = |class: ImpactClass| -> Vec<&ImpactItem> {
            analysis
                .items
                .iter()
                .filter(|item| {
                    item.paths
                        .iter()
                        .find(|path| path.change == change_number)
                        .is_some_and(|path| path.class == class)
                })
                .collect()
        };
        let editing = reached(ImpactClass::Adaptation);
        let behaviour = reached(ImpactClass::Behaviour);
        let _ = writeln!(
            text,
            "It reaches {} needing editing and {} that may behave differently.",
            editing.len(),
            behaviour.len()
        );
        for (class, items) in [
            (ImpactClass::Adaptation, &editing),
            (ImpactClass::Behaviour, &behaviour),
        ] {
            if items.is_empty() {
                continue;
            }
            let _ = writeln!(text, "Top {}:", class_text(class));
            for item in items.iter().take(TOP_REACHED) {
                let _ = writeln!(text, "{}", item_line(item, analysis));
            }
        }
        if editing.len() + behaviour.len() > TOP_REACHED {
            let _ = writeln!(text, "`list_impact` with change {index} lists them all.");
        }

        let selected: Vec<&SelectedTest> = analysis
            .tests
            .selected
            .iter()
            .filter(|test| test_of_change(analysis, test, change_number))
            .collect();
        if selected.is_empty() {
            let _ = writeln!(text, "No test reaches change #{index}.");
        } else {
            let _ = writeln!(
                text,
                "{} {} selected for it; `tests_to_run` with change {index} gives the commands:",
                selected.len(),
                noun(selected.len(), "test")
            );
            for test in selected.iter().take(TOP_ITEMS) {
                let _ = writeln!(
                    text,
                    "  {} ({}) {}",
                    test.symbol.path,
                    test_reason_text(test.reason),
                    site_text(&test.location)
                );
            }
            if selected.len() > TOP_ITEMS {
                let _ = writeln!(text, "  … {} more", selected.len() - TOP_ITEMS);
            }
        }

        let mut data = change_data(index, change);
        data["before"] = json!(before);
        data["now"] = json!(now);
        data["needs_editing"] = json!(editing.len());
        data["may_behave_differently"] = json!(behaviour.len());
        data["top_items"] = json!(
            editing
                .iter()
                .take(TOP_REACHED)
                .chain(behaviour.iter().take(TOP_REACHED))
                .map(|item| item_data(item, analysis))
                .collect::<Vec<_>>()
        );
        data["tests"] = json!(
            selected
                .iter()
                .take(TOP_ITEMS)
                .map(|test| test.symbol.path.clone())
                .collect::<Vec<_>>()
        );
        data["tests_selected"] = json!(selected.len());
        explained.push(data);
    }
    text.push_str(&limits_line(analysis));

    let data = json!({
        "comparison": comparison_data(status, Some(analysis)),
        "changes": explained,
        "limits": limit_parts(analysis),
    });
    Shaped { text, data }
}

/// Why one item is listed, for `explain_impact`.
pub fn explain(status: &Status, analysis: &Analysis, item: &ImpactItem) -> Shaped {
    let mut text = comparison_line(status, Some(analysis));
    text.push('\n');
    let _ = writeln!(
        text,
        "{} ({}) at {}",
        item.symbol.path,
        item.symbol.kind,
        site_text(&item.location)
    );
    let tags = tag_names(&item.tags);
    let _ = write!(
        text,
        "{} · {} confidence · {} {}",
        class_text(item.class),
        item.confidence,
        item.distance,
        noun(item.distance as usize, "step")
    );
    if !tags.is_empty() {
        let _ = write!(text, " · {}", tags.join(", "));
    }
    if item.tags.changed {
        text.push_str(" · itself changed in this comparison");
    }
    text.push('\n');
    if let Some(path) = best_path(item) {
        let _ = writeln!(text, "{}", explanation(item, path, analysis).text);
    }

    let mut paths = Vec::new();
    for (number, path) in item.paths.iter().enumerate() {
        let change = analysis.diff.changes.get(path.change as usize);
        let change_text = change.map_or(String::new(), |change| {
            let row = change_row(change);
            format!("{} {}", row.label, row.symbol)
        });
        let _ = writeln!(
            text,
            "Path {} ({}, from change #{} {}):",
            number + 1,
            class_text(path.class),
            path.change,
            change_text
        );
        let mut steps = Vec::new();
        for (index, step) in path.steps.iter().enumerate().rev() {
            let words = step_text(step, path, analysis, index == 0);
            let site = site_text(&step.site);
            let _ = writeln!(text, "  {} {words} at {site}", step.from.path);
            steps.push(json!({
                "from": step.from.path,
                "to": step.to.path,
                "text": words,
                "site": site,
                "edge": step.edge.as_str(),
                "certainty": step.certainty.as_str(),
            }));
        }
        paths.push(json!({
            "change": path.change,
            "class": class_text(path.class),
            "steps_from_item": steps,
        }));
    }
    if item.path_count as usize > item.paths.len() {
        let _ = writeln!(
            text,
            "{} paths reach it in all; the best {} are shown.",
            item.path_count,
            item.paths.len()
        );
    }

    let covering = covering_tests(analysis, &item.symbol);
    if covering.is_empty() {
        text.push_str("No selected test reaches it.\n");
    } else {
        let _ = writeln!(text, "Tests that reach it:");
        for test in covering.iter().take(MAX_TESTS) {
            let _ = writeln!(text, "  {}", test.symbol.path);
        }
    }
    text.push_str(&limits_line(analysis));

    let mut data = item_data(item, analysis);
    data["comparison"] = comparison_data(status, Some(analysis));
    data["explanation"] = json!(best_path(item).map(|path| explanation(item, path, analysis).text));
    data["paths"] = json!(paths);
    data["path_count"] = json!(item.path_count);
    data["covering_tests"] = json!(
        covering
            .iter()
            .take(MAX_TESTS)
            .map(|test| test.symbol.path.clone())
            .collect::<Vec<_>>()
    );
    data["limits"] = json!(limit_parts(analysis));
    Shaped { text, data }
}

/// The tests to run, for `tests_to_run`: all of them, or those of one change.
pub fn tests(status: &Status, analysis: &Analysis, change: Option<u32>) -> Shaped {
    let selection = &analysis.tests;
    let (selected, commands): (Vec<&SelectedTest>, Vec<TestCommand>) = match change {
        None => (
            selection.selected.iter().collect(),
            selection.commands.clone(),
        ),
        Some(change) => {
            let selected: Vec<&SelectedTest> = selection
                .selected
                .iter()
                .filter(|test| test_of_change(analysis, test, change))
                .collect();
            (
                selected.clone(),
                commands_for(&selected, &selection.commands),
            )
        }
    };

    let mut text = comparison_line(status, Some(analysis));
    text.push('\n');
    if selected.is_empty() {
        let _ = match change {
            Some(change) => writeln!(text, "No test reaches change #{change}."),
            None => writeln!(text, "No test reaches these changes."),
        };
    } else {
        let scope = change.map_or(String::new(), |change| format!(" for change #{change}"));
        let _ = writeln!(
            text,
            "{} of {} tests selected{scope}.",
            selected.len(),
            selection.total
        );
    }
    for command in &commands {
        let _ = writeln!(
            text,
            "{}   ({}: {} of {})",
            command.command_line(),
            target_label(command),
            command.tests,
            command.total
        );
    }
    if !selected.is_empty() {
        text.push_str("Tests:\n");
        for test in selected.iter().take(MAX_TESTS) {
            let _ = writeln!(
                text,
                "  {} ({}) {}",
                test.symbol.path,
                test_reason_text(test.reason),
                site_text(&test.location)
            );
        }
        if selected.len() > MAX_TESTS {
            let _ = writeln!(text, "  … {} more", selected.len() - MAX_TESTS);
        }
    }
    text.push_str(&limits_line(analysis));

    let data = json!({
        "comparison": comparison_data(status, Some(analysis)),
        "selected": selected.len(),
        "total": selection.total,
        "commands": commands.iter().map(|command| json!({
            "command": command.command_line(),
            "target": target_label(command),
            "tests": command.tests,
            "total": command.total,
        })).collect::<Vec<_>>(),
        "tests": selected.iter().take(MAX_TESTS).map(|test| json!({
            "symbol": test.symbol.path,
            "reason": test_reason_text(test.reason),
            "filter": test.filter,
            "location": site_text(&test.location),
        })).collect::<Vec<_>>(),
        "limits": limit_parts(analysis),
    });
    Shaped { text, data }
}

/// Symbols a search found, for `find_symbol`.
pub fn symbols(query: &str, hits: &[SymbolHit]) -> Shaped {
    let mut text = if hits.is_empty() {
        format!("No symbol matches `{query}`.")
    } else {
        format!(
            "{} {} matching `{query}`:\n",
            hits.len(),
            noun(hits.len(), "symbol")
        )
    };
    for hit in hits {
        let _ = writeln!(
            text,
            "{} {} {}",
            hit.kind,
            hit.symbol.path,
            site_text(&hit.location)
        );
    }
    let data = json!({
        "symbols": hits.iter().map(|hit| json!({
            "symbol": hit.symbol.path,
            "kind": hit.kind,
            "location": site_text(&hit.location),
        })).collect::<Vec<_>>(),
    });
    Shaped {
        text: text.trim_end().to_owned(),
        data,
    }
}

/// One change as one line: `#n kind symbol file:line`.
fn change_line(index: usize, change: &Change) -> String {
    let row = change_row(change);
    format!("#{index} {} {} {}", row.label, row.symbol, row.location)
        .trim_end()
        .to_owned()
}

/// One change as JSON.
fn change_data(index: usize, change: &Change) -> Value {
    let row = change_row(change);
    json!({
        "index": index,
        "kind": row.label,
        "symbol": row.symbol,
        "location": row.location,
        "note": change_note(change),
    })
}

/// How many changes there are of each kind, as `80 added`, in the order of
/// [`ChangeKind::ALL`].
fn kind_counts(changes: &[Change]) -> Vec<String> {
    ChangeKind::ALL
        .iter()
        .filter_map(|kind| {
            let count = changes.iter().filter(|change| change.kind == *kind).count();
            (count > 0).then(|| format!("{count} {}", kind.as_str()))
        })
        .collect()
}

/// The first changed files, and `…` when there are more.
fn files_text(analysis: &Analysis) -> String {
    let files = &analysis.diff.changed_files;
    let mut names: Vec<&str> = files
        .iter()
        .take(TOP_FILES)
        .map(|file| file.path.as_str())
        .collect();
    if files.len() > TOP_FILES {
        names.push("…");
    }
    names.join(", ")
}

/// One item as one line: class, confidence, distance, path, reason, place.
fn item_line(item: &ImpactItem, analysis: &Analysis) -> String {
    let mut line = format!(
        "- [{} · {} · {} {}] {} — {} ({})",
        class_text(item.class),
        item.confidence,
        item.distance,
        noun(item.distance as usize, "step"),
        item.symbol.path,
        path_summary(item, analysis),
        site_text(&item.location)
    );
    let tags = tag_names(&item.tags);
    if !tags.is_empty() {
        let _ = write!(line, " [{}]", tags.join(", "));
    }
    line
}

/// One item as JSON.
fn item_data(item: &ImpactItem, analysis: &Analysis) -> Value {
    json!({
        "symbol": item.symbol.path,
        "kind": item.symbol.kind.as_str(),
        "class": class_text(item.class),
        "confidence": item.confidence.as_str(),
        "distance": item.distance,
        "reason": path_summary(item, analysis),
        "location": site_text(&item.location),
        "tags": tag_names(&item.tags),
        "changed": item.tags.changed,
        "changes": item.paths.iter().map(|path| path.change).collect::<Vec<_>>(),
    })
}

/// The two sides compared, as the window labels them.
fn sides(status: &Status, analysis: Option<&Analysis>) -> (String, String) {
    match (&status.comparison, analysis) {
        (Some((base, head)), _) => (base.clone(), head.clone()),
        (None, Some(analysis)) => (
            analysis.diff.comparison.base.to_string(),
            analysis.diff.comparison.head.to_string(),
        ),
        (None, None) => (String::new(), String::new()),
    }
}

/// The comparison, and the what-if when one is shown, as one line.
fn comparison_line(status: &Status, analysis: Option<&Analysis>) -> String {
    let (base, head) = sides(status, analysis);
    let mut line = if base.is_empty() && head.is_empty() {
        "Comparison: none chosen".to_owned()
    } else {
        format!("Comparison: {base} → {head}")
    };
    if status.watching {
        line.push_str(" (refreshes when files are saved)");
    }
    if let Some((symbol, assume)) = &status.what_if {
        let _ = write!(
            line,
            "\nShowing a what-if: {} assumed `{}` (not a real change)",
            symbol.path,
            assume.as_str()
        );
    }
    if status.busy {
        line.push_str("\nA run is in progress.");
    }
    line
}

/// The comparison as JSON.
fn comparison_data(status: &Status, analysis: Option<&Analysis>) -> Value {
    let (base, head) = sides(status, analysis);
    json!({
        "base": base,
        "head": head,
        "watching": status.watching,
        "what_if": status.what_if.as_ref().map(|(symbol, assume)| json!({
            "symbol": symbol.path,
            "assume": assume.as_str(),
        })),
    })
}

/// The limits sentence, always present so the agent knows it was checked.
fn limits_line(analysis: &Analysis) -> String {
    let parts = limit_parts(analysis);
    if parts.is_empty() {
        "Limits: none.".to_owned()
    } else {
        format!("Limits: {}.", parts.join(" · "))
    }
}

/// How many items have `class`.
fn count_class(analysis: &Analysis, class: ImpactClass) -> usize {
    analysis
        .items
        .iter()
        .filter(|item| item.class == class)
        .count()
}

/// The selected tests whose evidence runs through `symbol`, or that are it,
/// as the window's evidence panel lists them.
fn covering_tests<'a>(analysis: &'a Analysis, symbol: &SymbolPath) -> Vec<&'a SelectedTest> {
    analysis
        .tests
        .selected
        .iter()
        .filter(|test| {
            &test.symbol == symbol
                || analysis
                    .items
                    .iter()
                    .filter(|item| item.symbol == test.symbol)
                    .flat_map(|item| item.paths.iter())
                    .any(|path| path.steps.iter().any(|step| &step.from == symbol))
        })
        .collect()
}

/// Whether `test` is selected because of the change numbered `change`: it is
/// that change, or one of its paths starts there.
fn test_of_change(analysis: &Analysis, test: &SelectedTest, change: u32) -> bool {
    let changed = analysis
        .diff
        .changes
        .get(change as usize)
        .is_some_and(|changed| test.reason == TestReason::Changed && changed.symbol == test.symbol);
    changed
        || analysis
            .items
            .iter()
            .filter(|item| item.symbol == test.symbol)
            .flat_map(|item| item.paths.iter())
            .any(|path| path.change == change)
}

/// Commands that run exactly `tests`, one per target of `commands` that
/// holds some of them.
fn commands_for(tests: &[&SelectedTest], commands: &[TestCommand]) -> Vec<TestCommand> {
    let mut chosen: Vec<TestCommand> = Vec::new();
    for test in tests {
        let Some(command) = target_of(test, commands) else {
            continue;
        };
        let at = match chosen.iter().position(|known| {
            known.package == command.package && known.target_name == command.target_name
        }) {
            Some(at) => at,
            None => {
                chosen.push(TestCommand {
                    filters: Vec::new(),
                    tests: 0,
                    ..command.clone()
                });
                chosen.len() - 1
            }
        };
        chosen[at].filters.push(test.filter.clone());
        chosen[at].tests += 1;
    }
    chosen
}

/// The command of the target `test` is in: the one named after its crate,
/// else the one of its package.
fn target_of<'a>(test: &SelectedTest, commands: &'a [TestCommand]) -> Option<&'a TestCommand> {
    let krate = test.symbol.crate_name();
    let named = |name: &str| name.replace('-', "_") == krate;
    commands
        .iter()
        .find(|command| named(&command.target_name))
        .or_else(|| commands.iter().find(|command| named(&command.package)))
}

/// `noun` with an `s` unless there is exactly one.
fn noun(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}
