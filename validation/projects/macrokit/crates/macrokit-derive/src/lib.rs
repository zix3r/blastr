//! Hand-written procedural macros for the `macrokit` crates.
//!
//! Both macros work directly on [`proc_macro::TokenStream`]: no `syn`, no
//! `quote`, no helper crates. The parsers accept the subset of Rust syntax
//! macrokit actually needs (named, tuple and unit structs, generic parameters,
//! `where` clauses, field attributes) and report everything else as a
//! `compile_error!`.
//!
//! * `#[derive(Describe)]` implements `macrokit::describe::Describe`, which
//!   exposes the container name, the (included) field names and a
//!   `Debug`-rendered view of every field.
//! * `#[command(..)]` keeps the annotated function and additionally emits a
//!   `CommandSpec` constant describing it, so a table of commands can be built
//!   without a global registry.

use proc_macro::{Delimiter, Group, Literal, TokenStream, TokenTree};

/// Derives `macrokit::describe::Describe` for a struct.
///
/// Supported container options: `#[describe(name = "Display name")]`.
/// Supported field options: `#[describe(skip)]` and
/// `#[describe(rename = "other-name")]`.
#[proc_macro_derive(Describe, attributes(describe))]
pub fn derive_describe(input: TokenStream) -> TokenStream {
    describe_impl(input).unwrap_or_else(|message| compile_error(&message))
}

/// Registers the annotated function as a macrokit command.
///
/// `#[command(name = "set", group = "write", summary = "..", usage = "..",
/// aliases = ["s"])]` keeps the function untouched and adds
/// `pub const __command_spec_<fn>: macrokit::commands::CommandSpec` next to it.
#[proc_macro_attribute]
pub fn command(attr: TokenStream, item: TokenStream) -> TokenStream {
    command_impl(attr, item).unwrap_or_else(|message| compile_error(&message))
}

fn compile_error(message: &str) -> TokenStream {
    let literal = Literal::string(message);
    format!("::core::compile_error!({literal})")
        .parse()
        .expect("`compile_error!` is always a valid token stream")
}

fn emit(source: String) -> Result<TokenStream, String> {
    source
        .parse::<TokenStream>()
        .map_err(|error| format!("generated tokens do not parse: {error}"))
}

// --- token helpers ---------------------------------------------------------

fn punct(tree: &TokenTree, character: char) -> bool {
    matches!(tree, TokenTree::Punct(value) if value.as_char() == character)
}

fn ident_of(tree: &TokenTree) -> Option<String> {
    match tree {
        TokenTree::Ident(value) => Some(value.to_string()),
        _ => None,
    }
}

fn is_keyword(tree: &TokenTree, keyword: &str) -> bool {
    matches!(ident_of(tree), Some(text) if text == keyword)
}

fn text_of(tokens: &[TokenTree]) -> String {
    tokens.iter().cloned().collect::<TokenStream>().to_string()
}

fn split_commas(tokens: impl IntoIterator<Item = TokenTree>) -> Vec<Vec<TokenTree>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    for tree in tokens {
        if punct(&tree, ',') {
            chunks.push(std::mem::take(&mut current));
        } else {
            current.push(tree);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Skips `pub`, `pub(crate)`, `pub(super)` and friends.
fn skip_visibility(tokens: &[TokenTree], mut index: usize) -> usize {
    if tokens
        .get(index)
        .is_some_and(|tree| is_keyword(tree, "pub"))
    {
        index += 1;
        if let Some(TokenTree::Group(group)) = tokens.get(index)
            && group.delimiter() == Delimiter::Parenthesis
        {
            index += 1;
        }
    }
    index
}

fn unquote(literal: &str) -> Result<String, String> {
    let trimmed = literal.trim();
    let inner = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| format!("expected a string literal, found `{literal}`"))?;
    let mut value = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            value.push(character);
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| "string literal ends with a backslash".to_string())?;
        match escaped {
            '"' => value.push('"'),
            '\\' => value.push('\\'),
            'n' => value.push('\n'),
            't' => value.push('\t'),
            'r' => value.push('\r'),
            '\'' => value.push('\''),
            other => return Err(format!("unsupported escape `\\{other}`")),
        }
    }
    Ok(value)
}

