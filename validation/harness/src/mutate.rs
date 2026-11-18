//! The mutations the test check seeds into a function body.
//!
//! One mutation per function, the first that applies in the order below, so
//! that the results of two runs are comparable. Every mutation is cut out of a
//! real syntax tree: the operators come from the expression that holds them and
//! the deleted statement is a whole call statement, so nothing is found by
//! searching text. A mutant that does not compile is discarded by the caller;
//! it says nothing about test selection.

use std::ops::Range;

use blastr_model::{Span, Symbol};
use ra_ap_syntax::{
    AstNode, SourceFile, SyntaxKind, SyntaxNode, TextRange,
    ast::{self, ArithOp, BinaryOp, CmpOp, LogicOp, Ordering},
};

use crate::edits::apply_text;

/// The kinds of mutation, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MutationKind {
    /// `a + b` becomes `a - b`, `a < b` becomes `a >= b`.
    SwapOperator,
    /// `if condition` becomes `if !(condition)`.
    NegateCondition,
    /// A returned value becomes `Default::default()`.
    DefaultValue,
    /// A call statement is deleted.
    DeleteCall,
}

impl MutationKind {
    /// The name used in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            MutationKind::SwapOperator => "swap-operator",
            MutationKind::NegateCondition => "negate-condition",
            MutationKind::DefaultValue => "default-value",
            MutationKind::DeleteCall => "delete-call",
        }
    }
}

/// One mutation: a replacement of a byte range inside a function body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    /// Which mutation this is.
    pub kind: MutationKind,
    /// The replaced range.
    pub range: Range<usize>,
    /// What the range becomes.
    pub replacement: String,
    /// The function body before the mutation, clipped.
    pub before: String,
    /// The body after the mutation, clipped.
    pub after: String,
}

impl Mutation {
    /// The file text with the mutation applied, after checking that it still
    /// parses.
    pub fn apply(&self, text: &str) -> crate::Result<String> {
        apply_text(text, &self.range, &self.replacement)
    }
}

/// The replacement of a binary operator, or `None` for one this harness leaves
/// alone (assignment and shifts, which change what a statement does rather than
/// what it computes, and bit operations on values whose meaning is unclear).
fn swapped(operator: &BinaryOp) -> Option<&'static str> {
    Some(match operator {
        BinaryOp::ArithOp(ArithOp::Add) => "-",
        BinaryOp::ArithOp(ArithOp::Sub) => "+",
        BinaryOp::ArithOp(ArithOp::Mul) => "/",
        BinaryOp::ArithOp(ArithOp::Div) => "*",
        BinaryOp::ArithOp(ArithOp::Rem) => "/",
        BinaryOp::LogicOp(LogicOp::And) => "||",
        BinaryOp::LogicOp(LogicOp::Or) => "&&",
        BinaryOp::CmpOp(CmpOp::Eq { negated }) => {
            if *negated {
                "=="
            } else {
                "!="
            }
        }
        BinaryOp::CmpOp(CmpOp::Ord { ordering, strict }) => match (ordering, strict) {
            (Ordering::Less, true) => ">=",
            (Ordering::Less, false) => ">",
            (Ordering::Greater, true) => "<=",
            (Ordering::Greater, false) => "<",
        },
        _ => return None,
    })
}

/// Plans the mutation of `symbol`'s body, if one applies.
pub fn mutate(text: &str, symbol: &Symbol) -> Option<Mutation> {
    let parse = SourceFile::parse(text, ra_ap_syntax::Edition::CURRENT);
    if !parse.errors().is_empty() {
        return None;
    }
    let root = parse.tree().syntax().clone();
    let node = node_at(&root, &symbol.location.span)?;
    let function = ast::Fn::cast(node)?;
    let body = function.body()?;
    let body_range = to_usize(body.syntax().text_range())?;

    let (kind, range, replacement) = swap_operator(&body)
        .or_else(|| negate_condition(&body))
        .or_else(|| default_value(&function, &body))
        .or_else(|| delete_call(text, &body))?;

    let mutation = Mutation {
        kind,
        range: range.clone(),
        replacement: replacement.clone(),
        before: String::new(),
        after: String::new(),
    };
    let before = clip_slice(text, &body_range);
    let after = clip_slice(
        &apply_text_unchecked(text, &range, &replacement),
        &body_range,
    );
    Some(Mutation {
        before,
        after,
        ..mutation
    })
}

/// `text` with one range replaced, without checking the result.
fn apply_text_unchecked(text: &str, range: &Range<usize>, replacement: &str) -> String {
    format!(
        "{}{}{}",
        &text[..range.start],
        replacement,
        &text[range.end..]
    )
}

