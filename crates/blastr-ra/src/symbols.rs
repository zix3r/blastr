//! The symbol-shaped view of one rust-analyzer definition.
//!
//! [`Raw`] is a symbol before the ids are assigned: it holds everything except
//! the [`SymbolId`], which only exists once all symbols of the graph are known
//! and sorted. [`DefKey`] is the reverse mapping the dependency pass needs, and
//! [`SymbolTable`] keeps it.

use std::collections::HashMap;

use blastr_model::{ImplInfo, Location, SymbolFlags, SymbolId, SymbolKind, Visibility};
use ra_ap_hir::{
    Adt, AssocItem, Const, EnumVariant, Field, Function, Impl, Macro, Module, ModuleDef, Static,
    Trait, TypeAlias,
};
use ra_ap_syntax::ast;
use ra_ap_syntax::ast::{HasGenericParams, HasName};
use ra_ap_syntax::{AstNode, SyntaxNode};

use crate::hashing;

/// One extracted symbol, before ids are assigned.
///
/// Everything here is position and content, not identity: two revisions of the
/// same item produce the same [`Raw::path`] and different hashes.
#[derive(Debug, Clone)]
pub(crate) struct Raw {
    /// What the path names.
    pub(crate) kind: SymbolKind,
    /// Canonical path, see [`crate::paths`].
    pub(crate) path: String,
    /// Index into `RevisionGraph::crates`.
    pub(crate) krate: usize,
    /// Where the item is written.
    pub(crate) location: Location,
    /// Declared visibility.
    pub(crate) visibility: Visibility,
    /// Hash of the interface of the item.
    pub(crate) signature_hash: u64,
    /// Hash of the body or initialiser of the item.
    pub(crate) body_hash: u64,
    /// Hash of the attributes other than `derive` and `doc`.
    pub(crate) attr_hash: u64,
    /// Sketch of the body.
    pub(crate) body_sketch: Vec<u32>,
    /// The enclosing symbol, as an index into the list of [`Raw`]s.
    pub(crate) parent: Option<usize>,
    /// Facts about the symbol.
    pub(crate) flags: SymbolFlags,
    /// What an impl block implements.
    pub(crate) impl_info: Option<ImplInfo>,
}

impl Raw {
    /// Builds a symbol before its id is known.
    ///
    /// `flags` and `impl_info` start out empty and are filled in by the caller,
    /// which knows what the definition is.
    pub(crate) fn new(
        kind: SymbolKind,
        path: String,
        krate: usize,
        location: Location,
        visibility: Visibility,
        hashes: Hashes,
        parent: Option<usize>,
    ) -> Raw {
        Raw {
            kind,
            path,
            krate,
            location,
            visibility,
            signature_hash: hashes.signature,
            body_hash: hashes.body,
            attr_hash: hashes.attr,
            body_sketch: hashes.body_sketch,
            parent,
            flags: SymbolFlags::default(),
            impl_info: None,
        }
    }
}

/// A rust-analyzer definition that has (or may have) a symbol in the graph.
///
/// The dependency pass resolves a piece of code to a rust-analyzer definition
/// and then asks [`SymbolTable`] for the id this crate assigned to it:
///
/// ```ignore
/// let symbol = symbols.module_def(ModuleDef::Function(f)).map(|id| &graph.symbols[id.0 as usize]);
/// ```
///
/// Every variant is a `Copy` handle of the rust-analyzer definition, so the key
/// can be built from any definition the analysis produces, however it was
/// reached. Definitions that have no symbol in the graph (items of other
/// crates, builtin types, inactive `cfg` code) are simply absent from the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DefKey {
    /// A module, including the crate root.
    Module(Module),
    /// A function or method, including methods of derive-generated impls.
    Function(Function),
    /// A struct, enum or union definition.
    Adt(Adt),
    /// An enum variant.
    Variant(EnumVariant),
    /// A field of a struct, a union or an enum variant.
    Field(Field),
    /// A trait definition.
    Trait(Trait),
    /// An impl block, inherent or for a trait, including derive-generated ones.
    Impl(Impl),
    /// A constant item.
    Const(Const),
    /// A static item.
    Static(Static),
    /// A type alias.
    TypeAlias(TypeAlias),
    /// A `macro_rules!` or `macro` definition.
    Macro(Macro),
}

