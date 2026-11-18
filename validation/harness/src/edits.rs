//! The seeded edits of the compiler check, cut out of a real syntax tree.
//!
//! Every edit changes exactly one definition and nothing else. The position of
//! the text to replace comes from `ra_ap_syntax` — the same parser and the same
//! version the analysed build uses — so the replacement lands in the parameter
//! list, the field list, the variant list or the visibility of the item, and the
//! result is always syntactically valid Rust. A generator returns `None` when
//! the item cannot take that edit (a unit-returning function cannot have its
//! return type changed, a tuple struct has no field list), and the case is then
//! simply not planned.

use std::ops::Range;

use blastr_model::{RelPath, Span, Symbol, SymbolKind, Visibility};
use ra_ap_syntax::{AstNode, SourceFile, SyntaxKind, SyntaxNode, TextRange, TextSize, ast};

use crate::{Error, Result};

/// The seeded edits the compiler check applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EditKind {
    /// `fn f(a: i64)` becomes `fn f(a: i64, __blastr_extra: i64)`.
    AddParameter,
    /// `-> T` becomes `-> (T,)`, so every caller sees a different type.
    ChangeReturnType,
    /// A struct loses one field.
    RemoveField,
    /// A field's type `T` becomes `(T,)`.
    RetypeField,
    /// A struct gains a field.
    AddField,
    /// An enum gains a variant.
    AddVariant,
    /// An enum loses one variant.
    RemoveVariant,
    /// A trait gains a required method, so every implementation is incomplete.
    AddRequiredTraitMethod,
    /// A free function disappears.
    RemoveFunction,
    /// `pub` becomes `pub(crate)`, or `pub(crate)` nothing.
    NarrowVisibility,
}

/// Every edit kind, in the order cases are planned and reported.
pub const EDIT_KINDS: [EditKind; 10] = [
    EditKind::AddParameter,
    EditKind::ChangeReturnType,
    EditKind::RemoveField,
    EditKind::RetypeField,
    EditKind::AddField,
    EditKind::AddVariant,
    EditKind::RemoveVariant,
    EditKind::AddRequiredTraitMethod,
    EditKind::RemoveFunction,
    EditKind::NarrowVisibility,
];

impl EditKind {
    /// The name used in reports and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            EditKind::AddParameter => "add-parameter",
            EditKind::ChangeReturnType => "change-return-type",
            EditKind::RemoveField => "remove-field",
            EditKind::RetypeField => "retype-field",
            EditKind::AddField => "add-field",
            EditKind::AddVariant => "add-variant",
            EditKind::RemoveVariant => "remove-variant",
            EditKind::AddRequiredTraitMethod => "add-required-trait-method",
            EditKind::RemoveFunction => "remove-function",
            EditKind::NarrowVisibility => "narrow-visibility",
        }
    }

    /// The kind with this name, if there is one.
    pub fn parse(text: &str) -> Option<EditKind> {
        EDIT_KINDS.into_iter().find(|kind| kind.as_str() == text)
    }

    /// Whether this edit can apply to a symbol of that kind, before the syntax
    /// is looked at.
    fn applies_to(self, kind: SymbolKind) -> bool {
        match self {
            EditKind::AddParameter | EditKind::ChangeReturnType | EditKind::RemoveFunction => {
                matches!(kind, SymbolKind::Function | SymbolKind::Method)
            }
            EditKind::RemoveField | EditKind::RetypeField => kind == SymbolKind::Field,
            EditKind::AddField => kind == SymbolKind::Struct,
            EditKind::AddVariant => kind == SymbolKind::Enum,
            EditKind::RemoveVariant => kind == SymbolKind::Variant,
            EditKind::AddRequiredTraitMethod => kind == SymbolKind::Trait,
            EditKind::NarrowVisibility => !matches!(
                kind,
                SymbolKind::Module | SymbolKind::Impl | SymbolKind::Macro
            ),
        }
    }
}

