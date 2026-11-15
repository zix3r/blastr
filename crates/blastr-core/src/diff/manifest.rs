//! Comparison of two `Cargo.toml` texts.
//!
//! Only what the rest of the analysis cares about is compared: the features a
//! package offers and the dependencies it names, including the ones declared
//! for a specific target. Every difference becomes one
//! [`ChangeKind::Manifest`] change whose detail is a one-line description.

use std::collections::{BTreeMap, BTreeSet};

use blastr_model::{Change, ChangeDetail, ChangeKind, RelPath, SymbolKind, SymbolPath};
use toml::{Table, Value};

/// The dependency tables Cargo reads, in the root and under `[target.*]`.
const DEPENDENCIES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// Feature and dependency differences between two `Cargo.toml` texts.
///
/// A side that is absent or does not parse counts as an empty manifest; when
/// neither side is available the result is empty. The result is sorted by
/// description and has neither an old nor a new location.
pub fn manifest_changes(path: &RelPath, old: Option<&str>, new: Option<&str>) -> Vec<Change> {
    let old_text = old.and_then(parse);
    let new_text = new.and_then(parse);
    if old_text.is_none() && new_text.is_none() {
        return Vec::new();
    }
    let empty = Table::new();
    let old_doc = read(old_text.as_ref().unwrap_or(&empty));
    let new_doc = read(new_text.as_ref().unwrap_or(&empty));

    let mut descriptions = BTreeSet::new();
    features(&old_doc, &new_doc, &mut descriptions);
    dependencies(&old_doc, &new_doc, &mut descriptions);

    let name = new_doc
        .package
        .or(old_doc.package)
        .unwrap_or_else(|| path.as_str().to_owned());
    let symbol = SymbolPath::new(name, SymbolKind::Module);
    descriptions
        .into_iter()
        .map(|description| Change {
            kind: ChangeKind::Manifest,
            symbol: symbol.clone(),
            old: None,
            new: None,
            detail: ChangeDetail::Manifest(description),
        })
        .collect()
}

/// Parses one manifest; `None` when the text is missing or not valid TOML.
fn parse(text: &str) -> Option<Table> {
    toml::from_str::<Table>(text).ok()
}

/// One dependency of a manifest.
#[derive(Debug, PartialEq, Eq)]
struct Dependency {
    /// The version, or `path`, `git` or `workspace` when there is no version.
    version: String,
    /// The enabled features, sorted.
    features: Vec<String>,
    /// The value of `default-features`; true when absent.
    default_features: bool,
}

/// What one manifest says that the comparison needs.
#[derive(Debug, Default)]
struct Doc {
    package: Option<String>,
    features: BTreeMap<String, Vec<String>>,
    dependencies: BTreeMap<(String, String), Dependency>,
}

/// Reads one manifest table into the comparable form.
fn read(table: &Table) -> Doc {
    let mut doc = Doc {
        package: table
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Doc::default()
    };
    if let Some(features) = table.get("features").and_then(Value::as_table) {
        for (name, value) in features {
            doc.features.insert(name.clone(), enabled(value));
        }
    }
    for section in DEPENDENCIES {
        if let Some(entries) = table.get(section).and_then(Value::as_table) {
            read_dependencies(section, entries, &mut doc.dependencies);
        }
    }
    if let Some(targets) = table.get("target").and_then(Value::as_table) {
        for (target, value) in targets {
            let Some(target_table) = value.as_table() else {
                continue;
            };
            for section in DEPENDENCIES {
                if let Some(entries) = target_table.get(section).and_then(Value::as_table) {
                    let label = format!("target.{target}.{section}");
                    read_dependencies(&label, entries, &mut doc.dependencies);
                }
            }
        }
    }
    doc
}

/// Adds the dependencies of one table, keeping the table's identity in the key
/// so that a dependency can move between tables.
fn read_dependencies(
    section: &str,
    entries: &Table,
    out: &mut BTreeMap<(String, String), Dependency>,
) {
    for (name, value) in entries {
        out.insert((section.to_owned(), name.clone()), dependency(value));
    }
}

/// Normalises one dependency entry.
fn dependency(value: &Value) -> Dependency {
    match value {
        Value::String(version) => Dependency {
            version: version.clone(),
            features: Vec::new(),
            default_features: true,
        },
        Value::Table(table) => Dependency {
            version: version_of(table),
            features: table.get("features").map(enabled).unwrap_or_default(),
            default_features: table
                .get("default-features")
                .or_else(|| table.get("default_features"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        other => Dependency {
            version: other.type_str().to_owned(),
            features: Vec::new(),
            default_features: true,
        },
    }
}

/// How a dependency's source is shown: its version, or what it points at.
fn version_of(table: &Table) -> String {
    if let Some(version) = table.get("version").and_then(Value::as_str) {
        return version.to_owned();
    }
    for key in ["path", "git"] {
        if table.contains_key(key) {
            return key.to_owned();
        }
    }
    if table.get("workspace").and_then(Value::as_bool) == Some(true) {
        return "workspace".to_owned();
    }
    "unknown".to_owned()
}

/// The strings of a feature list, sorted and without duplicates.
fn enabled(value: &Value) -> Vec<String> {
    let mut items: Vec<String> = value
        .as_array()
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    items.sort_unstable();
    items.dedup();
    items
}

/// Compares the two `[features]` tables.
fn features(old: &Doc, new: &Doc, out: &mut BTreeSet<String>) {
    for (name, items) in &new.features {
        let description = match old.features.get(name) {
            None => format!("feature `{name}` added"),
            Some(before) if before != items => format!("feature `{name}` changed"),
            Some(_) => continue,
        };
        out.insert(description);
    }
    for name in old.features.keys() {
        if !new.features.contains_key(name) {
            out.insert(format!("feature `{name}` removed"));
        }
    }
}

/// Compares the dependency tables of both manifests.
fn dependencies(old: &Doc, new: &Doc, out: &mut BTreeSet<String>) {
    for (key, after) in &new.dependencies {
        let name = &key.1;
        match old.dependencies.get(key) {
            None => {
                out.insert(format!("dependency `{name}` added ({})", after.version));
            }
            Some(before) if before.version != after.version => {
                out.insert(format!(
                    "dependency `{name}` changed: {} \u{2192} {}",
                    before.version, after.version
                ));
            }
            Some(before)
                if before.features != after.features
                    || before.default_features != after.default_features =>
            {
                out.insert(format!("dependency `{name}` features changed"));
            }
            Some(_) => {}
        }
    }
    for key in old.dependencies.keys() {
        if !new.dependencies.contains_key(key) {
            out.insert(format!("dependency `{}` removed", key.1));
        }
    }
}