/// Constructors for the dependency pass, which sees definitions as
/// `ModuleDef`s and as `AssocItem`s rather than as concrete hir types.
#[allow(dead_code)]
impl DefKey {
    /// The key of a definition that can appear in a module's declarations.
    ///
    /// `None` for builtin types, which are not Rust items and have no symbol.
    pub(crate) fn from_module_def(def: ModuleDef) -> Option<DefKey> {
        Some(match def {
            ModuleDef::Module(it) => DefKey::Module(it),
            ModuleDef::Function(it) => DefKey::Function(it),
            ModuleDef::Adt(it) => DefKey::Adt(it),
            ModuleDef::EnumVariant(it) => DefKey::Variant(it),
            ModuleDef::Const(it) => DefKey::Const(it),
            ModuleDef::Static(it) => DefKey::Static(it),
            ModuleDef::Trait(it) => DefKey::Trait(it),
            ModuleDef::TypeAlias(it) => DefKey::TypeAlias(it),
            ModuleDef::Macro(it) => DefKey::Macro(it),
            ModuleDef::BuiltinType(_) => return None,
        })
    }

    /// The key of an item declared inside a trait or an impl block.
    pub(crate) fn from_assoc_item(item: AssocItem) -> DefKey {
        match item {
            AssocItem::Function(it) => DefKey::Function(it),
            AssocItem::Const(it) => DefKey::Const(it),
            AssocItem::TypeAlias(it) => DefKey::TypeAlias(it),
        }
    }
}

impl From<Module> for DefKey {
    fn from(it: Module) -> Self {
        DefKey::Module(it)
    }
}

impl From<Function> for DefKey {
    fn from(it: Function) -> Self {
        DefKey::Function(it)
    }
}

impl From<Adt> for DefKey {
    fn from(it: Adt) -> Self {
        DefKey::Adt(it)
    }
}

impl From<EnumVariant> for DefKey {
    fn from(it: EnumVariant) -> Self {
        DefKey::Variant(it)
    }
}

impl From<Field> for DefKey {
    fn from(it: Field) -> Self {
        DefKey::Field(it)
    }
}

impl From<Trait> for DefKey {
    fn from(it: Trait) -> Self {
        DefKey::Trait(it)
    }
}

impl From<Impl> for DefKey {
    fn from(it: Impl) -> Self {
        DefKey::Impl(it)
    }
}

impl From<Const> for DefKey {
    fn from(it: Const) -> Self {
        DefKey::Const(it)
    }
}

impl From<Static> for DefKey {
    fn from(it: Static) -> Self {
        DefKey::Static(it)
    }
}

impl From<TypeAlias> for DefKey {
    fn from(it: TypeAlias) -> Self {
        DefKey::TypeAlias(it)
    }
}

impl From<Macro> for DefKey {
    fn from(it: Macro) -> Self {
        DefKey::Macro(it)
    }
}

/// The id of every symbol of the graph, by definition.
///
/// Built by [`crate::extract`] once the ids are known. A missing key means the
/// definition produced no symbol: it belongs to another crate, or to code that
/// is inactive under the analysed configuration.
#[derive(Debug, Default)]
pub(crate) struct SymbolTable {
    by_def: HashMap<DefKey, SymbolId>,
}

/// The lookup the dependency pass uses.
///
/// The dependency edges are part of the next stage, so some of this is not
/// called yet; it is kept here because it is the only way to go from a
/// definition back to the symbol the extraction assigned to it.
#[allow(dead_code)]
impl SymbolTable {
    /// The symbol of `key`, if that definition produced one.
    pub(crate) fn get(&self, key: DefKey) -> Option<SymbolId> {
        self.by_def.get(&key).copied()
    }