/// One replacement of a byte range of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// The file the range is in.
    pub file: RelPath,
    /// The byte range replaced, as offsets into the file text.
    pub range: Range<usize>,
    /// What the range becomes.
    pub replacement: String,
    /// The text of the whole item before the edit, clipped.
    pub before: String,
    /// The item's text after the edit, clipped.
    pub after: String,
}

impl Edit {
    /// The file text with the edit applied.
    pub fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + self.replacement.len());
        out.push_str(&text[..self.range.start]);
        out.push_str(&self.replacement);
        out.push_str(&text[self.range.end..]);
        out
    }
}

/// The longest snippet a case records, in characters.
const SNIPPET_LIMIT: usize = 240;

/// A parsed source file that edits are cut out of.
pub struct Source {
    /// The path the file has in the graph.
    file: RelPath,
    /// The file's text.
    text: String,
    /// The parsed syntax tree.
    root: SyntaxNode,
}

impl Source {
    /// Parses `text`. `None` when the file does not parse cleanly: an edit cut
    /// out of a broken tree does not prove anything.
    pub fn new(file: RelPath, text: String) -> Option<Source> {
        let parse = SourceFile::parse(&text, ra_ap_syntax::Edition::CURRENT);
        if !parse.errors().is_empty() {
            return None;
        }
        let root = parse.tree().syntax().clone();
        Some(Source { file, text, root })
    }

    /// The file's text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The syntax node of the item the graph gave the span of.
    ///
    /// The graph's span is the definition's whole syntax range, so the node is
    /// the one with exactly that range. Items that a macro produced have the
    /// span of the macro call instead, and are therefore not found.
    fn node_at(&self, span: &Span) -> Option<SyntaxNode> {
        let wanted = TextRange::new(TextSize::from(span.start), TextSize::from(span.end));
        self.root.descendants().find(|node| {
            node.text_range() == wanted
                && matches!(
                    node.kind(),
                    SyntaxKind::FN
                        | SyntaxKind::STRUCT
                        | SyntaxKind::ENUM
                        | SyntaxKind::UNION
                        | SyntaxKind::TRAIT
                        | SyntaxKind::RECORD_FIELD
                        | SyntaxKind::VARIANT
                        | SyntaxKind::CONST
                        | SyntaxKind::STATIC
                        | SyntaxKind::TYPE_ALIAS
                )
        })
    }

    /// The edit for `kind` applied to `symbol`, if that edit makes sense here.
    pub fn edit(&self, kind: EditKind, symbol: &Symbol) -> Option<Edit> {
        if !kind.applies_to(symbol.path.kind) {
            return None;
        }
        let node = self.node_at(&symbol.location.span)?;
        let item = node.text_range();
        let (range, replacement) = match kind {
            EditKind::AddParameter => add_parameter(&node)?,
            EditKind::ChangeReturnType => change_return_type(&node)?,
            EditKind::RemoveField | EditKind::RemoveVariant | EditKind::RemoveFunction => {
                if kind == EditKind::RemoveFunction && !removable_function(symbol, &node) {
                    return None;
                }
                (removal_range(&self.text, item)?, String::new())
            }
            EditKind::RetypeField => retype_field(&node)?,
            EditKind::AddField => add_field(&node)?,
            EditKind::AddVariant => add_variant(&node)?,
            EditKind::AddRequiredTraitMethod => add_required_trait_method(&node)?,
            EditKind::NarrowVisibility => narrow_visibility(symbol, &node)?,
        };
        let item_range = to_usize(item)?;
        let edit = Edit {
            file: self.file.clone(),
            range,
            replacement,
            before: String::new(),
            after: String::new(),
        };
        let (before, after) = snippets(&self.text, &item_range, &edit);
        Some(Edit {
            before,
            after,
            ..edit
        })
    }
}

/// The byte offsets of a syntax range, as `usize`.
fn to_usize(range: TextRange) -> Option<Range<usize>> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    Some(start..end)
}