// --- `describe` options ----------------------------------------------------

enum DescribeOption {
    Skip,
    Rename(String),
    Name(String),
}

fn parse_describe_attribute(group: &Group) -> Result<Vec<DescribeOption>, String> {
    if group.delimiter() != Delimiter::Bracket {
        return Ok(Vec::new());
    }
    let trees: Vec<TokenTree> = group.stream().into_iter().collect();
    if !trees
        .first()
        .is_some_and(|tree| is_keyword(tree, "describe"))
    {
        // `#[doc = ".."]`, `#[serde(..)]`, plain helper attributes: ignore.
        return Ok(Vec::new());
    }
    let inner = match trees.get(1) {
        Some(TokenTree::Group(inner)) if inner.delimiter() == Delimiter::Parenthesis => {
            inner.clone()
        }
        _ => return Err("expected `(..)` after `#[describe`".to_string()),
    };
    let mut options = Vec::new();
    for chunk in split_commas(inner.stream()) {
        if chunk.is_empty() {
            continue;
        }
        let keyword = chunk
            .first()
            .and_then(ident_of)
            .ok_or_else(|| "expected a `describe` option".to_string())?;
        match keyword.as_str() {
            "skip" => options.push(DescribeOption::Skip),
            "rename" | "name" => {
                if !chunk.get(1).is_some_and(|tree| punct(tree, '=')) {
                    return Err(format!("`{keyword}` needs a `= \"value\"` argument"));
                }
                let value = match chunk.get(2) {
                    Some(TokenTree::Literal(literal)) => unquote(&literal.to_string())?,
                    _ => return Err(format!("`{keyword}` needs a string literal")),
                };
                if keyword == "rename" {
                    options.push(DescribeOption::Rename(value));
                } else {
                    options.push(DescribeOption::Name(value));
                }
            }
            other => return Err(format!("unsupported `describe` option `{other}`")),
        }
    }
    Ok(options)
}

// --- container model -------------------------------------------------------

#[derive(Default)]
struct Generics {
    impl_params: String,
    type_args: String,
    where_clause: String,
    debug_bounds: Vec<String>,
}

impl Generics {
    fn where_text(&self) -> String {
        let existing = self.where_clause.trim();
        let bounds = self.debug_bounds.join(", ");
        match (existing.is_empty(), bounds.is_empty()) {
            (true, true) => String::new(),
            (true, false) => format!(" where {bounds}"),
            (false, true) => format!(" {existing}"),
            (false, false) => format!(" {existing}, {bounds}"),
        }
    }
}

struct FieldModel {
    name: String,
    accessor: String,
    skipped: bool,
}

struct ContainerModel {
    name: String,
    display_name: String,
    generics: Generics,
    fields: Vec<FieldModel>,
}

fn parse_container(input: TokenStream) -> Result<ContainerModel, String> {
    let trees: Vec<TokenTree> = input.into_iter().collect();
    let mut index = 0;
    let mut display_name = None;
    while index < trees.len() {
        if punct(&trees[index], '#') {
            let group = match trees.get(index + 1) {
                Some(TokenTree::Group(group)) => group.clone(),
                _ => return Err("expected an attribute after `#`".to_string()),
            };
            for option in parse_describe_attribute(&group)? {
                if let DescribeOption::Name(name) = option {
                    display_name = Some(name);
                }
            }
            index += 2;
            continue;
        }
        if is_keyword(&trees[index], "pub") {
            index = skip_visibility(&trees, index);
            continue;
        }
        break;
    }
    if !trees
        .get(index)
        .is_some_and(|tree| is_keyword(tree, "struct"))
    {
        return Err("`Describe` can only be derived for structs".to_string());
    }
    index += 1;
    let name = trees
        .get(index)
        .and_then(ident_of)
        .ok_or_else(|| "expected the struct name".to_string())?;
    index += 1;

    let mut generics = Generics::default();
    if trees.get(index).is_some_and(|tree| punct(tree, '<')) {
        let (params, next) = take_angle(&trees, index)?;
        generics = parse_generics(&params)?;
        index = next;
    }
    if trees
        .get(index)
        .is_some_and(|tree| is_keyword(tree, "where"))
    {
        let (clause, next) = take_until_body(&trees, index)?;
        generics.where_clause = text_of(&clause);
        index = next;
    }
    let body = trees
        .get(index)
        .ok_or_else(|| format!("`{name}` has no body"))?;
    let fields = match body {
        TokenTree::Group(group) => match group.delimiter() {
            Delimiter::Brace => parse_named_fields(&group.stream())?,
            Delimiter::Parenthesis => parse_tuple_fields(&group.stream())?,
            _ => return Err(format!("unsupported body of `{name}`")),
        },
        TokenTree::Punct(value) if value.as_char() == ';' => Vec::new(),
        _ => return Err(format!("unsupported body of `{name}`")),
    };
    Ok(ContainerModel {
        display_name: display_name.unwrap_or_else(|| name.clone()),
        name,
        generics,
        fields,
    })
}