    /// The symbol of a definition found in a module's declarations.
    pub(crate) fn module_def(&self, def: ModuleDef) -> Option<SymbolId> {
        DefKey::from_module_def(def).and_then(|key| self.get(key))
    }

    /// The symbol of an item declared inside a trait or an impl block.
    pub(crate) fn assoc_item(&self, item: AssocItem) -> Option<SymbolId> {
        self.get(DefKey::from_assoc_item(item))
    }

    /// Records the symbol of `key`.
    pub(crate) fn insert(&mut self, key: DefKey, id: SymbolId) {
        self.by_def.insert(key, id);
    }
}

/// The hashes of one item.
#[derive(Debug, Clone)]
pub(crate) struct Hashes {
    /// Hash of the interface of the item.
    pub(crate) signature: u64,
    /// Hash of the body or initialiser of the item.
    pub(crate) body: u64,
    /// Hash of the attributes other than `derive` and `doc`.
    pub(crate) attr: u64,
    /// Sketch of the body.
    pub(crate) body_sketch: Vec<u32>,
    /// Whether the item has a body or an initialiser.
    pub(crate) has_body: bool,
    /// Whether the item carries a test attribute.
    pub(crate) is_test: bool,
}

impl Hashes {
    /// The hashes of an item that has none.
    pub(crate) fn none() -> Hashes {
        Hashes {
            signature: 0,
            body: 0,
            attr: 0,
            body_sketch: Vec::new(),
            has_body: false,
            is_test: false,
        }
    }
}

/// The hashes of a module, which has none.
pub(crate) fn module_hashes() -> Hashes {
    Hashes::none()
}

/// The hashes of an impl block: the header, without the items it contains.
///
/// `None` for an impl block that has no source of its own, which happens for
/// impl blocks generated by a derive.
pub(crate) fn impl_hashes(item: &ast::Impl) -> Hashes {
    let lists = item.assoc_item_list().map(|list| list.syntax().clone());
    header_hashes(item.syntax(), lists.as_ref())
}

/// The hashes of a struct, enum or union: the header, without the fields, the
/// variants or the items they contain.
pub(crate) fn adt_hashes(kind: SymbolKind, node: &SyntaxNode) -> Hashes {
    let list = match kind {
        SymbolKind::Struct => ast::Struct::cast(node.clone())
            .and_then(|it| it.field_list())
            .map(|it| it.syntax().clone()),
        SymbolKind::Union => ast::Union::cast(node.clone())
            .and_then(|it| it.record_field_list())
            .map(|it| it.syntax().clone()),
        SymbolKind::Enum => ast::Enum::cast(node.clone())
            .and_then(|it| it.variant_list())
            .map(|it| it.syntax().clone()),
        _ => None,
    };
    header_hashes(node, list.as_ref())
}

/// The hashes of a trait: generics, supertraits and where clause.
pub(crate) fn trait_hashes(item: &ast::Trait) -> Hashes {
    let list = item.assoc_item_list().map(|list| list.syntax().clone());
    header_hashes(item.syntax(), list.as_ref())
}

/// The hashes of a function or method.
pub(crate) fn function_hashes(item: &ast::Fn) -> Hashes {
    let body = item.body();
    item_hashes(item.syntax(), body.as_ref().map(|it| it.syntax()))
}

/// The hashes of a constant: the type, and the initialiser as the body.
pub(crate) fn const_hashes(item: &ast::Const) -> Hashes {
    let body = item.body();
    item_hashes(item.syntax(), body.as_ref().map(|it| it.syntax()))
}

/// The hashes of a static: the type, and the initialiser as the body.
pub(crate) fn static_hashes(item: &ast::Static) -> Hashes {
    let body = item.body();
    item_hashes(item.syntax(), body.as_ref().map(|it| it.syntax()))
}

