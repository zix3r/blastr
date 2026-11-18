//! Generates the lookup tables used by `macrokit`.
//!
//! Three data files in `data/` are parsed and validated here, then rewritten as
//! Rust statics into `$OUT_DIR/tables_gen.rs`:
//!
//! * `units.txt` — the unit registry (symbol, dimension, scale, offset),
//! * `prefixes.txt` — the SI decimal prefixes used for prefixed lookups,
//! * `settings.txt` — the baseline schema (keys, kinds, defaults, aliases),
//!
//! The script is deterministic (every table is sorted, every float is copied
//! verbatim from the input file) and it writes nothing outside `OUT_DIR`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Dimension codes in the order used by `[i8; 7]` inside the library.
const DIMENSION_CODES: [&str; 7] = ["LEN", "MASS", "TIME", "CURR", "TEMP", "AMT", "LUM"];

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));

    println!("cargo::rerun-if-changed=build.rs");
    for name in ["units.txt", "prefixes.txt", "settings.txt"] {
        println!("cargo::rerun-if-changed=data/{name}");
    }

    let source =
        generate(&manifest).unwrap_or_else(|error| panic!("macrokit build script: {error}"));
    let target = out_dir.join("tables_gen.rs");
    fs::write(&target, source).expect("generated tables can be written");
}

#[derive(Debug)]
struct UnitRow {
    symbol: String,
    exponents: [i8; 7],
    scale: String,
    offset: String,
}

#[derive(Debug)]
struct PrefixRow {
    symbol: String,
    power: i32,
}

#[derive(Debug)]
struct SettingRow {
    key: String,
    kind: String,
    default: Option<String>,
    required: bool,
    aliases: Vec<String>,
    deprecated: Option<String>,
    doc: String,
}

fn generate(manifest: &Path) -> Result<String, String> {
    let units = parse_units(&read_data(manifest, "units.txt")?)?;
    let prefixes = parse_prefixes(&read_data(manifest, "prefixes.txt")?)?;
    let settings = parse_settings(&read_data(manifest, "settings.txt")?, &units)?;
    Ok(render(&units, &prefixes, &settings))
}

