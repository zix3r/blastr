//! Syntax colours for the source editor.
//!
//! The component library's Rust grammar is a tree-sitter feature this build does
//! not enable, and the editor takes its colours from an [`InputHighlighter`]
//! either way. So this module answers with a scanner: comments, strings,
//! numbers, keywords and capitalised names. It is not a parser and does not aim
//! to be one — it is enough to make code read well without a grammar
//! dependency, and it runs over the whole text once per edit.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};
use gpui_kit::{Context, HighlightStyle, Hsla, SharedString, Window};

use crate::theme;

/// What a run of source text is, for colouring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syntax {
    /// Plain code: identifiers, punctuation, whitespace.
    Plain,
    /// A `//` comment, to the end of its line.
    Comment,
    /// A string or character literal.
    Text,
    /// A number literal.
    Number,
    /// A Rust keyword.
    Keyword,
    /// A name that starts in upper case: a type, a variant, a module.
    Type,
    /// A name that is called.
    Function,
}

impl Syntax {
    /// The colour this kind of text is drawn in.
    fn colour(self) -> Hsla {
        match self {
            Syntax::Plain => theme::FG_2.hsla(),
            Syntax::Comment => theme::SYNTAX_COMMENT.hsla(),
            Syntax::Text => theme::SYNTAX_STRING.hsla(),
            Syntax::Number => theme::SYNTAX_NUMBER.hsla(),
            Syntax::Keyword => theme::SYNTAX_KEYWORD.hsla(),
            Syntax::Type => theme::SYNTAX_TYPE.hsla(),
            Syntax::Function => theme::SYNTAX_FUNCTION.hsla(),
        }
    }

    /// The style the editor draws this kind of text with.
    fn style(self) -> HighlightStyle {
        HighlightStyle {
            color: Some(self.colour()),
            ..HighlightStyle::default()
        }
    }
}

/// One coloured run of the file, by byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    range: Range<usize>,
    syntax: Syntax,
}

/// The Rust scanner, as the source editor's highlighter.
#[derive(Debug, Default)]
pub(crate) struct Rust {
    /// The styled runs, in order and without a gap inside a line.
    spans: Vec<Span>,
}

impl InputHighlighter for Rust {
    fn language(&self) -> SharedString {
        "rust".into()
    }

    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.spans = scan(&text.to_string());
    }

    fn styles(
        &self,
        range: &Range<usize>,
        _resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut styles: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut at = range.start;
        for span in &self.spans {
            if span.range.end <= at {
                continue;
            }
            if span.range.start >= range.end {
                break;
            }
            let start = span.range.start.max(at);
            let end = span.range.end.min(range.end);
            if start > at {
                styles.push((at..start, Syntax::Plain.style()));
            }
            styles.push((start..end, span.syntax.style()));
            at = end;
        }
        if at < range.end {
            styles.push((at..range.end, Syntax::Plain.style()));
        }
        styles
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        // The source view never folds: a hidden line is a line the reader
        // cannot check. Returning nothing keeps fold icons out of the gutter.
        Vec::new()
    }
}

/// The factory the editor asks for a highlighter by language name.
pub(crate) fn factory() -> InputHighlighterFactory {
    Rc::new(|language| {
        language
            .eq_ignore_ascii_case("rust")
            .then(|| Box::new(Rust::default()) as Box<dyn InputHighlighter>)
    })
}

/// The coloured runs of a whole file.
fn scan(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        scan_line(line, offset, &mut spans);
        offset += line.len() + 1;
    }
    spans
}

/// Adds the coloured runs of one line, which starts at `offset` in the file.
fn scan_line(line: &str, offset: usize, spans: &mut Vec<Span>) {
    let mut index = 0;
    while index < line.len() {
        let start = index;
        let rest = &line[index..];
        let syntax;
        if rest.starts_with("//") {
            index = line.len();
            syntax = Syntax::Comment;
        } else {
            let character = rest.chars().next().unwrap_or_default();
            if character == '"' || character == '\'' {
                index = literal_end(line, index, character);
                syntax = Syntax::Text;
            } else if character.is_ascii_digit() {
                index += character.len_utf8();
                while let Some(next) = line[index..].chars().next() {
                    if next.is_ascii_alphanumeric() || next == '_' || next == '.' {
                        index += next.len_utf8();
                    } else {
                        break;
                    }
                }
                syntax = Syntax::Number;
            } else if character.is_alphabetic() || character == '_' {
                index += character.len_utf8();
                while let Some(next) = line[index..].chars().next() {
                    if next.is_alphanumeric() || next == '_' {
                        index += next.len_utf8();
                    } else {
                        break;
                    }
                }
                syntax = word_syntax(&line[start..index], &line[index..]);
            } else {
                index += character.len_utf8();
                syntax = Syntax::Plain;
            }
        }
        push(
            spans,
            Span {
                range: offset + start..offset + index,
                syntax,
            },
        );
    }
}

/// Adds `span`, joining it to the previous one when both say the same thing.
fn push(spans: &mut Vec<Span>, span: Span) {
    match spans.last_mut() {
        Some(previous)
            if previous.syntax == span.syntax && previous.range.end == span.range.start =>
        {
            previous.range.end = span.range.end;
        }
        _ => spans.push(span),
    }
}

