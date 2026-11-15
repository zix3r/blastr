//! Token hashing over syntax nodes.
//!
//! All hashes of the extraction rules are taken over the item's tokens with
//! whitespace and comments removed, so reformatting never changes a hash. The
//! tokens are collected by walking the syntax tree and skipping the subtrees the
//! rule excludes (attributes, the visibility, the item's own name, the body, ...).
//! A signature never contains the name of the item it belongs to: the name is
//! part of the symbol's path, so a rename leaves the hashes alone.

use blastr_model::{hash_tokens, sketch_tokens};
use ra_ap_syntax::ast;
use ra_ap_syntax::{AstNode, NodeOrToken, SyntaxNode};

/// The non-trivia token texts of `node`, in source order, minus the subtrees in
/// `skip`.
pub(crate) fn tokens(node: &SyntaxNode, skip: &[SyntaxNode]) -> Vec<String> {
    let mut out = Vec::new();
    collect(node, skip, &mut out);
    out
}

fn collect(node: &SyntaxNode, skip: &[SyntaxNode], out: &mut Vec<String>) {
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(child) => {
                // A doc comment is a node of its own, not a comment token, and
                // is a comment as far as the hashes are concerned.
                if !ast::DocComment::can_cast(child.kind())
                    && !skip.iter().any(|skipped| skipped == &child)
                {
                    collect(&child, skip, out);
                }
            }
            NodeOrToken::Token(token) => {
                if !token.kind().is_trivia() {
                    out.push(token.text().to_owned());
                }
            }
        }
    }
}

/// The tokens of `node` joined without whitespace or comments, except for one
/// space where two words would otherwise run together (`&mut T`, `dyn Trait`).
///
/// Used for the `<impl Trait for Type>` path segments, which must not depend
/// on how the source is formatted but are read by people.
pub(crate) fn text_without_ws(node: &SyntaxNode) -> String {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut text = String::new();
    for token in tokens(node, &[]) {
        if text.ends_with(is_word) && token.starts_with(is_word) {
            text.push(' ');
        }
        text.push_str(&token);
    }
    text
}

/// [`hash_tokens`] over an already collected token sequence.
pub(crate) fn hash(tokens: &[String]) -> u64 {
    hash_tokens(tokens.iter().map(String::as_str))
}

/// [`sketch_tokens`] over an already collected token sequence.
pub(crate) fn sketch(tokens: &[String]) -> Vec<u32> {
    sketch_tokens(tokens.iter().map(String::as_str))
}

/// The attributes written directly on `node`.
pub(crate) fn attrs(node: &SyntaxNode) -> Vec<ast::Attr> {
    node.children().filter_map(ast::Attr::cast).collect()
}

/// The children of `node` that belong to the attributes or the visibility and
/// are therefore part of no signature.
pub(crate) fn skipped_children(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children()
        .filter(|child| {
            ast::Attr::can_cast(child.kind()) || ast::Visibility::can_cast(child.kind())
        })
        .collect()
}

/// The token that is the item's own name, which is part of no signature hash.
///
/// The name is already in the symbol's path, so a renamed item keeps its
/// signature hash. Only a direct child is considered: the names of parameters,
/// fields and variants are nested deeper and keep taking part in the hash.
pub(crate) fn name_child(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children()
        .find(|child| ast::Name::can_cast(child.kind()))
}

/// The hash of the attributes of `node` other than `derive`, `doc` and doc
/// comments. 0 when no attribute remains.
///
/// Doc comments are comment tokens, so they never reach the token sequence.
pub(crate) fn attr_hash(node: &SyntaxNode) -> u64 {
    let mut all = Vec::new();
    for attr in attrs(node) {
        if is_derive_or_doc(&attr) {
            continue;
        }
        all.extend(tokens(attr.syntax(), &[]));
    }
    hash(&all)
}

/// True when the item carries an attribute whose last path segment is `test`.
pub(crate) fn has_test_attr(node: &SyntaxNode) -> bool {
    attrs(node)
        .iter()
        .any(|attr| last_segment(attr).is_some_and(|name| name == "test"))
}

fn is_derive_or_doc(attr: &ast::Attr) -> bool {
    last_segment(attr).is_some_and(|name| name == "derive" || name == "doc")
}

