//! Comparison of two `Cargo.toml` texts.
//!
//! Only what the rest of the analysis cares about is compared: the features a
//! package offers and the dependencies it names, including the ones declared
//! for a specific target. Every difference becomes one
//! [`ChangeKind::Manifest`] change whose detail is a one-line description and
//! whose location is the manifest file.

use std::collections::{BTreeMap, BTreeSet};

use blastr_model::{
    Change, ChangeDetail, ChangeKind, Location, RelPath, Span, SymbolKind, SymbolPath,
};
use toml::{Table, Value};

/// The dependency tables Cargo reads, in the root and under `[target.*]`.
const DEPENDENCIES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// Feature and dependency differences between two `Cargo.toml` texts.
///
/// A side that is absent or does not parse counts as an empty manifest; when
/// neither side is available the result is empty. The result is sorted by
/// description. Each change is placed on the first line of the manifest: on
/// the head side, or on the base side when the file exists only there.
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
    let (old_place, new_place) = match (old, new) {
        (_, Some(text)) => (None, Some(first_line(path, text))),
        (Some(text), None) => (Some(first_line(path, text)), None),
        (None, None) => (None, None),
    };
    descriptions
        .into_iter()
        .map(|description| Change {
            kind: ChangeKind::Manifest,
            symbol: symbol.clone(),
            old: old_place.clone(),
            new: new_place.clone(),
            detail: ChangeDetail::Manifest(description),
        })
        .collect()
}

/// The first line of the manifest at `path` whose text is `text`.
fn first_line(path: &RelPath, text: &str) -> Location {
    let end = text.find('\n').unwrap_or(text.len());
    Location {
        file: path.clone(),
        span: Span {
            start: 0,
            end: u32::try_from(end).unwrap_or(u32::MAX),
            start_line: 1,
            end_line: 1,
        },
    }
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

/// The table a dependency is declared in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DependencyTable {
    /// The `cfg(...)` or triple of a `[target.*]` table; `None` at the root.
    target: Option<String>,
    /// `dependencies`, `dev-dependencies` or `build-dependencies`.
    section: &'static str,
}

impl DependencyTable {
    /// How a dependency of this table is called: `dev-dependency`.
    fn noun(&self) -> &'static str {
        match self.section {
            "dev-dependencies" => "dev-dependency",
            "build-dependencies" => "build-dependency",
            _ => "dependency",
        }
    }

    /// The start of a description of dependency `name` in this table:
    /// "dev-dependency `x`", "dependency `x` (target cfg(windows))".
    fn subject(&self, name: &str) -> String {
        match &self.target {
            None => format!("{} `{name}`", self.noun()),
            Some(target) => format!("{} `{name}` (target {target})", self.noun()),
        }
    }

    /// The table's name in a sentence: "dev-dependencies",
    /// "dependencies (target cfg(windows))".
    fn name(&self) -> String {
        match &self.target {
            None => self.section.to_owned(),
            Some(target) => format!("{} (target {target})", self.section),
        }
    }
}

/// What one manifest says that the comparison needs.
#[derive(Debug, Default)]
struct Doc {
    package: Option<String>,
    features: BTreeMap<String, Vec<String>>,
    /// The dependencies, by name and then by the table they are in.
    dependencies: BTreeMap<String, BTreeMap<DependencyTable, Dependency>>,
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
            let table = DependencyTable {
                target: None,
                section,
            };
            read_dependencies(&table, entries, &mut doc.dependencies);
        }
    }
    if let Some(targets) = table.get("target").and_then(Value::as_table) {
        for (target, value) in targets {
            let Some(target_table) = value.as_table() else {
                continue;
            };
            for section in DEPENDENCIES {
                if let Some(entries) = target_table.get(section).and_then(Value::as_table) {
                    let table = DependencyTable {
                        target: Some(target.clone()),
                        section,
                    };
                    read_dependencies(&table, entries, &mut doc.dependencies);
                }
            }
        }
    }
    doc
}

/// Adds the dependencies of one table, keeping the table with each so that a
/// dependency can move between tables.
fn read_dependencies(
    table: &DependencyTable,
    entries: &Table,
    out: &mut BTreeMap<String, BTreeMap<DependencyTable, Dependency>>,
) {
    for (name, value) in entries {
        out.entry(name.clone())
            .or_default()
            .insert(table.clone(), dependency(value));
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
///
/// A dependency that leaves exactly one table and enters exactly one other is
/// one move, not a removal and an addition.
fn dependencies(old: &Doc, new: &Doc, out: &mut BTreeSet<String>) {
    let none = BTreeMap::new();
    let names: BTreeSet<&String> = old
        .dependencies
        .keys()
        .chain(new.dependencies.keys())
        .collect();
    for name in names {
        let before = old.dependencies.get(name).unwrap_or(&none);
        let after = new.dependencies.get(name).unwrap_or(&none);
        let removed: Vec<(&DependencyTable, &Dependency)> = before
            .iter()
            .filter(|(table, _)| !after.contains_key(*table))
            .collect();
        let added: Vec<(&DependencyTable, &Dependency)> = after
            .iter()
            .filter(|(table, _)| !before.contains_key(*table))
            .collect();
        if let ([(from, was)], [(to, now)]) = (removed.as_slice(), added.as_slice()) {
            let mut description = format!(
                "dependency `{name}` moved from {} to {}",
                from.name(),
                to.name()
            );
            if was.version != now.version {
                description.push_str(&format!(" ({} \u{2192} {})", was.version, now.version));
            }
            out.insert(description);
        } else {
            for (table, dependency) in added {
                let subject = table.subject(name);
                out.insert(format!("{subject} added ({})", dependency.version));
            }
            for (table, _) in removed {
                out.insert(format!("{} removed", table.subject(name)));
            }
        }
        for (table, now) in after {
            let Some(was) = before.get(table) else {
                continue;
            };
            let subject = table.subject(name);
            if was.version != now.version {
                out.insert(format!(
                    "{subject} changed: {} \u{2192} {}",
                    was.version, now.version
                ));
            } else if was.features != now.features || was.default_features != now.default_features {
                out.insert(format!("{subject} features changed"));
            }
        }
    }
}