/// Collects the tokens between `<` and its matching `>`, honouring `->`.
fn take_angle(trees: &[TokenTree], start: usize) -> Result<(Vec<TokenTree>, usize), String> {
    let mut depth = 0i32;
    let mut collected = Vec::new();
    let mut index = start;
    while index < trees.len() {
        let tree = &trees[index];
        if punct(tree, '-') && trees.get(index + 1).is_some_and(|next| punct(next, '>')) {
            collected.push(tree.clone());
            collected.push(trees[index + 1].clone());
            index += 2;
            continue;
        }
        if punct(tree, '<') {
            depth += 1;
            if depth == 1 {
                index += 1;
                continue;
            }
        }
        if punct(tree, '>') {
            depth -= 1;
            if depth == 0 {
                return Ok((collected, index + 1));
            }
        }
        collected.push(tree.clone());
        index += 1;
    }
    Err("unclosed generic parameter list".to_string())
}

/// Collects a `where` clause, stopping at the struct body.
fn take_until_body(trees: &[TokenTree], start: usize) -> Result<(Vec<TokenTree>, usize), String> {
    let mut collected = Vec::new();
    let mut index = start;
    while index < trees.len() {
        let tree = &trees[index];
        let stops = match tree {
            TokenTree::Group(group) => {
                matches!(group.delimiter(), Delimiter::Brace | Delimiter::Parenthesis)
            }
            TokenTree::Punct(value) => value.as_char() == ';',
            TokenTree::Ident(_) | TokenTree::Literal(_) => false,
        };
        if stops {
            return Ok((collected, index));
        }
        collected.push(tree.clone());
        index += 1;
    }
    Err("`where` clause is not followed by a struct body".to_string())
}

/// Drops a top-level `= Default` from one generic parameter.
fn strip_default(tokens: &[TokenTree]) -> Vec<TokenTree> {
    let mut depth = 0i32;
    let mut stripped = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let tree = &tokens[index];
        if punct(tree, '-') && tokens.get(index + 1).is_some_and(|next| punct(next, '>')) {
            stripped.push(tree.clone());
            stripped.push(tokens[index + 1].clone());
            index += 2;
            continue;
        }
        if punct(tree, '<') {
            depth += 1;
        } else if punct(tree, '>') {
            depth -= 1;
        } else if depth == 0 && punct(tree, '=') {
            break;
        }
        stripped.push(tree.clone());
        index += 1;
    }
    stripped
}

fn parse_generics(params: &[TokenTree]) -> Result<Generics, String> {
    let mut impl_params = Vec::new();
    let mut type_args = Vec::new();
    let mut debug_bounds = Vec::new();
    for chunk in split_commas(params.iter().cloned()) {
        if chunk.is_empty() {
            continue;
        }
        let stripped = text_of(&strip_default(&chunk));
        match &chunk[0] {
            TokenTree::Punct(value) if value.as_char() == '\'' => {
                let name = match chunk.get(1) {
                    Some(TokenTree::Ident(identifier)) => format!("'{identifier}"),
                    _ => return Err("expected a lifetime name".to_string()),
                };
                impl_params.push(stripped);
                type_args.push(name);
            }
            TokenTree::Ident(identifier) if identifier.to_string() == "const" => {
                let name = chunk
                    .get(1)
                    .and_then(ident_of)
                    .ok_or_else(|| "expected a const generic name".to_string())?;
                impl_params.push(stripped);
                type_args.push(name);
            }
            _ => {
                let name = chunk
                    .first()
                    .and_then(ident_of)
                    .ok_or_else(|| "expected a generic type parameter".to_string())?;
                impl_params.push(stripped);
                type_args.push(name.clone());
                debug_bounds.push(format!("{name}: ::std::fmt::Debug"));
            }
        }
    }
    Ok(Generics {
        impl_params: wrap_angles(impl_params),
        type_args: wrap_angles(type_args),
        where_clause: String::new(),
        debug_bounds,
    })
}