/// The last path segment of an attribute: `test` for `#[tokio::test]`.
fn last_segment(attr: &ast::Attr) -> Option<String> {
    let segment = attr.path()?.segment()?;
    let name = segment.name_ref()?;
    Some(name.text().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ra_ap_syntax::{Edition, SourceFile};

    fn parse(text: &str) -> SourceFile {
        SourceFile::parse(text, Edition::CURRENT).tree()
    }

    fn item_of(text: &str) -> SyntaxNode {
        parse(text)
            .syntax()
            .children()
            .next()
            .expect("the snippet has one item")
    }

    #[test]
    fn tokens_drop_whitespace_and_comments() {
        let node = item_of("fn f(/* why */ x: u8) {}");
        assert_eq!(
            tokens(&node, &[]),
            ["fn", "f", "(", "x", ":", "u8", ")", "{", "}"]
        );
    }

    #[test]
    fn tokens_skip_doc_comments() {
        let node = item_of("/// Doc.\n/** More. */\nfn f() {}");
        assert_eq!(tokens(&node, &[]), ["fn", "f", "(", ")", "{", "}"]);
    }

    #[test]
    fn hash_ignores_whitespace_and_comments() {
        let first = tokens(&item_of("fn f(x: u8) { x }"), &[]);
        let second = tokens(&item_of("fn  f( // note\n x : u8 )\n{ x }"), &[]);
        assert_eq!(hash(&first), hash(&second));
    }

    #[test]
    fn hash_changes_with_the_body() {
        let first = tokens(&item_of("fn f(x: u8) { x }"), &[]);
        let second = tokens(&item_of("fn f(x: u8) { x + 1 }"), &[]);
        assert_ne!(hash(&first), hash(&second));
    }

    #[test]
    fn empty_token_sequence_hashes_to_zero() {
        assert_eq!(hash(&[]), 0);
    }

    #[test]
    fn text_without_ws_joins_tokens() {
        let node = item_of("struct S<T>(T);");
        assert!(text_without_ws(&node).starts_with("struct S<T>(T);"));
    }

    #[test]
    fn text_without_ws_keeps_words_apart() {
        let node = item_of("impl<'a>  Tr  for  &'a   mut   dyn  Other< u8 > {}");
        assert_eq!(
            text_without_ws(&node),
            "impl<'a>Tr for&'a mut dyn Other<u8>{}"
        );
    }

    #[test]
    fn skipped_children_cover_attributes_and_visibility() {
        let node = item_of("#[test]\npub fn f() {}");
        assert_eq!(skipped_children(&node).len(), 2);
    }

    #[test]
    fn name_child_is_the_item_name_only() {
        let node = item_of("fn f(g: u8) -> u8 { g }");
        let name = name_child(&node).expect("a function has a name");
        assert_eq!(name.text().to_string(), "f");
        let field = item_of("struct S { value: u8 }");
        assert_eq!(
            name_child(&field).map(|name| name.text().to_string()),
            Some("S".to_owned())
        );
        assert_eq!(name_child(&item_of("impl S {}")), None);
    }

    #[test]
    fn derive_and_doc_are_not_hashed() {
        assert_eq!(attr_hash(&item_of("#[derive(Debug)]\nstruct S;")), 0);
        assert_eq!(attr_hash(&item_of("#[doc = \"x\"]\nstruct S;")), 0);
        assert_eq!(attr_hash(&item_of("/// Doc.\nstruct S;")), 0);
        assert_eq!(attr_hash(&item_of("struct S;")), 0);
        assert_ne!(attr_hash(&item_of("#[inline]\nfn f() {}")), 0);
    }

    #[test]
    fn attr_hash_ignores_the_item_itself() {
        let first = attr_hash(&item_of("#[inline]\nfn f() {}"));
        let second = attr_hash(&item_of("#[inline]\nfn g(x: u8) -> u8 { x }"));
        assert_eq!(first, second);
    }

    #[test]
    fn test_attribute_is_recognised_by_its_last_segment() {
        assert!(has_test_attr(&item_of("#[test]\nfn f() {}")));
        assert!(has_test_attr(&item_of("#[tokio::test]\nfn f() {}")));
        assert!(!has_test_attr(&item_of("#[ignore]\nfn f() {}")));
        assert!(!has_test_attr(&item_of("fn f() {}")));
    }
}
