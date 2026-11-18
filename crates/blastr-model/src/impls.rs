//! Constructors, accessors and `Display` for the shared types.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{AnalysisKey, AnalysisOptions, ExecMode, RelPath, Source, SymbolKind, SymbolPath};

impl RelPath {
    /// Creates a path with `/` separators and no leading `./`.
    pub fn new(path: impl AsRef<str>) -> RelPath {
        let path = path.as_ref().replace('\\', "/");
        let path = path.strip_prefix("./").map_or(path.as_str(), |rest| rest);
        RelPath(path.to_owned())
    }

    /// The path text, with `/` separators.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Converts a relative [`Path`]; `None` when it is absolute or not UTF-8.
    pub fn from_path(path: &Path) -> Option<RelPath> {
        if path.is_absolute() {
            return None;
        }
        path.to_str().map(RelPath::new)
    }

    /// The absolute path obtained by joining this path onto `root`.
    pub fn to_path(&self, root: &Path) -> PathBuf {
        root.join(self.as_str())
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for SymbolPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Tree { label, .. } => f.write_str(label),
            Source::Index => f.write_str("index"),
            Source::Worktree => f.write_str("working tree"),
            Source::Dir(path) => write!(f, "{}", path.display()),
        }
    }
}

/// Splits a canonical path on the `::` separators that lie outside angle
/// brackets, so that a `<impl Trait for Type>` segment stays in one piece.
fn split_segments(path: &str) -> Vec<&str> {
    let bytes = path.as_bytes();
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
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

impl SymbolPath {
    /// Creates a path with disambiguator 0.
    pub fn new(path: impl Into<String>, kind: SymbolKind) -> Self {
        Self {
            path: path.into(),
            kind,
            disambiguator: 0,
        }
    }

    /// The last segment of the path.
    pub fn name(&self) -> &str {
        split_segments(&self.path)
            .pop()
            .unwrap_or(self.path.as_str())
    }

    /// The first segment of the path, which is the crate name.
    pub fn crate_name(&self) -> &str {
        split_segments(&self.path)
            .first()
            .copied()
            .unwrap_or(self.path.as_str())
    }
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            features: Vec::new(),
            all_features: false,
            target: None,
            exec: ExecMode::Full,
            manifest_dir: None,
            watch: true,
        }
    }
}

impl AnalysisKey {
    /// BLAKE3 of the canonical JSON serialisation, as lowercase hexadecimal.
    ///
    /// The serialisation cannot fail: every field is a string, a `Vec` of
    /// strings, a boolean, an `Option`, or an enum that serialises to a string.
    pub fn hash(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        blake3::hash(json.as_bytes()).to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key with every field set to a fixed value.
    fn sample_key() -> AnalysisKey {
        AnalysisKey {
            repository: "repo".to_owned(),
            source: "tree".to_owned(),
            features: vec!["default".to_owned()],
            all_features: false,
            target: Some("x86_64-pc-windows-msvc".to_owned()),
            cfg_test: false,
            cargo_lock: "lock".to_owned(),
            toolchain: "1.100.0".to_owned(),
            rust_analyzer: "0.0.358".to_owned(),
            blastr: "0.1.0".to_owned(),
            exec: ExecMode::Full,
            manifest_dir: None,
        }
    }

    #[test]
    fn relative_paths_are_normalised() {
        assert_eq!(RelPath::new("a\\b\\c.rs").as_str(), "a/b/c.rs");
        assert_eq!(RelPath::new("./a/b.rs").as_str(), "a/b.rs");
        assert_eq!(RelPath::new("a/b.rs").as_str(), "a/b.rs");
        // Only one leading `./` is stripped.
        assert_eq!(RelPath::new("././a").as_str(), "./a");
        // Parent components are kept as written.
        assert_eq!(RelPath::new("../a").as_str(), "../a");
    }

    #[test]
    fn relative_paths_convert_to_and_from_std_paths() {
        let relative = Path::new("src\\lib.rs");
        let converted = RelPath::from_path(relative).expect("a relative UTF-8 path");
        assert_eq!(converted.as_str(), "src/lib.rs");

        let absolute = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(RelPath::from_path(absolute).is_none());
    }

    #[test]
    fn relative_paths_join_onto_a_root() {
        let root = Path::new("root");
        assert_eq!(
            RelPath::new("a/b.rs").to_path(root),
            root.join("a").join("b.rs")
        );
    }

    #[cfg(windows)]
    #[test]
    fn non_utf8_paths_are_rejected() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        // A lone surrogate cannot be represented in UTF-8.
        let path = PathBuf::from(OsString::from_wide(&[0xD800]));
        assert!(RelPath::from_path(&path).is_none());
    }

