//! An attribute macro shaped like `#[tokio::main]`, working on tokens only so
//! that it builds with nothing but `proc_macro`.

use proc_macro::{Delimiter, Group, TokenStream, TokenTree};

/// Re-emits the function it is written on without `async`, its body wrapped in
/// a closure that the new body calls.
///
/// The signature and the statements keep their own tokens, so they map back to
/// the text written in the file; the new braces and the closure are the
/// macro's own.
#[proc_macro_attribute]
pub fn wrapped(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let trees: Vec<TokenTree> = item.into_iter().collect();
    let last = trees.len().saturating_sub(1);
    let mut output = TokenStream::new();
    for (index, tree) in trees.into_iter().enumerate() {
        if matches!(&tree, TokenTree::Ident(ident) if ident.to_string() == "async") {
            continue;
        }
        if index != last {
            output.extend([tree]);
            continue;
        }
        let mut body: TokenStream = "let body = ||".parse().expect("the closure parses");
        body.extend([tree]);
        body.extend("; body()".parse::<TokenStream>().expect("the call parses"));
        output.extend([TokenTree::Group(Group::new(Delimiter::Brace, body))]);
    }
    output
}