fn read_data(manifest: &Path, name: &str) -> Result<String, String> {
    let path = manifest.join("data").join(name);
    fs::read_to_string(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn data_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
}

// --- units ----------------------------------------------------------------

fn parse_units(text: &str) -> Result<Vec<UnitRow>, String> {
    let mut rows: BTreeMap<String, UnitRow> = BTreeMap::new();
    for (number, line) in data_lines(text) {
        let columns: Vec<&str> = line.split('|').map(str::trim).collect();
        if columns.len() != 4 {
            return Err(format!(
                "units.txt:{number}: expected 4 columns, found {}",
                columns.len()
            ));
        }
        let symbol = columns[0];
        if symbol.is_empty() {
            return Err(format!("units.txt:{number}: empty unit symbol"));
        }
        let exponents =
            parse_dimension(columns[1]).map_err(|error| format!("units.txt:{number}: {error}"))?;
        let scale = float_literal(columns[2])
            .map_err(|error| format!("units.txt:{number}: scale {error}"))?;
        let offset = float_literal(columns[3])
            .map_err(|error| format!("units.txt:{number}: offset {error}"))?;
        if scale.parse::<f64>().expect("validated float") == 0.0 {
            return Err(format!("units.txt:{number}: scale of `{symbol}` is zero"));
        }
        let row = UnitRow {
            symbol: symbol.to_string(),
            exponents,
            scale,
            offset,
        };
        if rows.insert(symbol.to_string(), row).is_some() {
            return Err(format!("units.txt:{number}: duplicate unit `{symbol}`"));
        }
    }
    if rows.is_empty() {
        return Err("units.txt defines no units".to_string());
    }
    Ok(rows.into_values().collect())
}

fn parse_prefixes(text: &str) -> Result<Vec<PrefixRow>, String> {
    let mut rows: BTreeMap<String, PrefixRow> = BTreeMap::new();
    for (number, line) in data_lines(text) {
        let columns: Vec<&str> = line.split('|').map(str::trim).collect();
        if columns.len() != 2 {
            return Err(format!(
                "prefixes.txt:{number}: expected 2 columns, found {}",
                columns.len()
            ));
        }
        let power: i32 = columns[1]
            .parse()
            .map_err(|_| format!("prefixes.txt:{number}: `{}` is not a power", columns[1]))?;
        if !(-24..=24).contains(&power) {
            return Err(format!(
                "prefixes.txt:{number}: power {power} is outside -24..=24"
            ));
        }
        if columns[0].is_empty() {
            return Err(format!("prefixes.txt:{number}: empty prefix"));
        }
        let row = PrefixRow {
            symbol: columns[0].to_string(),
            power,
        };
        if rows.insert(row.symbol.clone(), row).is_some() {
            return Err(format!(
                "prefixes.txt:{number}: duplicate prefix `{}`",
                columns[0]
            ));
        }
    }
    let mut rows: Vec<PrefixRow> = rows.into_values().collect();
    // Longest prefix first: the runtime resolver relies on this ordering.
    rows.sort_by(|left, right| {
        right
            .symbol
            .len()
            .cmp(&left.symbol.len())
            .then_with(|| left.symbol.cmp(&right.symbol))
    });
    Ok(rows)
}

fn parse_dimension(text: &str) -> Result<[i8; 7], String> {
    let mut exponents = [0i8; 7];
    for token in text.split_whitespace() {
        let split = token
            .find(|character: char| {
                character.is_ascii_digit() || character == '-' || character == '+'
            })
            .ok_or_else(|| format!("dimension token `{token}` has no exponent"))?;
        let code = &token[..split];
        let index = DIMENSION_CODES
            .iter()
            .position(|candidate| *candidate == code)
            .ok_or_else(|| format!("unknown dimension code `{code}`"))?;
        let exponent: i8 = token[split..]
            .parse()
            .map_err(|_| format!("dimension token `{token}` has a bad exponent"))?;
        exponents[index] = exponent;
    }
    Ok(exponents)
}

fn float_literal(text: &str) -> Result<String, String> {
    let value: f64 = text
        .parse()
        .map_err(|_| format!("`{text}` is not a number"))?;
    if !value.is_finite() {
        return Err(format!("`{text}` is not finite"));
    }
    let mut literal = text.to_string();
    if !literal.contains(['.', 'e', 'E']) {
        literal.push_str(".0");
    }
    Ok(literal)
}

// --- settings -------------------------------------------------------------

#[derive(Debug)]
enum SeedKind {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
    },
    Text {
        max_len: usize,
    },
    Quantity {
        exponents: [i8; 7],
        min: f64,
        max: f64,
    },
}

fn parse_kind(text: &str) -> Result<SeedKind, String> {
    let (tag, rest) = match text.split_once(':') {
        Some((tag, rest)) => (tag, Some(rest)),
        None => (text, None),
    };
    match (tag, rest) {
        ("bool", None) => Ok(SeedKind::Bool),
        ("int", Some(rest)) => {
            let (min, max) = parse_range(rest)?;
            Ok(SeedKind::Int {
                min: min
                    .parse()
                    .map_err(|_| format!("`{min}` is not an integer"))?,
                max: max
                    .parse()
                    .map_err(|_| format!("`{max}` is not an integer"))?,
            })
        }
        ("float", Some(rest)) => {
            let (min, max) = parse_range(rest)?;
            Ok(SeedKind::Float {
                min: min
                    .parse()
                    .map_err(|_| format!("`{min}` is not a number"))?,
                max: max
                    .parse()
                    .map_err(|_| format!("`{max}` is not a number"))?,
            })
        }
        ("text", Some(rest)) => Ok(SeedKind::Text {
            max_len: rest
                .trim()
                .parse()
                .map_err(|_| format!("`{rest}` is not a length"))?,
        }),
        ("quantity", Some(rest)) => {
            let (dimension, range) = rest
                .split_once(';')
                .ok_or_else(|| "quantity kinds need `<dimension>;<min>..<max>`".to_string())?;
            let (min, max) = parse_range(range)?;
            Ok(SeedKind::Quantity {
                exponents: parse_dimension(dimension)?,
                min: min
                    .parse()
                    .map_err(|_| format!("`{min}` is not a number"))?,
                max: max
                    .parse()
                    .map_err(|_| format!("`{max}` is not a number"))?,
            })
        }
        (other, _) => Err(format!("unknown kind `{other}`")),
    }
}