/// Where the literal that starts at `index` ends, one past its last byte.
///
/// A literal that is not closed on its line ends the line, which keeps one
/// stray quote from colouring the rest of the file.
fn literal_end(line: &str, index: usize, quote: char) -> usize {
    let mut at = index + quote.len_utf8();
    let mut escaped = false;
    while let Some(character) = line[at..].chars().next() {
        at += character.len_utf8();
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            break;
        }
    }
    at
}

/// The syntax of `word`, given the text that follows it.
fn word_syntax(word: &str, rest: &str) -> Syntax {
    if is_keyword(word) {
        Syntax::Keyword
    } else if word.starts_with(|character: char| character.is_uppercase()) {
        Syntax::Type
    } else if rest.trim_start().starts_with('(') || rest.starts_with("::<") {
        Syntax::Function
    } else {
        Syntax::Plain
    }
}

/// Whether `word` is a Rust keyword worth colouring.
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The syntax the scan gives the run of text that holds `word`.
    fn syntax_of(text: &str, word: &str) -> Option<Syntax> {
        let at = text.find(word)?;
        scan(text)
            .into_iter()
            .find(|span| span.range.contains(&at))
            .map(|span| span.syntax)
    }

    struct NoStyles;

    impl HighlightStyleResolver for NoStyles {
        fn style(&self, _name: &str) -> Option<HighlightStyle> {
            None
        }
    }

    #[test]
    fn a_comment_runs_to_the_end_of_the_line() {
        let text = "let a = 1; // the comment\nlet b = 2;\n";
        assert_eq!(syntax_of(text, "comment"), Some(Syntax::Comment));
        assert_eq!(syntax_of(text, "b = 2"), Some(Syntax::Plain));
        assert_eq!(syntax_of(text, "a = 1"), Some(Syntax::Plain));
    }

    #[test]
    fn strings_and_numbers_keep_their_own_colours() {
        let text = "let name = \"blastr\";\nlet count = 42;\n";
        assert_eq!(syntax_of(text, "\"blastr\""), Some(Syntax::Text));
        assert_eq!(syntax_of(text, "42"), Some(Syntax::Number));
        assert_eq!(syntax_of(text, "name"), Some(Syntax::Plain));
    }

    #[test]
    fn keywords_types_and_calls_are_told_apart() {
        let text = "pub fn generate(order: Order) -> Invoice {\n";
        assert_eq!(syntax_of(text, "pub"), Some(Syntax::Keyword));
        assert_eq!(syntax_of(text, "Order"), Some(Syntax::Type));
        assert_eq!(syntax_of(text, "generate"), Some(Syntax::Function));
        assert_eq!(syntax_of(text, "Invoice"), Some(Syntax::Type));
    }

    #[test]
    fn an_unclosed_literal_stops_at_the_end_of_its_line() {
        let text = "let a = \"one\nlet b = 2;\n";
        assert_eq!(syntax_of(text, "one"), Some(Syntax::Text));
        assert_eq!(syntax_of(text, "b = 2"), Some(Syntax::Plain));
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_literal() {
        let text = "let a = \"one \\\" two\"; let b = 2;\n";
        assert_eq!(syntax_of(text, "two"), Some(Syntax::Text));
        assert_eq!(syntax_of(text, "b = 2"), Some(Syntax::Plain));
    }

    #[test]
    fn the_spans_are_in_order_and_hold_all_but_the_line_breaks() {
        for text in [
            "",
            "fn a() {}\n",
            "  //   \n",
            "let x = 'q';\nlet y = \"esc\\\"aped\";\n",
            "weird::<T>()\n",
            "let s = \"héllo\"; // ünïcode\n",
            "no trailing break",
        ] {
            let spans = scan(text);
            let covered: String = spans
                .iter()
                .filter_map(|span| text.get(span.range.clone()))
                .collect();
            assert_eq!(covered, text.replace('\n', ""), "{text:?}");
            let mut end = 0;
            for span in &spans {
                assert!(span.range.start >= end, "spans are in order: {span:?}");
                end = span.range.end;
            }
        }
    }

    #[test]
    fn the_styles_fill_the_range_they_are_asked_for() {
        let text = "fn a() {}\n";
        let highlighter = Rust { spans: scan(text) };

        for range in [0..text.len(), 3..7, 0..0, 9..9] {
            let styles = highlighter.styles(&range, &NoStyles);
            let mut end = range.start;
            for (style_range, _) in &styles {
                assert_eq!(style_range.start, end, "styles are contiguous");
                end = style_range.end;
            }
            assert_eq!(end, range.end, "{range:?} is covered to its end");
        }
    }

    #[test]
    fn only_rust_is_highlighted() {
        let factory = factory();
        assert!(factory("rust").is_some());
        assert!(factory("Rust").is_some());
        assert!(factory("markdown").is_none());
    }

    #[test]
    fn every_kind_of_text_has_an_opaque_colour() {
        for syntax in [
            Syntax::Plain,
            Syntax::Comment,
            Syntax::Text,
            Syntax::Number,
            Syntax::Keyword,
            Syntax::Type,
            Syntax::Function,
        ] {
            assert_eq!(syntax.colour().a, 1.0, "{syntax:?} is opaque");
        }
    }

    #[test]
    fn nothing_is_folded() {
        let text = "fn a() {\n    let b = 1;\n}\n";
        assert!(
            Rust { spans: scan(text) }
                .fold_ranges(&Rope::from(text))
                .is_empty()
        );
    }
}