/// A helper for the generators: a syntax range and its replacement text.
fn placed(range: TextRange, text: String) -> Option<(Range<usize>, String)> {
    Some((to_usize(range)?, text))
}

/// The item's text before the edit, and the same text with the edit applied.
fn snippets(text: &str, item: &Range<usize>, edit: &Edit) -> (String, String) {
    let before = text.get(item.clone()).unwrap_or_default();
    let after = if edit.range.start >= item.start && edit.range.end <= item.end {
        let head = &text[item.start..edit.range.start];
        let tail = &text[edit.range.end..item.end];
        format!("{head}{}{tail}", edit.replacement)
    } else {
        edit.replacement.clone()
    };
    (clip(before), clip(&after))
}

/// Shortens a snippet to what a report can show.
fn clip(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= SNIPPET_LIMIT {
        return text.to_owned();
    }
    let head: String = text.chars().take(SNIPPET_LIMIT).collect();
    format!("{head}…")
}

/// `fn f(a: i64)` -> `fn f(a: i64, __blastr_extra: i64)`.
fn add_parameter(node: &SyntaxNode) -> Option<(Range<usize>, String)> {
    let function = ast::Fn::cast(node.clone())?;
    // A foreign function has no body, and its signature is not ours to change.
    function.body()?;
    let params = function.param_list()?;
    let close = params.r_paren_token()?;
    let has_params = params.self_param().is_some() || params.params().next().is_some();
    let text = if has_params {
        ", __blastr_extra: i64"
    } else {
        "__blastr_extra: i64"
    };
    placed(
        TextRange::empty(close.text_range().start()),
        text.to_owned(),
    )
}

/// `-> T` -> `-> (T,)`.
fn change_return_type(node: &SyntaxNode) -> Option<(Range<usize>, String)> {
    let function = ast::Fn::cast(node.clone())?;
    let ret = function.ret_type()?;
    let ty = ret.ty()?;
    let text = ty.syntax().text().to_string();
    // Unsized and opaque types cannot be moved into a tuple: `(impl Trait,)`
    // and `(dyn Trait,)` are not types one can write there.
    if ["impl ", "dyn ", "!"]
        .iter()
        .any(|word| text.contains(word))
    {
        return None;
    }
    placed(ty.syntax().text_range(), format!("({text},)"))
}

/// A field's type `T` -> `(T,)`.
fn retype_field(node: &SyntaxNode) -> Option<(Range<usize>, String)> {
    let field = ast::RecordField::cast(node.clone())?;
    let ty = field.ty()?;
    let text = ty.syntax().text().to_string();
    if ["impl ", "dyn ", "!"]
        .iter()
        .any(|word| text.contains(word))
    {
        return None;
    }
    placed(ty.syntax().text_range(), format!("({text},)"))
}

/// A struct gains one field at the end of its field list.
fn add_field(node: &SyntaxNode) -> Option<(Range<usize>, String)> {
    let item = ast::Struct::cast(node.clone())?;
    let ast::FieldList::RecordFieldList(fields) = item.field_list()? else {
        return None;
    };
    let close = fields.r_curly_token()?;
    placed(
        TextRange::empty(close.text_range().start()),
        "    __blastr_extra: u8,\n".to_owned(),
    )
}

/// An enum gains one variant at the end of its variant list.
fn add_variant(node: &SyntaxNode) -> Option<(Range<usize>, String)> {
    let item = ast::Enum::cast(node.clone())?;
    let variants = item.variant_list()?;
    // A variant with an explicit discriminant makes adding another one a
    // question of arithmetic rather than of shape, which is not what this case
    // is about.
    if variants.variants().any(|variant| {
        variant
            .syntax()
            .children_with_tokens()
            .any(|child| child.kind() == SyntaxKind::EQ)
    }) {
        return None;
    }
    let close = variants.r_curly_token()?;
    placed(
        TextRange::empty(close.text_range().start()),
        "    __BlastrExtra,\n".to_owned(),
    )
}