/// The hashes of a type alias: the whole item except attributes and visibility.
pub(crate) fn type_alias_hashes(item: &ast::TypeAlias) -> Hashes {
    header_hashes(item.syntax(), None)
}

/// The hashes of an enum variant: the fields and the discriminant of the
/// variant, without its name.
pub(crate) fn variant_hashes(item: &ast::Variant) -> Hashes {
    header_hashes(item.syntax(), None)
}

/// The hashes of a struct, union or variant field: the type.
pub(crate) fn field_hashes(node: &SyntaxNode, ty: Option<ast::Type>) -> Hashes {
    let signature = ty
        .as_ref()
        .map(|ty| hashing::hash(&hashing::tokens(ty.syntax(), &[])))
        .unwrap_or(0);
    Hashes {
        signature,
        body: 0,
        attr: hashing::attr_hash(node),
        body_sketch: Vec::new(),
        has_body: false,
        is_test: false,
    }
}

/// Whether the type of `field` is the only use of one of the generic parameters
/// of the struct or union that declares it.
///
/// Only the field types keep a parameter alive: a bound or a `where` clause does
/// not, which is why the compiler reports `E0392` for a parameter no field
/// names. A field of an enum variant is never such a use: the parameter belongs
/// to the enum, whose declaration is the item that would have to change.
pub(crate) fn sole_parameter_use(field: &SyntaxNode, ty: &SyntaxNode) -> bool {
    let Some(list) = field.parent().and_then(ast::FieldList::cast) else {
        return false;
    };
    let Some(params) = generic_parameter_names(list.syntax().parent().as_ref()) else {
        return false;
    };
    let mentioned: Vec<&String> = params
        .iter()
        .filter(|name| mentions_parameter(ty, name))
        .collect();
    if mentioned.is_empty() {
        return false;
    }
    let others: Vec<SyntaxNode> = field_types(&list)
        .into_iter()
        .filter(|other| other != ty)
        .collect();
    mentioned
        .into_iter()
        .any(|name| !others.iter().any(|other| mentions_parameter(other, name)))
}

/// The names of the generic parameters of a struct or union declaration, as the
/// tokens that name them: `T`, `N`, `'a`.
///
/// `None` for anything else, including an enum: a field of a variant does not
/// declare the parameters it uses.
fn generic_parameter_names(container: Option<&SyntaxNode>) -> Option<Vec<String>> {
    let container = container?;
    let list = ast::Struct::cast(container.clone())
        .and_then(|item| item.generic_param_list())
        .or_else(|| {
            ast::Union::cast(container.clone()).and_then(|item| item.generic_param_list())
        })?;
    Some(
        list.generic_params()
            .filter_map(|param| match param {
                ast::GenericParam::LifetimeParam(param) => param
                    .lifetime()
                    .and_then(|lifetime| lifetime.lifetime_ident_token())
                    .map(|token| token.text().to_owned()),
                ast::GenericParam::TypeParam(param) => {
                    param.name().map(|name| name.text().to_owned())
                }
                ast::GenericParam::ConstParam(param) => {
                    param.name().map(|name| name.text().to_owned())
                }
            })
            .collect(),
    )
}

/// The type of every field of a field list, in source order.
fn field_types(list: &ast::FieldList) -> Vec<SyntaxNode> {
    match list {
        ast::FieldList::RecordFieldList(list) => list
            .fields()
            .filter_map(|field| field.ty())
            .map(|ty| ty.syntax().clone())
            .collect(),
        ast::FieldList::TupleFieldList(list) => list
            .fields()
            .filter_map(|field| field.ty())
            .map(|ty| ty.syntax().clone())
            .collect(),
    }
}

/// Whether `node` names the generic parameter `name`.
///
/// The names are compared token by token, so a parameter `T` is not found in a
/// type called `Time`.
fn mentions_parameter(node: &SyntaxNode, name: &str) -> bool {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.text() == name)
}

