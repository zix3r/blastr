//! Small helpers shared by the engine, the unit registry and the commands.
//!
//! Everything here is pure and allocation-light so it can be tested directly.

/// Levenshtein distance between two strings, measured in `char`s.
///
/// Used to suggest `loop.gain` when the operator typed `loop.gian`.
pub fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, left_character) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, right_character) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_character != right_character);
            let deletion = previous[column + 1] + 1;
            let insertion = current[column] + 1;
            current[column + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// The closest candidate within `max_distance`, ties resolved by iteration
/// order.
pub fn closest<'a, I>(needle: &str, candidates: I, max_distance: usize) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut best: Option<(usize, &'a str)> = None;
    for candidate in candidates {
        let distance = levenshtein(needle, candidate);
        if distance > max_distance {
            continue;
        }
        if best.is_none_or(|(closest, _)| distance < closest) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// Splits a command line into arguments, honouring double quotes.
///
/// `set site.name "bay 7"` yields `["set", "site.name", "bay 7"]`. Inside
/// quotes, `\"` produces a literal quote and `\\` a literal backslash.
pub fn split_arguments(line: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut started = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            started = true;
            continue;
        }
        match character {
            '\\' if quoted => {
                escaped = true;
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            character => {
                current.push(character);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// Drops the `#` comment of a configuration line, ignoring `#` inside quotes.
pub fn split_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => return line[..index].trim_end(),
            _ => {}
        }
    }
    line.trim_end()
}

/// Renders a float without trailing zeroes, switching to exponent notation for
/// very small and very large magnitudes.
pub fn format_float(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let magnitude = value.abs();
    if magnitude >= 1.0e9 || magnitude < 1.0e-4 {
        return format!("{value:e}");
    }
    let mut text = format!("{value:.6}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

/// Word-wraps `text` at `width` characters without breaking words.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }
        if current.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            current.push(' ');
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Parses the boolean words accepted by `Kind::Bool`.
pub fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "enabled" => Some(true),
        "false" | "no" | "off" | "0" | "disabled" => Some(false),
        _ => None,
    }
}

/// `count` followed by the singular or plural noun.
pub fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        closest, format_float, levenshtein, parse_bool, plural, split_arguments, split_comment,
        wrap,
    };

    #[test]
    fn levenshtein_matches_known_distances() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("same", "same"), 0);
        assert_eq!(levenshtein("kPa", "Pa"), 1);
    }

    #[test]
    fn closest_picks_the_nearest_candidate() {
        let candidates = ["loop.gain", "loop.name", "loop.integral"];
        assert_eq!(closest("loop.gian", candidates, 3), Some("loop.gain"));
        assert_eq!(closest("site.name", candidates, 1), None);
        assert_eq!(closest("loop.nam", candidates, 2), Some("loop.name"));
    }

    #[test]
    fn arguments_survive_quotes_and_escapes() {
        assert_eq!(
            split_arguments("set site.name \"bay 7\""),
            vec!["set", "site.name", "bay 7"]
        );
        assert_eq!(
            split_arguments("set   logging.level\tdebug "),
            vec!["set", "logging.level", "debug"]
        );
        assert_eq!(split_arguments(""), Vec::<String>::new());
        assert_eq!(split_arguments("\"\""), vec![""]);
        assert_eq!(
            split_arguments(r#"set x "a\"b" "#),
            vec!["set", "x", "a\"b"]
        );
    }

    #[test]
    fn comments_are_cut_outside_quotes() {
        assert_eq!(split_comment("loop.gain = 2 # tuned"), "loop.gain = 2");
        assert_eq!(
            split_comment("site.name = \"a # b\""),
            "site.name = \"a # b\""
        );
        assert_eq!(split_comment("   "), "");
        assert_eq!(split_comment("no comment"), "no comment");
    }

    #[test]
    fn floats_render_without_noise() {
        assert_eq!(format_float(1.25), "1.25");
        assert_eq!(format_float(1000.0), "1000");
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(format_float(0.0), "0");
        assert_eq!(format_float(-2.5), "-2.5");
        assert_eq!(format_float(1.0e-6), "1e-6");
        assert_eq!(format_float(313_322.1), "313322.1");
    }

    #[test]
    fn wrapping_does_not_split_words() {
        assert_eq!(
            wrap("the loop gain is tuned", 10),
            vec!["the loop", "gain is", "tuned"]
        );
        assert_eq!(wrap("short", 10), vec!["short"]);
        assert_eq!(wrap("", 10), Vec::<String>::new());
    }

    #[test]
    fn boolean_words_are_recognised() {
        assert_eq!(parse_bool("YES"), Some(true));
        assert_eq!(parse_bool(" off "), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn plural_agrees_with_count() {
        assert_eq!(plural(1, "setting", "settings"), "1 setting");
        assert_eq!(plural(3, "setting", "settings"), "3 settings");
    }
}
