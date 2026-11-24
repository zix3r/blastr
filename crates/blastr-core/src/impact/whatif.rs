//! The change a what-if query asks the analysis to assume.
//!
//! A user points at one symbol of the current revision and says what would
//! happen if it changed. The change is built from the symbol's kind and
//! flags, so the rest of the analysis needs no special cases.

use blastr_model::{
    Assume, Change, ChangeDetail, ChangeKind, Delta, RevisionGraph, Symbol, SymbolKind, SymbolPath,
    Visibility,
};

/// What assuming `body` needs.
const BODY_EXPECTED: &str = "a function, a method, a const, a static or a macro";
/// What assuming `signature` needs.
const SIGNATURE_EXPECTED: &str = "a function, a method, a type or a type alias";
/// What the field assumptions need.
const FIELD_EXPECTED: &str = "a field";
/// What assuming `field-added` needs.
const OWNER_EXPECTED: &str = "a struct or a variant";
/// What assuming `variant-added` needs.
const ENUM_EXPECTED: &str = "an enum";
/// What assuming `variant-removed` needs.
const VARIANT_EXPECTED: &str = "a variant";

/// Why a what-if query cannot be turned into a change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WhatIfError {
    /// The working tree has no symbol with the given path.
    #[error("`{0}` is not in the working tree")]
    NotFound(String),
    /// The symbol exists but cannot undergo the assumed change.
    #[error("`{symbol}` is a {kind}; assuming `{assume}` needs {expected}")]
    WrongKind {
        /// The symbol's path text.
        symbol: String,
        /// What the symbol is.
        kind: SymbolKind,
        /// The assumed change, as its kebab-case name.
        assume: &'static str,
        /// What the assumed change applies to.
        expected: &'static str,
    },
}

