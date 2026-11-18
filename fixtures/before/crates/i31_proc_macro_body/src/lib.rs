//! A procedural macro that works on tokens only, so it builds with nothing but
//! `proc_macro`.

use proc_macro::{TokenStream, TokenTree};

/// The name of the struct the derive is written on.
fn struct_name(input: TokenStream) -> String {
    let mut after_struct = false;
    for tree in input {
        let TokenTree::Ident(ident) = tree else {
            continue;
        };
        let text = ident.to_string();
        if after_struct {
            return text;
        }
        after_struct = text == "struct";
    }
    String::new()
}

/// The value the derived method returns.
fn greet_value() -> u8 {
    1
}

/// The implementation the derive writes.
fn describe_impl(name: &str) -> String {
    let value = greet_value();
    format!("impl Greet for {name} {{ fn greet(&self) -> u8 {{ {value} }} }}")
}

/// Derives `Greet` for a struct.
#[proc_macro_derive(Greet)]
pub fn derive_describe(input: TokenStream) -> TokenStream {
    let name = struct_name(input);
    describe_impl(&name)
        .parse()
        .expect("the generated impl parses")
}