fn parse_range(text: &str) -> Result<(&str, &str), String> {
    let (min, max) = text
        .split_once("..")
        .ok_or_else(|| format!("`{text}` is not a `<min>..<max>` range"))?;
    let (min, max) = (min.trim(), max.trim());
    if min.is_empty() || max.is_empty() {
        return Err(format!("`{text}` has an empty bound"));
    }
    Ok((min, max))
}

fn parse_settings(text: &str, units: &[UnitRow]) -> Result<Vec<SettingRow>, String> {
    let mut rows: BTreeMap<String, SettingRow> = BTreeMap::new();
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    for (number, line) in data_lines(text) {
        let columns: Vec<&str> = line.split('|').map(str::trim).collect();
        if columns.len() != 7 {
            return Err(format!(
                "settings.txt:{number}: expected 7 columns, found {}",
                columns.len()
            ));
        }
        let key = columns[0];
        validate_key(key).map_err(|error| format!("settings.txt:{number}: key {error}"))?;
        let kind =
            parse_kind(columns[1]).map_err(|error| format!("settings.txt:{number}: {error}"))?;
        let default = if columns[2].is_empty() {
            None
        } else {
            check_default(&kind, columns[2], units)
                .map_err(|error| format!("settings.txt:{number}: default of `{key}`: {error}"))?;
            Some(columns[2].to_string())
        };
        let required = match columns[3] {
            "true" => true,
            "false" => false,
            other => return Err(format!("settings.txt:{number}: `required` is `{other}`")),
        };
        let mut row_aliases = Vec::new();
        for alias in columns[4]
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            validate_key(alias).map_err(|error| format!("settings.txt:{number}: alias {error}"))?;
            if alias == key {
                return Err(format!("settings.txt:{number}: alias repeats its key"));
            }
            if let Some(owner) = aliases.insert(alias.to_string(), key.to_string()) {
                return Err(format!(
                    "settings.txt:{number}: alias `{alias}` is already used by `{owner}`"
                ));
            }
            row_aliases.push(alias.to_string());
        }
        let deprecated = if columns[5].is_empty() {
            None
        } else {
            Some(columns[5].to_string())
        };
        let row = SettingRow {
            key: key.to_string(),
            kind: columns[1].to_string(),
            default,
            required,
            aliases: row_aliases,
            deprecated,
            doc: columns[6].to_string(),
        };
        if rows.insert(key.to_string(), row).is_some() {
            return Err(format!("settings.txt:{number}: duplicate key `{key}`"));
        }
    }
    for (alias, owner) in &aliases {
        if rows.contains_key(alias) {
            return Err(format!("alias `{alias}` of `{owner}` shadows a key"));
        }
    }
    Ok(rows.into_values().collect())
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("is empty".to_string());
    }
    for segment in key.split('.') {
        let mut characters = segment.chars();
        match characters.next() {
            Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
            _ => return Err(format!("`{key}` must start each segment with [a-z0-9]")),
        }
        if !characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        }) {
            return Err(format!("`{key}` contains an unsupported character"));
        }
    }
    Ok(())
}