/// The change to propagate for a what-if query on `graph`.
pub fn assumed_change(
    graph: &RevisionGraph,
    target: &SymbolPath,
    assume: Assume,
) -> Result<Change, WhatIfError> {
    let Some(symbol) = graph.symbols.iter().find(|symbol| symbol.path == *target) else {
        return Err(WhatIfError::NotFound(target.path.clone()));
    };

    let (kind, detail) = match assume {
        Assume::Body => match symbol.path.kind {
            SymbolKind::Function | SymbolKind::Method if symbol.flags.in_trait => {
                (ChangeKind::TraitItem, ChangeDetail::DefaultBody)
            }
            SymbolKind::Function | SymbolKind::Method => (ChangeKind::Body, ChangeDetail::None),
            SymbolKind::Const | SymbolKind::Static => (ChangeKind::ConstValue, ChangeDetail::None),
            SymbolKind::Macro => (ChangeKind::MacroDef, ChangeDetail::None),
            _ => return Err(wrong_kind(symbol, assume, BODY_EXPECTED)),
        },
        Assume::Signature => match symbol.path.kind {
            SymbolKind::Function | SymbolKind::Method if symbol.flags.in_trait => {
                (ChangeKind::TraitItem, ChangeDetail::Member(Delta::Modified))
            }
            SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Union
            | SymbolKind::Trait
            | SymbolKind::Impl => (ChangeKind::Signature, ChangeDetail::None),
            SymbolKind::Const | SymbolKind::Static => {
                (ChangeKind::ConstValue, ChangeDetail::Retyped)
            }
            SymbolKind::TypeAlias => (ChangeKind::TypeAlias, ChangeDetail::None),
            _ => return Err(wrong_kind(symbol, assume, SIGNATURE_EXPECTED)),
        },
        Assume::Removed => match symbol.path.kind {
            SymbolKind::Field => (ChangeKind::Field, ChangeDetail::Member(Delta::Removed)),
            SymbolKind::Variant => (ChangeKind::Variant, ChangeDetail::Member(Delta::Removed)),
            SymbolKind::Impl => (ChangeKind::Impl, ChangeDetail::Member(Delta::Removed)),
            _ if symbol.flags.in_trait => {
                (ChangeKind::TraitItem, ChangeDetail::Member(Delta::Removed))
            }
            _ => (ChangeKind::Removed, ChangeDetail::None),
        },
        Assume::Visibility => (
            ChangeKind::Visibility,
            ChangeDetail::Visibility {
                from: symbol.visibility,
                to: Visibility::Private,
            },
        ),
        Assume::FieldRemoved => match symbol.path.kind {
            SymbolKind::Field => (ChangeKind::Field, ChangeDetail::Member(Delta::Removed)),
            _ => return Err(wrong_kind(symbol, assume, FIELD_EXPECTED)),
        },
        Assume::FieldRetyped => match symbol.path.kind {
            SymbolKind::Field => (ChangeKind::Field, ChangeDetail::Member(Delta::Modified)),
            _ => return Err(wrong_kind(symbol, assume, FIELD_EXPECTED)),
        },
        Assume::FieldAdded => match symbol.path.kind {
            SymbolKind::Struct | SymbolKind::Variant => {
                (ChangeKind::Field, ChangeDetail::Member(Delta::Added))
            }
            _ => return Err(wrong_kind(symbol, assume, OWNER_EXPECTED)),
        },
        Assume::VariantAdded => match symbol.path.kind {
            SymbolKind::Enum => (ChangeKind::Variant, ChangeDetail::Member(Delta::Added)),
            _ => return Err(wrong_kind(symbol, assume, ENUM_EXPECTED)),
        },
        Assume::VariantRemoved => match symbol.path.kind {
            SymbolKind::Variant => (ChangeKind::Variant, ChangeDetail::Member(Delta::Removed)),
            _ => return Err(wrong_kind(symbol, assume, VARIANT_EXPECTED)),
        },
    };

    let added = match assume {
        Assume::FieldAdded => Some(SymbolKind::Field),
        Assume::VariantAdded => Some(SymbolKind::Variant),
        _ => None,
    };
    let symbol_path = match added {
        Some(kind) => SymbolPath::new(format!("{}::<new>", symbol.path.path), kind),
        None => symbol.path.clone(),
    };
    let mut change = Change {
        kind,
        symbol: symbol_path,
        old: Some(symbol.location.clone()),
        new: Some(symbol.location.clone()),
        detail,
    };
    if kind == ChangeKind::Removed {
        change.new = None;
    }
    Ok(change)
}