/// A trait gains one required method at the end of its item list.
fn add_required_trait_method(node: &SyntaxNode) -> Option<(Range<usize>, String)> {
    let item = ast::Trait::cast(node.clone())?;
    let list = item.assoc_item_list()?;
    let close = list.r_curly_token()?;
    placed(
        TextRange::empty(close.text_range().start()),
        "    fn __blastr_required(&self);\n".to_owned(),
    )
}

/// `pub` -> `pub(crate)`, and `pub(crate)` -> nothing.
fn narrow_visibility(symbol: &Symbol, node: &SyntaxNode) -> Option<(Range<usize>, String)> {
    let visibility = node.children().find_map(ast::Visibility::cast)?;
    let replacement = match symbol.visibility {
        Visibility::Public => "pub(crate)",
        Visibility::Crate | Visibility::Restricted => "",
        Visibility::Private => return None,
    };
    placed(visibility.syntax().text_range(), replacement.to_owned())
}

/// Only free functions that have a body and are nobody's `main` are removed:
/// a removed trait method or `main` fails for reasons that are about the
/// container rather than about who referred to the function.
fn removable_function(symbol: &Symbol, node: &SyntaxNode) -> bool {
    if symbol.flags.in_trait || symbol.flags.in_trait_impl || symbol.flags.is_main {
        return false;
    }
    ast::Fn::cast(node.clone()).is_some_and(|function| function.body().is_some())
}

/// The range that removes a whole item without leaving broken syntax behind:
/// the item, its trailing comma if it is a list member, and the rest of its
/// line.
fn removal_range(text: &str, item: TextRange) -> Option<Range<usize>> {
    line_range(text, &to_usize(item)?)
}

/// The range that removes something written on its own lines: the item, the
/// comma after it if it is a list member, the indentation in front of it and
/// the line breaks around it. Something that shares its line with other code is
/// removed on its own, because taking the rest of the line would take that code
/// with it.
pub(crate) fn line_range(text: &str, item: &Range<usize>) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    if item.end > bytes.len() || item.start > item.end {
        return None;
    }
    let mut begin = item.start;
    while begin > 0 && matches!(bytes[begin - 1], b' ' | b'\t' | b'\r') {
        begin -= 1;
    }
    let alone_on_its_line = begin == 0 || bytes[begin - 1] == b'\n';
    if !alone_on_its_line {
        begin = item.start;
    }
    let mut end = item.end;
    if bytes.get(end) == Some(&b',') {
        end += 1;
    }
    while matches!(bytes.get(end), Some(b' ') | Some(b'\t') | Some(b'\r')) {
        end += 1;
    }
    if bytes.get(end) == Some(&b'\n') {
        end += 1;
        // A blank line that separated the item from what follows goes too.
        if alone_on_its_line && bytes.get(end) == Some(&b'\n') {
            end += 1;
        }
    } else if alone_on_its_line && begin > 0 && bytes[begin - 1] == b'\n' {
        // The last item of its block: take the line break in front of it.
        begin -= 1;
    }
    Some(begin..end)
}

/// Applies `edit` to `text` and checks that the result is still Rust.
pub fn apply_checked(edit: &Edit, text: &str) -> Result<String> {
    let edited = edit.apply(text);
    if edited == text {
        return Err(Error::Harness(format!(
            "the edit of `{}` changed nothing",
            edit.file.as_str()
        )));
    }
    require_parses(&edited, edit.file.as_str())?;
    Ok(edited)
}

/// Replaces `range` of `text` with `replacement` and checks that the result is
/// still Rust.
pub fn apply_text(text: &str, range: &Range<usize>, replacement: &str) -> Result<String> {
    if range.end > text.len() || range.start > range.end {
        return Err(Error::Harness(format!(
            "the range {range:?} is outside a text of {} bytes",
            text.len()
        )));
    }
    let edited = format!(
        "{}{}{}",
        &text[..range.start],
        replacement,
        &text[range.end..]
    );
    require_parses(&edited, "the mutation")?;
    Ok(edited)
}