/// The hashes of a macro definition: the rules as the body.
///
/// The signature is empty: the name of a macro is the only thing but the rules,
/// and the name is already in the symbol's path.
pub(crate) fn macro_hashes(item: &ast::Macro) -> Hashes {
    let body = match item {
        ast::Macro::MacroRules(it) => it.token_tree().map(|it| it.syntax().clone()),
        ast::Macro::MacroDef(it) => it.body().map(|it| it.syntax().clone()),
    };
    let body_tokens = body
        .as_ref()
        .map(|node| hashing::tokens(node, &[]))
        .unwrap_or_default();
    Hashes {
        signature: 0,
        body: hashing::hash(&body_tokens),
        attr: hashing::attr_hash(item.syntax()),
        body_sketch: hashing::sketch(&body_tokens),
        has_body: body.is_some(),
        is_test: false,
    }
}

/// Hashes of an item made of a header and one skipped block: attributes,
/// visibility, the item's own name and `list` are skipped, the rest is the
/// signature.
fn header_hashes(node: &SyntaxNode, list: Option<&SyntaxNode>) -> Hashes {
    let mut skip = hashing::skipped_children(node);
    skip.extend(list.cloned());
    skip.extend(hashing::name_child(node));
    Hashes {
        signature: hashing::hash(&hashing::tokens(node, &skip)),
        body: 0,
        attr: hashing::attr_hash(node),
        body_sketch: Vec::new(),
        has_body: false,
        is_test: false,
    }
}