/// The error for an assumed change that does not apply to `symbol`.
fn wrong_kind(symbol: &Symbol, assume: Assume, expected: &'static str) -> WhatIfError {
    WhatIfError::WrongKind {
        symbol: symbol.path.path.clone(),
        kind: symbol.path.kind,
        assume: assume.as_str(),
        expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{CrateId, Location, RelPath, Span, SymbolFlags};

    /// A graph holding one public symbol in `src/lib.rs`.
    fn graph_with(path: &str, kind: SymbolKind, in_trait: bool) -> RevisionGraph {
        let mut graph = RevisionGraph::default();
        graph.symbols.push(Symbol {
            path: SymbolPath::new(path, kind),
            krate: CrateId(0),
            location: Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 10,
                    start_line: 1,
                    end_line: 1,
                },
            },
            visibility: Visibility::Crate,
            signature_hash: 0,
            body_hash: 0,
            attr_hash: 0,
            body_sketch: Vec::new(),
            parent: None,
            flags: SymbolFlags {
                in_trait,
                ..SymbolFlags::default()
            },
            impl_info: None,
        });
        graph
    }

    /// The change assumed for a symbol outside a trait.
    fn check(path: &str, kind: SymbolKind, assume: Assume) -> Change {
        let graph = graph_with(path, kind, false);
        assumed_change(&graph, &SymbolPath::new(path, kind), assume)
            .expect("the assumption applies")
    }

    /// The change assumed for a symbol declared in a trait.
    fn check_in_trait(path: &str, kind: SymbolKind, assume: Assume) -> Change {
        let graph = graph_with(path, kind, true);
        assumed_change(&graph, &SymbolPath::new(path, kind), assume)
            .expect("the assumption applies")
    }

    /// The message for an assumption that does not apply.
    fn rejected(path: &str, kind: SymbolKind, assume: Assume) -> String {
        let graph = graph_with(path, kind, false);
        let error = assumed_change(&graph, &SymbolPath::new(path, kind), assume)
            .expect_err("the assumption is rejected");
        error.to_string()
    }

    #[test]
    fn body_of_a_function_changes_the_body() {
        let change = check("krate::run", SymbolKind::Function, Assume::Body);
        assert_eq!(change.kind, ChangeKind::Body);
        assert_eq!(change.detail, ChangeDetail::None);
    }

    #[test]
    fn body_of_a_trait_method_changes_the_default_body() {
        let change = check_in_trait("krate::Trait::run", SymbolKind::Method, Assume::Body);
        assert_eq!(change.kind, ChangeKind::TraitItem);
        assert_eq!(change.detail, ChangeDetail::DefaultBody);
    }

    #[test]
    fn body_of_a_const_or_static_changes_its_value() {
        for kind in [SymbolKind::Const, SymbolKind::Static] {
            let change = check("krate::LIMIT", kind, Assume::Body);
            assert_eq!(change.kind, ChangeKind::ConstValue);
        }
    }

    #[test]
    fn body_of_a_macro_changes_the_definition() {
        let change = check("krate::checked", SymbolKind::Macro, Assume::Body);
        assert_eq!(change.kind, ChangeKind::MacroDef);
    }

    #[test]
    fn signature_of_a_function_or_type_changes_the_signature() {
        let kinds = [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Union,
            SymbolKind::Trait,
            SymbolKind::Impl,
        ];
        for kind in kinds {
            let change = check("krate::item", kind, Assume::Signature);
            assert_eq!(change.kind, ChangeKind::Signature, "for {kind}");
        }
    }

    #[test]
    fn signature_of_a_trait_method_modifies_the_trait_item() {
        let change = check_in_trait("krate::Trait::run", SymbolKind::Method, Assume::Signature);
        assert_eq!(change.kind, ChangeKind::TraitItem);
        assert_eq!(change.detail, ChangeDetail::Member(Delta::Modified));
    }

    #[test]
    fn signature_of_a_const_or_static_retypes_it() {
        for kind in [SymbolKind::Const, SymbolKind::Static] {
            let change = check("krate::LIMIT", kind, Assume::Signature);
            assert_eq!(change.detail, ChangeDetail::Retyped);
        }
    }

    #[test]
    fn signature_of_a_type_alias_changes_the_alias() {
        let change = check("krate::Amount", SymbolKind::TypeAlias, Assume::Signature);
        assert_eq!(change.kind, ChangeKind::TypeAlias);
    }

    #[test]
    fn removed_field_is_a_field_change() {
        let change = check("krate::Point::x", SymbolKind::Field, Assume::Removed);
        assert_eq!(change.kind, ChangeKind::Field);
        assert_eq!(change.detail, ChangeDetail::Member(Delta::Removed));
    }

    #[test]
    fn removed_variant_is_a_variant_change() {
        let change = check("krate::Colour::Red", SymbolKind::Variant, Assume::Removed);
        assert_eq!(change.kind, ChangeKind::Variant);
        assert_eq!(change.detail, ChangeDetail::Member(Delta::Removed));
    }

    #[test]
    fn removed_impl_is_an_impl_change() {
        let change = check("krate::Point", SymbolKind::Impl, Assume::Removed);
        assert_eq!(change.kind, ChangeKind::Impl);
    }

    #[test]
    fn removed_trait_item_is_a_trait_item_change() {
        let change = check_in_trait("krate::Trait::run", SymbolKind::Function, Assume::Removed);
        assert_eq!(change.kind, ChangeKind::TraitItem);
    }

    #[test]
    fn removed_other_item_has_no_head_side_location() {
        let change = check("krate::run", SymbolKind::Function, Assume::Removed);
        assert_eq!(change.kind, ChangeKind::Removed);
        assert!(change.old.is_some());
        assert_eq!(change.new, None);
    }

    #[test]
    fn visibility_narrows_to_private() {
        let change = check("krate::Point", SymbolKind::Struct, Assume::Visibility);
        assert_eq!(change.kind, ChangeKind::Visibility);
        assert_eq!(
            change.detail,
            ChangeDetail::Visibility {
                from: Visibility::Crate,
                to: Visibility::Private,
            }
        );
    }

    #[test]
    fn field_removed_applies_to_a_field() {
        let change = check("krate::Point::x", SymbolKind::Field, Assume::FieldRemoved);
        assert_eq!(change.kind, ChangeKind::Field);
        assert_eq!(change.detail, ChangeDetail::Member(Delta::Removed));
    }

    #[test]
    fn field_retyped_modifies_a_field() {
        let change = check("krate::Point::x", SymbolKind::Field, Assume::FieldRetyped);
        assert_eq!(change.detail, ChangeDetail::Member(Delta::Modified));
    }

    #[test]
    fn field_added_names_a_new_field_of_a_struct_or_variant() {
        for kind in [SymbolKind::Struct, SymbolKind::Variant] {
            let change = check("krate::Point", kind, Assume::FieldAdded);
            assert_eq!(change.kind, ChangeKind::Field);
            assert_eq!(change.symbol.path, "krate::Point::<new>");
            assert_eq!(change.symbol.kind, SymbolKind::Field);
        }
    }

    #[test]
    fn variant_added_names_a_new_variant_of_an_enum() {
        let change = check("krate::Colour", SymbolKind::Enum, Assume::VariantAdded);
        assert_eq!(change.kind, ChangeKind::Variant);
        assert_eq!(change.detail, ChangeDetail::Member(Delta::Added));
        assert_eq!(change.symbol.path, "krate::Colour::<new>");
        assert_eq!(change.symbol.kind, SymbolKind::Variant);
    }

    #[test]
    fn variant_removed_removes_a_variant() {
        let change = check(
            "krate::Colour::Red",
            SymbolKind::Variant,
            Assume::VariantRemoved,
        );
        assert_eq!(change.kind, ChangeKind::Variant);
        assert_eq!(change.detail, ChangeDetail::Member(Delta::Removed));
    }

    #[test]
    fn body_of_a_struct_is_rejected() {
        assert_eq!(
            rejected("krate::Point", SymbolKind::Struct, Assume::Body),
            format!("`krate::Point` is a struct; assuming `body` needs {BODY_EXPECTED}")
        );
    }

    #[test]
    fn signature_of_a_field_is_rejected() {
        assert_eq!(
            rejected("krate::Point::x", SymbolKind::Field, Assume::Signature),
            format!(
                "`krate::Point::x` is a field; assuming `signature` needs {SIGNATURE_EXPECTED}"
            )
        );
    }

    #[test]
    fn variant_removed_on_a_struct_is_rejected() {
        assert_eq!(
            rejected("krate::Point", SymbolKind::Struct, Assume::VariantRemoved),
            format!(
                "`krate::Point` is a struct; assuming `variant-removed` needs {VARIANT_EXPECTED}"
            )
        );
    }

    #[test]
    fn an_unknown_symbol_is_not_found() {
        let graph = graph_with("krate::Point", SymbolKind::Struct, false);
        let missing = SymbolPath::new("krate::Missing", SymbolKind::Struct);
        let error = assumed_change(&graph, &missing, Assume::Body).expect_err("no symbol");
        assert_eq!(error, WhatIfError::NotFound("krate::Missing".to_owned()));
    }

    #[test]
    fn the_path_must_match_exactly() {
        // A shorter path or another kind names a different symbol; nothing is guessed.
        let graph = graph_with("krate::Point", SymbolKind::Struct, false);
        let short = SymbolPath::new("Point", SymbolKind::Struct);
        assert!(assumed_change(&graph, &short, Assume::Body).is_err());
        let other_kind = SymbolPath::new("krate::Point", SymbolKind::Enum);
        assert!(assumed_change(&graph, &other_kind, Assume::Signature).is_err());
    }
}