/// Whether `text` parses as a Rust source file without syntax errors.
pub fn parses(text: &str) -> bool {
    SourceFile::parse(text, ra_ap_syntax::Edition::CURRENT)
        .errors()
        .is_empty()
}

/// Fails when `text` does not parse.
fn require_parses(text: &str, what: &str) -> Result<()> {
    let parse = SourceFile::parse(text, ra_ap_syntax::Edition::CURRENT);
    if parse.errors().is_empty() {
        return Ok(());
    }
    let first = parse
        .errors()
        .first()
        .map(|error| format!("{error:?}"))
        .unwrap_or_default();
    Err(Error::Harness(format!(
        "{what} does not parse after the change: {first}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{CrateId, Location, SymbolFlags, SymbolPath};

    /// A source file, panicking when it does not parse.
    fn source(text: &str) -> Source {
        Source::new(RelPath::new("src/lib.rs"), text.to_owned()).expect("the text parses")
    }

    /// A symbol for the item `name` at the nth occurrence of `needle` in the
    /// text, which is how the graph would describe it.
    fn symbol_at(source: &Source, needle: &str, kind: SymbolKind) -> Symbol {
        let start = source.text().find(needle).expect("the item is in the text");
        let node = source
            .root
            .descendants()
            .find(|node| {
                usize::from(node.text_range().start()) == start
                    && matches!(
                        node.kind(),
                        SyntaxKind::FN
                            | SyntaxKind::STRUCT
                            | SyntaxKind::ENUM
                            | SyntaxKind::TRAIT
                            | SyntaxKind::RECORD_FIELD
                            | SyntaxKind::VARIANT
                    )
            })
            .expect("the item has a node");
        let span = Span {
            start: u32::from(node.text_range().start()),
            end: u32::from(node.text_range().end()),
            start_line: 1,
            end_line: 1,
        };
        let visibility = node
            .children()
            .find_map(ast::Visibility::cast)
            .map(
                |visibility| match visibility.syntax().text().to_string().trim() {
                    "pub" => Visibility::Public,
                    "pub(crate)" => Visibility::Crate,
                    _ => Visibility::Restricted,
                },
            )
            .unwrap_or(Visibility::Private);
        Symbol {
            path: SymbolPath::new(format!("test::{needle}"), kind),
            krate: CrateId(0),
            location: Location {
                file: RelPath::new("src/lib.rs"),
                span,
            },
            visibility,
            signature_hash: 0,
            body_hash: 0,
            attr_hash: 0,
            body_sketch: Vec::new(),
            parent: None,
            flags: SymbolFlags::default(),
            impl_info: None,
        }
    }

    /// The edited text of the only case `kind` produces for `needle`.
    fn edited(text: &str, needle: &str, kind: SymbolKind, edit_kind: EditKind) -> String {
        let source = source(text);
        let symbol = symbol_at(&source, needle, kind);
        let edit = source
            .edit(edit_kind, &symbol)
            .unwrap_or_else(|| panic!("`{edit_kind:?}` applies to `{needle}`"));
        apply_checked(&edit, text).expect("the edit keeps the file parseable")
    }

    #[test]
    fn a_parameter_is_added_before_the_closing_parenthesis() {
        let text = "pub fn total(value: i64) -> i64 {\n    value + 1\n}\n";
        assert_eq!(
            edited(
                text,
                "pub fn total",
                SymbolKind::Function,
                EditKind::AddParameter
            ),
            "pub fn total(value: i64, __blastr_extra: i64) -> i64 {\n    value + 1\n}\n"
        );
    }

    #[test]
    fn a_parameter_is_added_to_a_method_with_a_receiver() {
        let text = "impl Point {\n    fn len(&self) -> i64 {\n        1\n    }\n}\n";
        assert_eq!(
            edited(text, "fn len", SymbolKind::Method, EditKind::AddParameter),
            "impl Point {\n    fn len(&self, __blastr_extra: i64) -> i64 {\n        1\n    }\n}\n"
        );
    }

    #[test]
    fn a_parameter_is_added_to_a_function_without_one() {
        let text = "fn nothing() {}\n";
        assert_eq!(
            edited(
                text,
                "fn nothing",
                SymbolKind::Function,
                EditKind::AddParameter
            ),
            "fn nothing(__blastr_extra: i64) {}\n"
        );
    }

    #[test]
    fn the_return_type_is_wrapped_in_a_tuple() {
        let text = "fn count() -> i64 {\n    1\n}\n";
        assert_eq!(
            edited(
                text,
                "fn count",
                SymbolKind::Function,
                EditKind::ChangeReturnType
            ),
            "fn count() -> (i64,) {\n    1\n}\n"
        );
    }

    #[test]
    fn a_return_type_of_an_opaque_type_is_left_alone() {
        let source =
            source("fn items() -> impl Iterator<Item = i64> {\n    Vec::new().into_iter()\n}\n");
        let symbol = symbol_at(&source, "fn items", SymbolKind::Function);
        assert!(source.edit(EditKind::ChangeReturnType, &symbol).is_none());
    }

    #[test]
    fn a_unit_function_has_no_return_type_to_change() {
        let source = source("fn run() {\n}\n");
        let symbol = symbol_at(&source, "fn run", SymbolKind::Function);
        assert!(source.edit(EditKind::ChangeReturnType, &symbol).is_none());
    }

    #[test]
    fn a_field_is_removed_with_its_comma_and_line() {
        let text = "pub struct Point {\n    pub x: i64,\n    pub y: i64,\n}\n";
        assert_eq!(
            edited(text, "pub x: i64", SymbolKind::Field, EditKind::RemoveField),
            "pub struct Point {\n    pub y: i64,\n}\n"
        );
    }

    #[test]
    fn the_last_field_is_removed_without_its_comma() {
        let text = "pub struct Point {\n    pub x: i64,\n    pub y: i64\n}\n";
        assert_eq!(
            edited(text, "pub y: i64", SymbolKind::Field, EditKind::RemoveField),
            "pub struct Point {\n    pub x: i64,\n}\n"
        );
    }

    #[test]
    fn a_field_type_is_wrapped_in_a_tuple() {
        let text = "pub struct Point {\n    pub x: i64,\n}\n";
        assert_eq!(
            edited(text, "pub x: i64", SymbolKind::Field, EditKind::RetypeField),
            "pub struct Point {\n    pub x: (i64,),\n}\n"
        );
    }

    #[test]
    fn a_struct_gains_a_field() {
        let text = "pub struct Point {\n    pub x: i64,\n}\n";
        assert_eq!(
            edited(
                text,
                "pub struct Point",
                SymbolKind::Struct,
                EditKind::AddField
            ),
            "pub struct Point {\n    pub x: i64,\n    __blastr_extra: u8,\n}\n"
        );
    }

    #[test]
    fn a_tuple_struct_takes_no_field() {
        let source = source("pub struct Pair(pub i64, pub i64);\n");
        let symbol = symbol_at(&source, "pub struct Pair", SymbolKind::Struct);
        assert!(source.edit(EditKind::AddField, &symbol).is_none());
    }

    #[test]
    fn an_enum_gains_a_variant() {
        let text = "pub enum Shape {\n    Dot,\n}\n";
        assert_eq!(
            edited(
                text,
                "pub enum Shape",
                SymbolKind::Enum,
                EditKind::AddVariant
            ),
            "pub enum Shape {\n    Dot,\n    __BlastrExtra,\n}\n"
        );
    }

    #[test]
    fn an_enum_with_discriminants_is_left_alone() {
        let source = source("pub enum Code {\n    One = 1,\n    Two = 2,\n}\n");
        let symbol = symbol_at(&source, "pub enum Code", SymbolKind::Enum);
        assert!(source.edit(EditKind::AddVariant, &symbol).is_none());
    }

    #[test]
    fn a_variant_is_removed() {
        let text = "pub enum Shape {\n    Dot,\n    Line(i64),\n}\n";
        assert_eq!(
            edited(
                text,
                "Line(i64)",
                SymbolKind::Variant,
                EditKind::RemoveVariant
            ),
            "pub enum Shape {\n    Dot,\n}\n"
        );
    }

    #[test]
    fn a_trait_gains_a_required_method() {
        let text = "pub trait Sizer {\n    fn size(&self) -> i64;\n}\n";
        assert_eq!(
            edited(
                text,
                "pub trait Sizer",
                SymbolKind::Trait,
                EditKind::AddRequiredTraitMethod
            ),
            "pub trait Sizer {\n    fn size(&self) -> i64;\n    fn __blastr_required(&self);\n}\n"
        );
    }

    #[test]
    fn a_function_is_removed_with_its_attributes() {
        let text = "pub fn first() -> i64 {\n    1\n}\n\npub fn second() -> i64 {\n    2\n}\n";
        assert_eq!(
            edited(
                text,
                "pub fn first",
                SymbolKind::Function,
                EditKind::RemoveFunction
            ),
            "pub fn second() -> i64 {\n    2\n}\n"
        );
    }

    #[test]
    fn a_method_of_a_trait_implementation_is_not_removed() {
        let source =
            source("impl Sizer for Point {\n    fn size(&self) -> i64 {\n        1\n    }\n}\n");
        let mut symbol = symbol_at(&source, "fn size", SymbolKind::Method);
        symbol.flags.in_trait_impl = true;
        assert!(source.edit(EditKind::RemoveFunction, &symbol).is_none());
    }

    #[test]
    fn visibility_is_narrowed_from_public_to_the_crate() {
        let text = "pub fn total() -> i64 {\n    1\n}\n";
        assert_eq!(
            edited(
                text,
                "pub fn total",
                SymbolKind::Function,
                EditKind::NarrowVisibility
            ),
            "pub(crate) fn total() -> i64 {\n    1\n}\n"
        );
    }

    #[test]
    fn visibility_is_narrowed_from_the_crate_to_private() {
        let text = "pub(crate) struct Point {\n    x: i64,\n}\n";
        assert_eq!(
            edited(
                text,
                "pub(crate) struct Point",
                SymbolKind::Struct,
                EditKind::NarrowVisibility
            ),
            " struct Point {\n    x: i64,\n}\n"
        );
    }

    #[test]
    fn a_private_item_has_no_visibility_to_narrow() {
        let source = source("fn hidden() {}\n");
        let symbol = symbol_at(&source, "fn hidden", SymbolKind::Function);
        assert!(source.edit(EditKind::NarrowVisibility, &symbol).is_none());
    }

    #[test]
    fn the_recorded_snippets_show_the_change() {
        let text = "pub fn total(value: i64) -> i64 {\n    value + 1\n}\n";
        let source = source(text);
        let symbol = symbol_at(&source, "pub fn total", SymbolKind::Function);
        let edit = source
            .edit(EditKind::AddParameter, &symbol)
            .expect("the edit applies");
        assert_eq!(
            edit.before,
            "pub fn total(value: i64) -> i64 {\n    value + 1\n}"
        );
        assert_eq!(
            edit.after,
            "pub fn total(value: i64, __blastr_extra: i64) -> i64 {\n    value + 1\n}"
        );
    }

    #[test]
    fn a_broken_file_is_not_used() {
        assert!(Source::new(RelPath::new("src/lib.rs"), "fn f( {".to_owned()).is_none());
    }

    #[test]
    fn kinds_round_trip_through_their_names() {
        for kind in EDIT_KINDS {
            assert_eq!(EditKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EditKind::parse("no-such-edit"), None);
    }
}