/// Hashes of an item whose body (or initialiser) is one optional subtree.
fn item_hashes(node: &SyntaxNode, body: Option<&SyntaxNode>) -> Hashes {
    let mut skip = hashing::skipped_children(node);
    skip.extend(hashing::name_child(node));
    skip.extend(body.cloned());
    let body_tokens = body
        .map(|body| hashing::tokens(body, &[]))
        .unwrap_or_default();
    Hashes {
        signature: hashing::hash(&hashing::tokens(node, &skip)),
        body: hashing::hash(&body_tokens),
        attr: hashing::attr_hash(node),
        body_sketch: hashing::sketch(&body_tokens),
        has_body: body.is_some(),
        is_test: hashing::has_test_attr(node),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ra_ap_syntax::{Edition, SourceFile};

    /// The first item of a snippet, as the type the test needs.
    fn item<T: AstNode>(text: &str) -> T {
        SourceFile::parse(text, Edition::CURRENT)
            .tree()
            .syntax()
            .children()
            .find_map(T::cast)
            .expect("the snippet holds one item of the requested kind")
    }

    /// The first variant of an enum snippet.
    fn variant(text: &str) -> ast::Variant {
        item::<ast::Enum>(text)
            .variant_list()
            .and_then(|list| list.variants().next())
            .expect("the snippet holds one variant")
    }

    /// The first field of a named-field struct snippet.
    fn field(text: &str) -> ast::RecordField {
        let list = match item::<ast::Struct>(text).field_list() {
            Some(ast::FieldList::RecordFieldList(list)) => list,
            _ => panic!("the snippet holds one named field"),
        };
        list.fields().next().expect("the snippet holds one field")
    }

    #[test]
    fn a_function_signature_and_body_ignore_the_name() {
        let first = function_hashes(&item("fn first(x: u8) -> u8 { x + 1 }"));
        let second = function_hashes(&item("fn second(x: u8) -> u8 { x + 1 }"));
        assert_eq!(first.signature, second.signature);
        assert_eq!(first.body, second.body);
        assert_eq!(first.body_sketch, second.body_sketch);
    }

    #[test]
    fn parameter_names_stay_in_the_signature() {
        let first = function_hashes(&item("fn f(x: u8) -> u8 { x }"));
        let second = function_hashes(&item("fn f(value: u8) -> u8 { value }"));
        assert_ne!(first.signature, second.signature);
    }

    #[test]
    fn a_constant_signature_ignores_the_name() {
        let first = const_hashes(&item("const FIRST: u8 = 1;"));
        let second = const_hashes(&item("const SECOND: u8 = 1;"));
        assert_eq!(first.signature, second.signature);
        assert_ne!(first.body, 0);
    }

    #[test]
    fn a_struct_header_ignores_the_name() {
        let first = adt_hashes(
            SymbolKind::Struct,
            item::<ast::Struct>("struct First { value: u8 }").syntax(),
        );
        let second = adt_hashes(
            SymbolKind::Struct,
            item::<ast::Struct>("struct Second { value: u8 }").syntax(),
        );
        assert_eq!(first.signature, second.signature);
    }

    #[test]
    fn a_variant_hashes_its_fields_without_its_name() {
        let first = variant_hashes(&variant("enum E { First(u8) }"));
        let second = variant_hashes(&variant("enum E { Second(u8) }"));
        let widened = variant_hashes(&variant("enum E { First(u16) }"));
        assert_eq!(first.signature, second.signature);
        assert_ne!(first.signature, widened.signature);
    }

    #[test]
    fn a_macro_signature_is_empty() {
        let first = macro_hashes(&item("macro_rules! first { () => { 1 }; }"));
        let second = macro_hashes(&item("macro_rules! second { () => { 1 }; }"));
        assert_eq!(first.signature, 0);
        assert_eq!(second.signature, 0);
        assert_eq!(first.body, second.body);
        assert_ne!(first.body, 0);
    }

    #[test]
    fn a_field_hashes_only_its_type() {
        let first = field("struct S { value: u8 }");
        let second = field("struct S { other: u8 }");
        let first = field_hashes(first.syntax(), first.ty());
        let second = field_hashes(second.syntax(), second.ty());
        assert_eq!(first.signature, second.signature);
        assert_ne!(first.signature, 0);
    }

    /// The lone-use test for the named field of a struct snippet.
    fn sole(text: &str, name: &str) -> bool {
        let item = item::<ast::Struct>(text);
        let Some(ast::FieldList::RecordFieldList(list)) = item.field_list() else {
            panic!("the snippet holds named fields");
        };
        let field = list
            .fields()
            .find(|field| {
                field
                    .name()
                    .is_some_and(|field_name| field_name.text() == name)
            })
            .expect("the snippet holds the named field");
        sole_parameter_use(
            field.syntax(),
            field.ty().expect("the field has a type").syntax(),
        )
    }

    /// The lone-use test for the first field of a tuple struct snippet.
    fn sole_in_tuple(text: &str) -> bool {
        let item = item::<ast::Struct>(text);
        let Some(ast::FieldList::TupleFieldList(list)) = item.field_list() else {
            panic!("the snippet holds tuple fields");
        };
        let field = list.fields().next().expect("the snippet holds a field");
        sole_parameter_use(
            field.syntax(),
            field.ty().expect("the field has a type").syntax(),
        )
    }

    #[test]
    fn a_field_that_keeps_a_parameter_alive_is_recognised() {
        assert!(sole(
            "struct S<'a> { lines: Lines<'a>, count: usize }",
            "lines"
        ));
        assert!(sole("struct S<A, B> { first: A, second: B }", "first"));
        assert!(sole("struct S<T> { value: Vec<T> }", "value"));
        assert!(sole_in_tuple("struct S<T>(Vec<T>);"));
    }

    #[test]
    fn a_field_another_field_also_uses_is_not_a_lone_use() {
        assert!(!sole("struct S<T> { left: T, right: T }", "left"));
        assert!(!sole("struct S<'a> { a: &'a str, b: &'a str }", "a"));
        assert!(!sole("struct S<T> { left: Vec<T>, right: T }", "right"));
        assert!(!sole("struct S { value: Time }", "value"));
        assert!(!sole("struct S<T> { value: u8, unused: T }", "value"));
    }
}