    #[test]
    fn relative_path_display_is_the_text() {
        assert_eq!(RelPath::new("a\\b").to_string(), "a/b");
    }

    #[test]
    fn symbol_path_has_no_disambiguator_by_default() {
        let path = SymbolPath::new("mycrate::a::Point", SymbolKind::Struct);
        assert_eq!(path.disambiguator, 0);
        assert_eq!(path.path, "mycrate::a::Point");
        assert_eq!(path.kind, SymbolKind::Struct);
    }

    #[test]
    fn symbol_path_segments_are_named() {
        let path = SymbolPath::new("mycrate::a::b::run", SymbolKind::Function);
        assert_eq!(path.name(), "run");
        assert_eq!(path.crate_name(), "mycrate");

        let single = SymbolPath::new("mycrate", SymbolKind::Module);
        assert_eq!(single.name(), "mycrate");
        assert_eq!(single.crate_name(), "mycrate");
    }

    #[test]
    fn symbol_path_keeps_impl_segments_together() {
        let path = SymbolPath::new(
            "mycrate::a::<impl core::fmt::Display for Point<T>>::fmt",
            SymbolKind::Method,
        );
        assert_eq!(path.name(), "fmt");
        assert_eq!(path.crate_name(), "mycrate");

        let block = SymbolPath::new(
            "mycrate::a::<impl From<u8> for Wrapper<T>>",
            SymbolKind::Impl,
        );
        assert_eq!(block.name(), "<impl From<u8> for Wrapper<T>>");
        assert_eq!(block.crate_name(), "mycrate");
    }

    #[test]
    fn symbol_path_display_is_the_path() {
        let path = SymbolPath::new("mycrate::a::Point", SymbolKind::Struct);
        assert_eq!(path.to_string(), "mycrate::a::Point");
    }

    #[test]
    fn source_display_names_the_side() {
        let tree = Source::Tree {
            id: "abc123".to_owned(),
            label: "main".to_owned(),
        };
        assert_eq!(tree.to_string(), "main");
        assert_eq!(Source::Index.to_string(), "index");
        assert_eq!(Source::Worktree.to_string(), "working tree");
        let dir_path = Path::new("some").join("dir");
        let dir = Source::Dir(dir_path.clone());
        assert_eq!(dir.to_string(), dir_path.display().to_string());
    }

    #[test]
    fn analysis_options_default_is_unrestricted() {
        let options = AnalysisOptions::default();
        assert!(options.features.is_empty());
        assert!(!options.all_features);
        assert_eq!(options.target, None);
        assert_eq!(options.exec, ExecMode::Full);
        assert_eq!(options.manifest_dir, None);
    }

    #[test]
    fn analysis_key_hash_is_stable_and_hexadecimal() {
        let key = sample_key();
        let hash = key.hash();

        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash, hash.to_lowercase());
        assert_eq!(hash, sample_key().hash());
    }

    #[test]
    fn analysis_key_hash_changes_with_a_field() {
        let mut other = sample_key();
        other.features.push("extra".to_owned());
        assert_ne!(sample_key().hash(), other.hash());

        let mut changed_target = sample_key();
        changed_target.target = None;
        assert_ne!(sample_key().hash(), changed_target.hash());
    }
}