/// The node of the item whose range is `span`.
fn node_at(root: &SyntaxNode, span: &Span) -> Option<SyntaxNode> {
    let wanted = TextRange::new(span.start.into(), span.end.into());
    root.descendants()
        .find(|node| node.kind() == SyntaxKind::FN && node.text_range() == wanted)
}

/// Turns a syntax range into byte offsets.
fn to_usize(range: TextRange) -> Option<Range<usize>> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    (start <= end).then_some(start..end)
}

/// The text of `range` in `text`, clipped.
fn clip_slice(text: &str, range: &Range<usize>) -> String {
    let slice = text.get(range.clone()).unwrap_or_default().trim();
    if slice.chars().count() <= 240 {
        return slice.to_owned();
    }
    format!("{}…", slice.chars().take(240).collect::<String>())
}

/// `a + b` -> `a - b`, on the first arithmetic or comparison operator in the
/// body, taking the first met in a pre-order walk so the choice is reproducible.
fn swap_operator(body: &ast::BlockExpr) -> Option<(MutationKind, Range<usize>, String)> {
    for node in body.syntax().descendants() {
        let Some(expression) = ast::BinExpr::cast(node) else {
            continue;
        };
        let Some((token, operator)) = expression.op_details() else {
            continue;
        };
        let Some(replacement) = swapped(&operator) else {
            continue;
        };
        return Some((
            MutationKind::SwapOperator,
            to_usize(token.text_range())?,
            replacement.to_owned(),
        ));
    }
    None
}

/// `if condition {` -> `if !(condition) {`.
fn negate_condition(body: &ast::BlockExpr) -> Option<(MutationKind, Range<usize>, String)> {
    let condition = body
        .syntax()
        .descendants()
        .find_map(|node| ast::IfExpr::cast(node)?.condition())?;
    // A condition that already is a negation would be negated back; keep it.
    if condition.syntax().first_token()?.kind() == SyntaxKind::BANG {
        return None;
    }
    let text = condition.syntax().text().to_string();
    let range = to_usize(condition.syntax().text_range())?;
    Some((MutationKind::NegateCondition, range, format!("!({text})")))
}

/// A returned value becomes `Default::default()`.
///
/// Only functions that declare a return type are mutated: without one the
/// replacement has nothing to infer its type from and would not compile.
fn default_value(
    function: &ast::Fn,
    body: &ast::BlockExpr,
) -> Option<(MutationKind, Range<usize>, String)> {
    function.ret_type()?;
    let expression = match body.tail_expr() {
        Some(expression) => expression,
        None => body.statements().find_map(|statement| match statement {
            ast::Stmt::ExprStmt(statement) => statement.expr(),
            _ => None,
        })?,
    };
    // `()` has no useful default, and a block or a diverging expression is not
    // a value the replacement would fit.
    let kind = expression.syntax().kind();
    if matches!(
        kind,
        SyntaxKind::BLOCK_EXPR | SyntaxKind::RETURN_EXPR | SyntaxKind::BREAK_EXPR
    ) {
        return None;
    }
    let range = to_usize(expression.syntax().text_range())?;
    Some((
        MutationKind::DefaultValue,
        range,
        "Default::default()".to_owned(),
    ))
}

