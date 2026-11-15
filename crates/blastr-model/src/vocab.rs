//! Shared vocabulary types for Blastr.
//!
//! This crate holds the small enums that the rest of the workspace uses to
//! describe symbols, dependency edges, changes and their impact. Every enum
//! serialises to kebab-case strings and exposes the same spelling through
//! [`Display`](std::fmt::Display), so serialised data and human-readable output
//! never disagree.

use serde::{Deserialize, Serialize};

/// Generates the `ALL` constant, `as_str` and `Display` for a vocabulary enum.
macro_rules! model_enum {
    ($ty:ty, [$($variant:ident => $text:literal),* $(,)?]) => {
        impl $ty {
            /// Every variant of this enum, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant),*];

            /// The kebab-case name serde uses for this variant.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $text),*
                }
            }
        }

        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

/// The kind of definition a symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolKind {
    /// A free function.
    Function,
    /// A method in an inherent or trait implementation.
    Method,
    /// A struct definition.
    Struct,
    /// An enum definition.
    Enum,
    /// A union definition.
    Union,
    /// A trait definition.
    Trait,
    /// An implementation block.
    Impl,
    /// A named or tuple field.
    Field,
    /// An enum variant.
    Variant,
    /// A constant item.
    Const,
    /// A static item.
    Static,
    /// A type alias.
    TypeAlias,
    /// A module.
    Module,
    /// A macro definition.
    Macro,
}

model_enum!(
    SymbolKind,
    [
        Function => "function",
        Method => "method",
        Struct => "struct",
        Enum => "enum",
        Union => "union",
        Trait => "trait",
        Impl => "impl",
        Field => "field",
        Variant => "variant",
        Const => "const",
        Static => "static",
        TypeAlias => "type-alias",
        Module => "module",
        Macro => "macro",
    ]
);

/// A directed dependency edge from one symbol to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeKind {
    /// A statically resolved call.
    Calls,
    /// A function referenced without being called.
    TakesAsValue,
    /// A type mentioned in a signature, body or bound.
    UsesType,
    /// A read of a field.
    ReadsField,
    /// A write to a field.
    WritesField,
    /// A struct or variant literal.
    Constructs,
    /// A destructuring pattern or match arm.
    Matches,
    /// A trait implementation for a type.
    Implements,
    /// A call through a trait object or generic bound that could reach this
    /// implementation.
    MayDispatchTo,
    /// A read of a constant or static.
    UsesValue,
    /// Module, type or impl containing an item.
    Contains,
}

model_enum!(
    EdgeKind,
    [
        Calls => "calls",
        TakesAsValue => "takes-as-value",
        UsesType => "uses-type",
        ReadsField => "reads-field",
        WritesField => "writes-field",
        Constructs => "constructs",
        Matches => "matches",
        Implements => "implements",
        MayDispatchTo => "may-dispatch-to",
        UsesValue => "uses-value",
        Contains => "contains",
    ]
);

/// How precisely a dependency edge was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Certainty {
    /// Resolved to exactly one definition.
    Resolved,
    /// One of several possible targets.
    Candidate,
}

model_enum!(
    Certainty,
    [
        Resolved => "resolved",
        Candidate => "candidate",
    ]
);

/// What kind of change happened to a symbol between two revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    /// Whitespace, comments or formatting only.
    NonSemantic,
    /// The item body changed.
    Body,
    /// The item signature changed.
    Signature,
    /// The item visibility changed.
    Visibility,
    /// The item was added.
    Added,
    /// The item was removed.
    Removed,
    /// The item was renamed or moved.
    Renamed,
    /// A struct field was added, removed or retyped.
    Field,
    /// An enum variant was added, removed or reshaped.
    Variant,
    /// A trait item was added, removed or changed.
    TraitItem,
    /// A trait implementation was added or removed.
    Impl,
    /// A constant or static value or type changed.
    ConstValue,
    /// A type alias target changed.
    TypeAlias,
    /// A macro definition changed.
    MacroDef,
    /// An attribute on the item changed.
    Attribute,
    /// `Cargo.toml` features or dependency versions changed.
    Manifest,
}