fn check_default(kind: &SeedKind, text: &str, units: &[UnitRow]) -> Result<(), String> {
    match kind {
        SeedKind::Bool => match text {
            "true" | "false" => Ok(()),
            other => Err(format!("`{other}` is not a boolean")),
        },
        SeedKind::Int { min, max } => {
            let value: i64 = text
                .parse()
                .map_err(|_| format!("`{text}` is not an integer"))?;
            if value < *min || value > *max {
                return Err(format!("{value} is outside {min}..={max}"));
            }
            Ok(())
        }
        SeedKind::Float { min, max } => {
            let value: f64 = text
                .parse()
                .map_err(|_| format!("`{text}` is not a number"))?;
            check_bounds(value, *min, *max)
        }
        SeedKind::Text { max_len } => {
            if text.chars().count() > *max_len {
                return Err(format!("`{text}` is longer than {max_len} characters"));
            }
            Ok(())
        }
        SeedKind::Quantity {
            exponents,
            min,
            max,
        } => {
            let (number, symbol) = split_quantity(text)?;
            let value: f64 = number
                .parse()
                .map_err(|_| format!("`{number}` is not a number"))?;
            let unit = units
                .iter()
                .find(|unit| unit.symbol == symbol)
                .ok_or_else(|| format!("unknown unit `{symbol}`"))?;
            if unit.exponents != *exponents {
                return Err(format!(
                    "`{symbol}` has dimension {:?}, expected {:?}",
                    unit.exponents, exponents
                ));
            }
            let scale: f64 = unit.scale.parse().expect("validated scale");
            let offset: f64 = unit.offset.parse().expect("validated offset");
            check_bounds(value * scale + offset, *min, *max)
        }
    }
}

fn check_bounds(value: f64, min: f64, max: f64) -> Result<(), String> {
    if value < min || value > max {
        return Err(format!("{value} is outside {min}..={max}"));
    }
    Ok(())
}

fn split_quantity(text: &str) -> Result<(&str, &str), String> {
    let end = text
        .find(|character: char| {
            !(character.is_ascii_digit() || matches!(character, '+' | '-' | '.' | 'e' | 'E'))
        })
        .unwrap_or(text.len());
    let number = &text[..end];
    let symbol = text[end..].trim();
    if number.is_empty() {
        return Err("value does not start with a number".to_string());
    }
    if symbol.is_empty() {
        return Err(format!("`{text}` has no unit"));
    }
    Ok((number, symbol))
}

// --- rendering ------------------------------------------------------------

fn render(units: &[UnitRow], prefixes: &[PrefixRow], settings: &[SettingRow]) -> String {
    let mut source = String::new();
    source.push_str("// @generated by macrokit's build script from data/*.txt. Do not edit.\n");
    source.push_str(
        "// Every table is sorted; the values are copied verbatim from the data files.\n\n",
    );

    source.push_str(
        "#[allow(clippy::excessive_precision, clippy::unreadable_literal)]\n\
         pub(crate) static UNIT_TABLE: &[crate::unit::UnitSeed] = &[\n",
    );
    for unit in units {
        let _ = writeln!(
            source,
            "    ({:?}, {:?}, {}, {}),",
            unit.symbol, unit.exponents, unit.scale, unit.offset
        );
    }
    source.push_str("];\n\n");

    source.push_str(
        "#[allow(clippy::unreadable_literal)]\n\
         pub(crate) static PREFIX_TABLE: &[crate::unit::PrefixSeed] = &[\n",
    );
    for prefix in prefixes {
        let _ = writeln!(source, "    ({:?}, {}),", prefix.symbol, prefix.power);
    }
    source.push_str("];\n\n");

    source.push_str("pub(crate) static SETTING_SEED: &[crate::schema::SettingSeed] = &[\n");
    for setting in settings {
        let default = match &setting.default {
            Some(text) => format!("Some({text:?})"),
            None => "None".to_string(),
        };
        let aliases: Vec<String> = setting
            .aliases
            .iter()
            .map(|alias| format!("{alias:?}"))
            .collect();
        let deprecated = match &setting.deprecated {
            Some(text) => format!("Some({text:?})"),
            None => "None".to_string(),
        };
        let _ = writeln!(
            source,
            "    ({:?}, {:?}, {}, {}, &[{}], {}, {:?}),",
            setting.key,
            setting.kind,
            default,
            setting.required,
            aliases.join(", "),
            deprecated,
            setting.doc
        );
    }
    source.push_str("];\n\n");

    let _ = writeln!(
        source,
        "pub(crate) const GENERATED_UNIT_COUNT: usize = {};",
        units.len()
    );
    let _ = writeln!(
        source,
        "pub(crate) const GENERATED_PREFIX_COUNT: usize = {};",
        prefixes.len()
    );
    let _ = writeln!(
        source,
        "pub(crate) const GENERATED_SETTING_COUNT: usize = {};",
        settings.len()
    );
    source
}