/// A call statement in the body is deleted:
/// `total += step();` disappears entirely.
fn delete_call(text: &str, body: &ast::BlockExpr) -> Option<(MutationKind, Range<usize>, String)> {
    for statement in body.statements() {
        let ast::Stmt::ExprStmt(statement) = statement else {
            continue;
        };
        let Some(expression) = statement.expr() else {
            continue;
        };
        let is_call = matches!(
            expression,
            ast::Expr::CallExpr(_) | ast::Expr::MethodCallExpr(_)
        );
        if !is_call {
            continue;
        }
        let statement_range = to_usize(statement.syntax().text_range())?;
        let range = crate::edits::line_range(text, &statement_range)?;
        return Some((MutationKind::DeleteCall, range, String::new()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{
        CrateId, Location, RelPath, SymbolFlags, SymbolKind, SymbolPath, Visibility,
    };

    /// A symbol for the function that starts at `needle` in `text`.
    fn function_symbol(text: &str, needle: &str) -> Symbol {
        let start = text.find(needle).expect("the function is in the text");
        let parse = SourceFile::parse(text, ra_ap_syntax::Edition::CURRENT);
        let node = parse
            .tree()
            .syntax()
            .descendants()
            .find(|node| {
                node.kind() == SyntaxKind::FN && usize::from(node.text_range().start()) == start
            })
            .expect("the function has a node");
        Symbol {
            path: SymbolPath::new("test::f", SymbolKind::Function),
            krate: CrateId(0),
            location: Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: u32::from(node.text_range().start()),
                    end: u32::from(node.text_range().end()),
                    start_line: 1,
                    end_line: 1,
                },
            },
            visibility: Visibility::Private,
            signature_hash: 0,
            body_hash: 0,
            attr_hash: 0,
            body_sketch: Vec::new(),
            parent: None,
            flags: SymbolFlags::default(),
            impl_info: None,
        }
    }

    /// The mutated text of the only mutation of `text`.
    fn mutated(text: &str, needle: &str) -> String {
        let symbol = function_symbol(text, needle);
        let mutation = mutate(text, &symbol).expect("a mutation applies");
        mutation.apply(text).expect("the mutant parses")
    }

    #[test]
    fn an_addition_becomes_a_subtraction() {
        let text = "fn f(a: i64) -> i64 {\n    a + 1\n}\n";
        assert_eq!(
            mutated(text, "fn f"),
            "fn f(a: i64) -> i64 {\n    a - 1\n}\n"
        );
    }

    #[test]
    fn a_less_than_becomes_a_greater_or_equal() {
        let text = "fn f(a: i64) -> bool {\n    a < 3\n}\n";
        assert_eq!(
            mutated(text, "fn f"),
            "fn f(a: i64) -> bool {\n    a >= 3\n}\n"
        );
    }

    #[test]
    fn a_comparison_is_preferred_over_a_condition() {
        let text = "fn f(a: i64) -> i64 {\n    if a == 0 {\n        return 1;\n    }\n    2\n}\n";
        assert_eq!(
            mutated(text, "fn f"),
            "fn f(a: i64) -> i64 {\n    if a != 0 {\n        return 1;\n    }\n    2\n}\n"
        );
    }

    #[test]
    fn a_condition_is_negated_when_no_operator_can_be_swapped() {
        let text = "fn f(flag: bool) -> i64 {\n    if flag {\n        return 1;\n    }\n    2\n}\n";
        assert_eq!(
            mutated(text, "fn f"),
            "fn f(flag: bool) -> i64 {\n    if !(flag) {\n        return 1;\n    }\n    2\n}\n"
        );
    }

    #[test]
    fn an_already_negated_condition_is_left_alone() {
        let text =
            "fn f(flag: bool) -> i64 {\n    if !flag {\n        return 1;\n    }\n    2\n}\n";
        let symbol = function_symbol(text, "fn f");
        let mutation = mutate(text, &symbol).expect("a mutation applies");
        assert_ne!(mutation.kind, MutationKind::NegateCondition);
    }

    #[test]
    fn a_returned_value_becomes_a_default() {
        let text = "fn f() -> i64 {\n    41\n}\n";
        assert_eq!(
            mutated(text, "fn f"),
            "fn f() -> i64 {\n    Default::default()\n}\n"
        );
    }

    #[test]
    fn nothing_is_mutated_without_an_operator_a_type_or_a_call() {
        let text = "fn f() {\n    let x = 1;\n}\n";
        let symbol = function_symbol(text, "fn f");
        // No operator to swap, no condition to negate, no return type to
        // replace and no call statement to delete.
        assert!(mutate(text, &symbol).is_none());
    }

    #[test]
    fn the_return_value_is_mutated_before_a_call_is_deleted() {
        let text = "fn f() -> i64 {\n    let mut total = 0;\n    total += 1;\n    total\n}\n";
        assert_eq!(
            mutated(text, "fn f"),
            "fn f() -> i64 {\n    let mut total = 0;\n    total += 1;\n    Default::default()\n}\n"
        );
    }

    #[test]
    fn a_call_statement_is_deleted_when_nothing_else_applies() {
        let text = "fn f(log: &mut Vec<i64>) {\n    log.push(1);\n}\n";
        assert_eq!(mutated(text, "fn f"), "fn f(log: &mut Vec<i64>) {\n}\n");
    }

    #[test]
    fn nothing_is_mutated_in_an_empty_body() {
        let text = "fn f() {}\n";
        let symbol = function_symbol(text, "fn f");
        assert!(mutate(text, &symbol).is_none());
    }

    #[test]
    fn mutation_kinds_have_names() {
        assert_eq!(MutationKind::SwapOperator.as_str(), "swap-operator");
        assert_eq!(MutationKind::NegateCondition.as_str(), "negate-condition");
        assert_eq!(MutationKind::DefaultValue.as_str(), "default-value");
        assert_eq!(MutationKind::DeleteCall.as_str(), "delete-call");
    }

    #[test]
    fn the_mutation_records_the_body_it_changed() {
        let text = "fn f(a: i64) -> i64 {\n    a + 1\n}\n";
        let symbol = function_symbol(text, "fn f");
        let mutation = mutate(text, &symbol).expect("a mutation applies");
        assert_eq!(mutation.before, "{\n    a + 1\n}");
        assert_eq!(mutation.after, "{\n    a - 1\n}");
    }
}
