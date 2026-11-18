//! Plain text rendering helpers.
//!
//! Everything the commands print is built here so the text can be asserted on
//! in tests without going through the command layer.

use crate::engine::{Engine, Source};
use crate::util;

/// Renders a fixed width table with a header and a dashed separator.
///
/// Rows shorter than the header are padded with empty cells; longer rows are
/// truncated to the number of header columns.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in rows {
        for (index, width) in widths.iter_mut().enumerate() {
            let cell = row.get(index).map_or("", String::as_str);
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut output = String::new();
    let header: Vec<String> = headers
        .iter()
        .zip(&widths)
        .map(|(text, width)| pad(text, *width))
        .collect();
    output.push_str(header.join("  ").trim_end());
    output.push('\n');
    let total = widths.iter().sum::<usize>() + 2 * headers.len().saturating_sub(1);
    output.push_str(&"-".repeat(total));
    output.push('\n');
    for row in rows {
        let cells: Vec<String> = widths
            .iter()
            .enumerate()
            .map(|(index, width)| pad(row.get(index).map_or("", String::as_str), *width))
            .collect();
        output.push_str(cells.join("  ").trim_end());
        output.push('\n');
    }
    output
}

fn pad(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - length))
    }
}

/// Renders `label: value` lines aligned on the labels.
pub fn key_values(pairs: &[(&str, String)]) -> String {
    let width = pairs
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::with_capacity(pairs.len());
    for (label, value) in pairs {
        lines.push(format!("{}: {value}", pad(label, width)));
    }
    lines.join("\n")
}

/// Renders a bulleted list.
pub fn bullets(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<String>>()
        .join("\n")
}

/// Renders a horizontal bar of `width` characters.
pub fn bar(value: usize, total: usize, width: usize) -> String {
    if total == 0 || width == 0 {
        return String::new();
    }
    let filled = (value * width + total / 2) / total;
    let filled = filled.min(width);
    format!("{}{}", "#".repeat(filled), ".".repeat(width - filled))
}

/// The effective settings of an engine as a table.
pub fn settings_table(engine: &Engine) -> String {
    let rows: Vec<Vec<String>> = engine
        .effective()
        .into_iter()
        .map(|entry| {
            vec![
                entry.key().to_string(),
                entry.value().render(),
                entry.source().as_str().to_string(),
            ]
        })
        .collect();
    table(&["key", "value", "source"], &rows)
}

/// How many effective values came from each source, as a bar chart.
pub fn source_histogram(engine: &Engine) -> String {
    let counts = engine.count_by_source();
    let total: usize = counts.values().sum();
    let rows: Vec<Vec<String>> = counts
        .iter()
        .map(|(source, count)| {
            vec![
                source.as_str().to_string(),
                count.to_string(),
                bar(*count, total, 20),
            ]
        })
        .collect();
    table(&["source", "count", "share"], &rows)
}

/// The effective values of one source, in key order.
pub fn values_from(engine: &Engine, source: Source) -> Vec<String> {
    engine
        .effective()
        .into_iter()
        .filter(|entry| entry.source() == source)
        .map(|entry| format!("{} = {}", entry.key(), entry.value().render()))
        .collect()
}

/// Wraps `text` and indents the continuation lines.
pub fn paragraph(text: &str, width: usize) -> String {
    let lines = util::wrap(text, width);
    if lines.is_empty() {
        return String::new();
    }
    let mut output = lines[0].clone();
    for line in &lines[1..] {
        output.push('\n');
        output.push_str("  ");
        output.push_str(line);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        bar, bullets, key_values, paragraph, settings_table, source_histogram, table, values_from,
    };
    use crate::engine::{Engine, Source};

    #[test]
    fn tables_align_their_columns() {
        let rows = vec![
            vec!["loop.gain".to_string(), "1.25".to_string()],
            vec!["sample.interval".to_string(), "1 s".to_string()],
        ];
        let rendered = table(&["key", "value"], &rows);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], format!("key{}value", " ".repeat(14)));
        assert_eq!(lines[1], "-".repeat(22));
        assert_eq!(lines[2], format!("loop.gain{}1.25", " ".repeat(8)));
        assert_eq!(lines[3], "sample.interval  1 s");
    }

    #[test]
    fn tables_tolerate_ragged_rows() {
        let rows = vec![vec!["only".to_string()]];
        let rendered = table(&["a", "b"], &rows);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "a     b");
        assert_eq!(lines[1], "-".repeat(7));
        assert_eq!(lines[2], "only");
        assert_eq!(table(&[], &rows), "");
    }

    #[test]
    fn key_values_align_on_the_longest_label() {
        let rendered = key_values(&[
            ("key", "loop.gain".to_string()),
            ("kind", "float".to_string()),
        ]);
        assert_eq!(rendered, "key : loop.gain\nkind: float");
    }

    #[test]
    fn bullets_and_paragraphs_wrap() {
        assert_eq!(bullets(&["a".to_string(), "b".to_string()]), "- a\n- b");
        assert_eq!(bullets(&[]), "");
        assert_eq!(paragraph("the loop gain", 20), "the loop gain");
        assert_eq!(
            paragraph("the loop gain is tuned on site", 10),
            "the loop\n  gain is\n  tuned on\n  site"
        );
        assert_eq!(paragraph("", 10), "");
    }

    #[test]
    fn bars_scale_with_the_total() {
        assert_eq!(bar(0, 10, 10), "..........");
        assert_eq!(bar(5, 10, 10), "#####.....");
        assert_eq!(bar(10, 10, 10), "##########");
        assert_eq!(bar(1, 0, 10), "");
        assert_eq!(bar(1, 10, 0), "");
        assert_eq!(bar(1, 3, 10), "###.......");
    }

    #[test]
    fn engine_tables_show_provenance() {
        let mut engine = Engine::with_baseline();
        engine.set("loop.gain", "2.5").expect("in range");
        let rendered = settings_table(&engine);
        assert!(rendered.contains("fluid.density"));
        assert!(
            rendered
                .lines()
                .next()
                .unwrap_or_default()
                .starts_with("key")
        );
        assert!(rendered.contains("loop.gain"));
        assert!(rendered.contains("2.5"));
        assert!(rendered.contains("argument"));
        let histogram = source_histogram(&engine);
        assert!(histogram.contains("default"));
        assert!(histogram.contains("argument"));
        assert_eq!(values_from(&engine, Source::Argument).len(), 1);
        assert_eq!(values_from(&engine, Source::File).len(), 0);
    }
}