model_enum!(
    ChangeKind,
    [
        NonSemantic => "non-semantic",
        Body => "body",
        Signature => "signature",
        Visibility => "visibility",
        Added => "added",
        Removed => "removed",
        Renamed => "renamed",
        Field => "field",
        Variant => "variant",
        TraitItem => "trait-item",
        Impl => "impl",
        ConstValue => "const-value",
        TypeAlias => "type-alias",
        MacroDef => "macro-def",
        Attribute => "attribute",
        Manifest => "manifest",
    ]
);

/// Why a dependent symbol is affected by a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImpactClass {
    /// The code refers to something whose shape changed and must be made
    /// compatible with it.
    Adaptation,
    /// The code is unchanged but may now behave differently.
    Behaviour,
}

model_enum!(
    ImpactClass,
    [
        Adaptation => "adaptation",
        Behaviour => "behaviour",
    ]
);

/// How strongly the analysis believes an impact is real.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    /// The impact may exist but the evidence is weak.
    Possible,
    /// The impact is probable but not proven.
    Likely,
    /// The impact follows directly from a resolved dependency.
    Definite,
}

model_enum!(
    Confidence,
    [
        Possible => "possible",
        Likely => "likely",
        Definite => "definite",
    ]
);

/// How much execution the analysis is allowed during a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecMode {
    /// Build scripts and procedural macros run.
    Full,
    /// Neither build scripts nor procedural macros run.
    Restricted,
}

model_enum!(
    ExecMode,
    [
        Full => "full",
        Restricted => "restricted",
    ]
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::fmt::{Debug, Display};

    /// Checks that a variant survives a JSON round-trip and that serde and
    /// `Display`/`as_str` agree on its spelling.
    fn check<T, F>(all: &[T], as_str: F)
    where
        T: Copy + Debug + Display + PartialEq + Serialize + DeserializeOwned,
        F: Fn(T) -> &'static str,
    {
        for &value in all {
            let text = as_str(value);
            assert_eq!(value.to_string(), text, "Display disagrees for {value:?}");

            let json = serde_json::to_string(&value).expect("serialisation must succeed");
            assert_eq!(
                json,
                format!("\"{text}\""),
                "serde disagrees with as_str for {value:?}"
            );

            let back: T = serde_json::from_str(&json).expect("deserialisation must succeed");
            assert_eq!(back, value, "round-trip changed {value:?}");
        }
    }

    #[test]
    fn symbol_kind_round_trips() {
        check(SymbolKind::ALL, |v| v.as_str());
    }

    #[test]
    fn edge_kind_round_trips() {
        check(EdgeKind::ALL, |v| v.as_str());
    }

    #[test]
    fn certainty_round_trips() {
        check(Certainty::ALL, |v| v.as_str());
    }

    #[test]
    fn change_kind_round_trips() {
        check(ChangeKind::ALL, |v| v.as_str());
    }

    #[test]
    fn impact_class_round_trips() {
        check(ImpactClass::ALL, |v| v.as_str());
    }

    #[test]
    fn confidence_round_trips() {
        check(Confidence::ALL, |v| v.as_str());
    }

    #[test]
    fn exec_mode_round_trips() {
        check(ExecMode::ALL, |v| v.as_str());
    }

    #[test]
    fn confidence_is_ordered() {
        assert!(Confidence::Possible < Confidence::Likely);
        assert!(Confidence::Likely < Confidence::Definite);
    }

    #[test]
    fn all_contains_every_variant() {
        assert_eq!(SymbolKind::ALL.len(), 14);
        assert_eq!(EdgeKind::ALL.len(), 11);
        assert_eq!(Certainty::ALL.len(), 2);
        assert_eq!(ChangeKind::ALL.len(), 16);
        assert_eq!(ImpactClass::ALL.len(), 2);
        assert_eq!(Confidence::ALL.len(), 3);
        assert_eq!(ExecMode::ALL.len(), 2);
    }
}
