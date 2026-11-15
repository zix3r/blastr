//! Tests for [`manifest_changes`].

use super::manifest::manifest_changes;
use blastr_model::{Change, ChangeDetail, ChangeKind, RelPath, SymbolKind};

/// The manifest descriptions of a result.
fn descriptions(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .map(|change| match &change.detail {
            ChangeDetail::Manifest(description) => description.clone(),
            other => panic!("unexpected detail {other:?}"),
        })
        .collect()
}

/// A manifest with a package name and every dependency table.
const OLD: &str = r#"
[package]
name = "demo"

[features]
default = ["std"]
std = []
extra = ["std", "fast"]

[dependencies]
serde = "1.0"
old = { version = "0.1" }
local = { path = "../local" }
rebuilt = { git = "https://example.com/x.git" }
shared = { workspace = true }
tweaked = { version = "2.0", features = ["a"] }
pinned = { version = "3.0", default-features = false }

[dev-dependencies]
dev-only = "1.0"

[build-dependencies]
cc = "1.0"

[target.'cfg(unix)'.dependencies]
libc = "0.2"
"#;

/// The same manifest after one change of every kind.
const NEW: &str = r#"
[package]
name = "demo"

[features]
default = ["std"]
std = []
extra = ["fast"]
fresh = []

[dependencies]
serde = "1.1"
new = "0.3"
local = { path = "../local" }
rebuilt = { git = "https://example.com/x.git" }
shared = { workspace = true }
tweaked = { version = "2.0", features = ["a", "b"] }
pinned = { version = "3.0", default-features = true }
legacy = { workspace = true, features = ["x"] }

[dev-dependencies]
dev-only = "1.0"

[build-dependencies]
cc = "1.1"

[target.'cfg(unix)'.dependencies]
libc = "0.3"
"#;

#[test]
fn reports_every_kind_of_manifest_change() {
    let changes = manifest_changes(&RelPath::new("Cargo.toml"), Some(OLD), Some(NEW));
    assert_eq!(
        descriptions(&changes),
        [
            "dependency `cc` changed: 1.0 \u{2192} 1.1",
            "dependency `legacy` added (workspace)",
            "dependency `libc` changed: 0.2 \u{2192} 0.3",
            "dependency `new` added (0.3)",
            "dependency `old` removed",
            "dependency `pinned` features changed",
            "dependency `serde` changed: 1.0 \u{2192} 1.1",
            "dependency `tweaked` features changed",
            "feature `extra` changed",
            "feature `fresh` added",
        ]
    );
    for change in &changes {
        assert_eq!(change.kind, ChangeKind::Manifest);
        assert_eq!(change.symbol.path, "demo");
        assert_eq!(change.symbol.kind, SymbolKind::Module);
        assert_eq!(change.old, None);
        assert_eq!(change.new, None);
    }
}

#[test]
fn reports_a_feature_that_disappeared() {
    let old = "[features]\nonce = [\"a\"]\nkeeper = []\n";
    let new = "[features]\nkeeper = [\"a\"]\n";
    let changes = manifest_changes(&RelPath::new("Cargo.toml"), Some(old), Some(new));
    assert_eq!(
        descriptions(&changes),
        ["feature `keeper` changed", "feature `once` removed"]
    );
}

#[test]
fn names_dependencies_without_a_version_by_what_they_point_at() {
    let old = r#"
[dependencies]
local = { path = "../local" }
rebuilt = { git = "https://example.com/x.git" }
"#;
    let new = r#"
[dependencies]
rebuilt = { git = "https://example.com/x.git" }
local2 = { version = "1.0", path = "../local2" }
shared = { workspace = true }
via-git = { git = "https://example.com/y.git" }
"#;
    let changes = manifest_changes(&RelPath::new("Cargo.toml"), Some(old), Some(new));
    assert_eq!(
        descriptions(&changes),
        [
            "dependency `local2` added (1.0)",
            "dependency `local` removed",
            "dependency `shared` added (workspace)",
            "dependency `via-git` added (git)",
        ]
    );
}

#[test]
fn reports_dependencies_added_to_a_target_table() {
    let old = "[target.'cfg(windows)'.dependencies]\nwindows = \"0.1\"\n";
    let new = "[target.'cfg(windows)'.build-dependencies]\nwindows = \"0.1\"\n";
    let changes = manifest_changes(&RelPath::new("Cargo.toml"), Some(old), Some(new));
    assert_eq!(
        descriptions(&changes),
        [
            "dependency `windows` added (0.1)",
            "dependency `windows` removed",
        ]
    );
}

#[test]
fn treats_an_unparsable_side_as_absent() {
    let changes = manifest_changes(
        &RelPath::new("Cargo.toml"),
        Some("not [ valid toml"),
        Some("[package]\nname = \"demo\"\n[dependencies]\nserde = \"1.0\"\n"),
    );
    assert_eq!(descriptions(&changes), ["dependency `serde` added (1.0)"]);
    assert_eq!(changes[0].symbol.path, "demo");
}

#[test]
fn returns_nothing_when_both_sides_are_absent() {
    assert!(manifest_changes(&RelPath::new("Cargo.toml"), None, None).is_empty());
    assert!(manifest_changes(&RelPath::new("Cargo.toml"), Some("["), Some("[")).is_empty());
}

#[test]
fn returns_nothing_when_the_manifests_agree() {
    assert!(manifest_changes(&RelPath::new("Cargo.toml"), Some(OLD), Some(OLD)).is_empty());
}

#[test]
fn falls_back_to_the_path_when_no_package_name_is_known() {
    let old = "[dependencies]\nserde = \"1.0\"\n";
    let new = "[dependencies]\nserde = \"1.1\"\n";
    let changes = manifest_changes(
        &RelPath::new("crates/demo/Cargo.toml"),
        Some(old),
        Some(new),
    );
    assert_eq!(
        descriptions(&changes),
        ["dependency `serde` changed: 1.0 \u{2192} 1.1"]
    );
    assert_eq!(changes[0].symbol.path, "crates/demo/Cargo.toml");
}

#[test]
fn takes_the_package_name_from_the_old_side_when_the_new_one_has_none() {
    let old = "[package]\nname = \"demo\"\n[dependencies]\nserde = \"1.0\"\n";
    let new = "[dependencies]\nserde = \"1.1\"\n";
    let changes = manifest_changes(&RelPath::new("Cargo.toml"), Some(old), Some(new));
    assert_eq!(
        descriptions(&changes),
        ["dependency `serde` changed: 1.0 \u{2192} 1.1"]
    );
    assert_eq!(changes[0].symbol.path, "demo");
}