fn wrap_angles(parts: Vec<String>) -> String {
    if parts.is_empty() {
        String::new()
    } else {
        format!("<{}>", parts.join(", "))
    }
}

fn parse_named_fields(stream: &TokenStream) -> Result<Vec<FieldModel>, String> {
    let mut fields = Vec::new();
    for chunk in split_commas(stream.clone()) {
        if chunk.is_empty() {
            continue;
        }
        let (index, skipped, renamed) = skip_field_attributes(&chunk)?;
        let name = chunk
            .get(index)
            .and_then(ident_of)
            .ok_or_else(|| "expected a field name".to_string())?;
        if !chunk.get(index + 1).is_some_and(|tree| punct(tree, ':')) {
            return Err(format!("expected `:` after field `{name}`"));
        }
        if chunk.len() <= index + 2 {
            return Err(format!("field `{name}` has no type"));
        }
        fields.push(FieldModel {
            name: renamed.unwrap_or_else(|| name.clone()),
            accessor: name,
            skipped,
        });
    }
    Ok(fields)
}

fn parse_tuple_fields(stream: &TokenStream) -> Result<Vec<FieldModel>, String> {
    let mut fields = Vec::new();
    for (position, chunk) in split_commas(stream.clone()).into_iter().enumerate() {
        if chunk.is_empty() {
            continue;
        }
        let (index, skipped, renamed) = skip_field_attributes(&chunk)?;
        if index >= chunk.len() {
            return Err("tuple field has no type".to_string());
        }
        let accessor = position.to_string();
        fields.push(FieldModel {
            name: renamed.unwrap_or_else(|| accessor.clone()),
            accessor,
            skipped,
        });
    }
    Ok(fields)
}

/// Returns the index of the first non-attribute, non-visibility token.
fn skip_field_attributes(chunk: &[TokenTree]) -> Result<(usize, bool, Option<String>), String> {
    let mut index = 0;
    let mut skipped = false;
    let mut renamed = None;
    while index < chunk.len() {
        if punct(&chunk[index], '#') {
            let group = match chunk.get(index + 1) {
                Some(TokenTree::Group(group)) => group.clone(),
                _ => return Err("expected an attribute after `#`".to_string()),
            };
            for option in parse_describe_attribute(&group)? {
                match option {
                    DescribeOption::Skip => skipped = true,
                    DescribeOption::Rename(name) => renamed = Some(name),
                    DescribeOption::Name(_) => {
                        return Err("`name` is only supported on the struct itself".to_string());
                    }
                }
            }
            index += 2;
            continue;
        }
        if is_keyword(&chunk[index], "pub") {
            index = skip_visibility(chunk, index);
            continue;
        }
        break;
    }
    Ok((index, skipped, renamed))
}

fn describe_impl(input: TokenStream) -> Result<TokenStream, String> {
    let container = parse_container(input)?;
    let included: Vec<&FieldModel> = container
        .fields
        .iter()
        .filter(|field| !field.skipped)
        .collect();
    let names: Vec<String> = included
        .iter()
        .map(|field| format!("{:?}", field.name))
        .collect();
    let generics = &container.generics;
    let name = &container.name;

    let mut source = String::new();
    source.push_str(&format!(
        "impl{} ::macrokit::describe::Describe for {name}{}{} {{\n",
        generics.impl_params,
        generics.type_args,
        generics.where_text()
    ));
    source.push_str(&format!(
        "    const NAME: &'static str = {:?};\n",
        container.display_name
    ));
    source.push_str(&format!(
        "    const FIELDS: ::macrokit::describe::Fields = &[{}];\n",
        names.join(", ")
    ));
    source.push_str(
        "    fn describe(&self) -> ::macrokit::describe::Description {\n        let mut description = ::macrokit::describe::Description::new(\n            <Self as ::macrokit::describe::Describe>::FIELDS,\n        );\n",
    );
    for (field, name) in included.iter().zip(names.iter()) {
        source.push_str(&format!(
            "        description.push({}, ::std::format!(\"{{:?}}\", &self.{}));\n",
            name, field.accessor
        ));
    }
    source.push_str("        description\n    }\n}\n");
    emit(source)
}

// --- `#[command]` ----------------------------------------------------------

#[derive(Default)]
struct CommandOptions {
    name: Option<String>,
    group: Option<String>,
    summary: Option<String>,
    usage: Option<String>,
    aliases: Vec<String>,
}

fn parse_command_options(attr: TokenStream) -> Result<CommandOptions, String> {
    let mut options = CommandOptions::default();
    let trees: Vec<TokenTree> = attr.into_iter().collect();
    for chunk in split_commas(trees) {
        if chunk.is_empty() {
            continue;
        }
        let key = chunk
            .first()
            .and_then(ident_of)
            .ok_or_else(|| "expected a `#[command]` option name".to_string())?;
        if !chunk.get(1).is_some_and(|tree| punct(tree, '=')) {
            return Err(format!("`{key}` needs a `= value` argument"));
        }
        match key.as_str() {
            "aliases" => {
                let group = match chunk.get(2) {
                    Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Bracket => {
                        group.clone()
                    }
                    _ => return Err("`aliases` needs a `[\"a\", \"b\"]` list".to_string()),
                };
                for entry in split_commas(group.stream()) {
                    match entry.first() {
                        Some(TokenTree::Literal(literal)) => {
                            options.aliases.push(unquote(&literal.to_string())?);
                        }
                        _ => return Err("`aliases` entries must be string literals".to_string()),
                    }
                }
            }
            "name" | "group" | "summary" | "usage" => {
                let value = match chunk.get(2) {
                    Some(TokenTree::Literal(literal)) => unquote(&literal.to_string())?,
                    _ => return Err(format!("`{key}` needs a string literal")),
                };
                match key.as_str() {
                    "name" => options.name = Some(value),
                    "group" => options.group = Some(value),
                    "summary" => options.summary = Some(value),
                    _ => options.usage = Some(value),
                }
            }
            other => return Err(format!("unsupported `#[command]` option `{other}`")),
        }
    }
    Ok(options)
}

fn find_function_name(trees: &[TokenTree]) -> Option<String> {
    let mut index = 0;
    while index < trees.len() {
        if is_keyword(&trees[index], "fn") {
            return trees.get(index + 1).and_then(ident_of);
        }
        index += 1;
    }
    None
}

fn command_impl(attr: TokenStream, item: TokenStream) -> Result<TokenStream, String> {
    let options = parse_command_options(attr)?;
    let trees: Vec<TokenTree> = item.clone().into_iter().collect();
    let function = find_function_name(&trees)
        .ok_or_else(|| "`#[command]` can only be applied to a function".to_string())?;
    let name = options.name.unwrap_or_else(|| function.clone());
    let group = options.group.unwrap_or_else(|| "general".to_string());
    let summary = options.summary.unwrap_or_default();
    let usage = options.usage.unwrap_or_default();
    let aliases = options
        .aliases
        .iter()
        .map(|alias| format!("{alias:?}"))
        .collect::<Vec<String>>()
        .join(", ");
    let constant = format!("__command_spec_{function}");
    let generated = format!(
        "\n#[allow(non_upper_case_globals)]\n\
         #[doc = \"Command specification generated by `#[command]`.\"]\n\
         pub const {constant}: ::macrokit::commands::CommandSpec = \
         ::macrokit::commands::CommandSpec::new(\
         {name:?}, {group:?}, {summary:?}, {usage:?}, &[{aliases}], {function});\n"
    );
    let mut output = item;
    output.extend(
        generated
            .parse::<TokenStream>()
            .map_err(|error| format!("generated command specification does not parse: {error}"))?,
    );
    Ok(output)
}
