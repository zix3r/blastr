//! A tiny command registry on top of the [`Engine`].
//!
//! Handlers are plain functions with the `#[command]` attribute, which emits a
//! [`CommandSpec`] constant next to each of them. [`Registry`] collects those
//! constants, sorts them by name and dispatches text lines to the handlers.

use macrokit_derive::{Describe, command};

use crate::describe::Describe;
use crate::dimension::Dimension;
use crate::engine::{Engine, Source};
use crate::error::{ConfigError, UnitError};
use crate::render;
use crate::unit;
use crate::util;

/// Result alias used by command handlers.
pub type CmdResult = Result<CmdOutcome, CmdError>;

/// Signature every command handler implements.
pub type Handler = fn(&mut Engine, &[&str]) -> CmdResult;

/// The output of one command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdOutcome {
    lines: Vec<String>,
}

impl CmdOutcome {
    /// An outcome without lines.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one line.
    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    /// Appends every line of `lines`.
    pub fn extend<I: IntoIterator<Item = String>>(&mut self, lines: I) {
        self.lines.extend(lines);
    }

    /// The lines of the outcome.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Number of lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the outcome has no lines.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The lines joined with newlines.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl std::fmt::Display for CmdOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            formatter.write_str(line)?;
        }
        Ok(())
    }
}

impl FromIterator<String> for CmdOutcome {
    fn from_iter<I: IntoIterator<Item = String>>(lines: I) -> Self {
        let mut outcome = Self::new();
        outcome.lines.extend(lines);
        outcome
    }
}

/// Everything that can go wrong while dispatching a command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CmdError {
    /// The line contained no command name.
    EmptyCommandLine,
    /// No command (or alias) matches the name.
    UnknownCommand {
        /// The name that was typed.
        name: String,
        /// Closest registered command, if one is close enough.
        suggestion: Option<String>,
    },
    /// A required positional argument is missing.
    MissingArgument {
        /// The command that was called.
        command: &'static str,
        /// The argument that is missing.
        argument: &'static str,
    },
    /// More positional arguments were supplied than the command accepts.
    TooManyArguments {
        /// The command that was called.
        command: &'static str,
        /// The number of arguments the command accepts.
        expected: usize,
    },
    /// The arguments are present but unusable.
    BadUsage {
        /// The command that was called.
        command: &'static str,
        /// Why the call was rejected.
        hint: String,
    },
    /// The engine rejected the operation.
    Config(ConfigError),
    /// The unit layer rejected the operation.
    Unit(UnitError),
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCommandLine => formatter.write_str("empty command line"),
            Self::UnknownCommand { name, suggestion } => {
                write!(formatter, "unknown command `{name}`")?;
                match suggestion {
                    Some(candidate) => write!(formatter, ", did you mean `{candidate}`?"),
                    None => Ok(()),
                }
            }
            Self::MissingArgument { command, argument } => {
                write!(formatter, "`{command}` needs a <{argument}> argument")
            }
            Self::TooManyArguments { command, expected } => write!(
                formatter,
                "`{command}` accepts at most {expected} argument(s)"
            ),
            Self::BadUsage { command, hint } => write!(formatter, "`{command}`: {hint}"),
            Self::Config(error) => std::fmt::Display::fmt(error, formatter),
            Self::Unit(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for CmdError {}

impl From<ConfigError> for CmdError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<UnitError> for CmdError {
    fn from(error: UnitError) -> Self {
        Self::Unit(error)
    }
}

/// The description of one command.
#[derive(Debug, Clone, Copy, Describe)]
#[describe(name = "command")]
pub struct CommandSpec {
    name: &'static str,
    group: &'static str,
    summary: &'static str,
    usage: &'static str,
    aliases: &'static [&'static str],
    #[describe(skip)]
    handler: Handler,
}

// `handler` is a function pointer, so it is deliberately left out of the
// comparison: the metadata is what identifies a command.
impl PartialEq for CommandSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.group == other.group
            && self.summary == other.summary
            && self.usage == other.usage
            && self.aliases == other.aliases
    }
}

impl Eq for CommandSpec {}

impl CommandSpec {
    /// Builds a specification; used by the constants that `#[command]` emits.
    pub const fn new(
        name: &'static str,
        group: &'static str,
        summary: &'static str,
        usage: &'static str,
        aliases: &'static [&'static str],
        handler: Handler,
    ) -> Self {
        Self {
            name,
            group,
            summary,
            usage,
            aliases,
            handler,
        }
    }

    /// The command name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The group used by the help output.
    pub const fn group(&self) -> &'static str {
        self.group
    }

    /// One line description.
    pub const fn summary(&self) -> &'static str {
        self.summary
    }

    /// Argument pattern.
    pub const fn usage(&self) -> &'static str {
        self.usage
    }

    /// Alternative names.
    pub const fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    /// The handler function.
    pub const fn handler(&self) -> Handler {
        self.handler
    }

    /// Whether `name` is the command name or one of its aliases.
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

/// A set of commands, sorted by name.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    specs: Vec<CommandSpec>,
}

impl Registry {
    /// Builds a registry, rejecting duplicate names and aliases.
    ///
    /// # Errors
    ///
    /// Returns [`CmdError::BadUsage`] when a name or alias is registered twice.
    pub fn new(mut specs: Vec<CommandSpec>) -> Result<Self, CmdError> {
        specs.sort_by(|left, right| left.name().cmp(right.name()));
        let mut seen: Vec<&'static str> = Vec::new();
        for spec in &specs {
            for candidate in std::iter::once(spec.name()).chain(spec.aliases().iter().copied()) {
                if seen.contains(&candidate) {
                    return Err(CmdError::BadUsage {
                        command: "registry",
                        hint: format!("`{candidate}` is registered twice"),
                    });
                }
                seen.push(candidate);
            }
        }
        Ok(Self { specs })
    }

    /// The registry with every command `macrokit` ships.
    ///
    /// # Panics
    ///
    /// Panics only if the built-in command table is inconsistent, which the
    /// unit tests rule out.
    pub fn builtin() -> Self {
        Self::new(builtin_specs()).expect("the built-in command table is consistent")
    }

    /// Adds commands, re-sorting the registry.
    ///
    /// # Errors
    ///
    /// See [`Registry::new`].
    pub fn extend(&mut self, specs: Vec<CommandSpec>) -> Result<(), CmdError> {
        let mut combined = self.specs.clone();
        combined.extend(specs);
        *self = Self::new(combined)?;
        Ok(())
    }

    /// Finds a command by name or alias.
    pub fn find(&self, name: &str) -> Option<&CommandSpec> {
        let needle = name.trim().to_ascii_lowercase();
        self.specs.iter().find(|spec| spec.matches(&needle))
    }

    /// The closest registered name or alias, if one is close enough.
    pub fn suggest(&self, name: &str) -> Option<String> {
        let candidates = self
            .specs
            .iter()
            .flat_map(|spec| std::iter::once(spec.name()).chain(spec.aliases().iter().copied()));
        util::closest(name, candidates, 2).map(str::to_string)
    }

    /// Every command, in name order.
    pub fn iter(&self) -> impl Iterator<Item = &CommandSpec> {
        self.specs.iter()
    }

    /// Every command name, in order.
    pub fn names(&self) -> Vec<&'static str> {
        self.specs.iter().map(CommandSpec::name).collect()
    }

    /// Every group name, deduplicated and sorted.
    pub fn groups(&self) -> Vec<&'static str> {
        let mut groups: Vec<&'static str> = self.specs.iter().map(CommandSpec::group).collect();
        groups.sort_unstable();
        groups.dedup();
        groups
    }

    /// Number of commands.
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Dispatches a text line such as `set loop.gain 2.5`.
    ///
    /// # Errors
    ///
    /// Returns [`CmdError`] for unknown commands, bad arguments and engine
    /// errors.
    pub fn dispatch(&self, engine: &mut Engine, line: &str) -> CmdResult {
        let tokens = util::split_arguments(line);
        self.dispatch_tokens(engine, &tokens)
    }

    /// Dispatches already tokenised arguments.
    ///
    /// # Errors
    ///
    /// See [`Registry::dispatch`].
    pub fn dispatch_tokens(&self, engine: &mut Engine, tokens: &[String]) -> CmdResult {
        let name = tokens
            .first()
            .map(String::as_str)
            .ok_or(CmdError::EmptyCommandLine)?;
        let spec = self.find(name).ok_or_else(|| CmdError::UnknownCommand {
            name: name.to_string(),
            suggestion: self.suggest(name),
        })?;
        let arguments: Vec<&str> = tokens.iter().skip(1).map(String::as_str).collect();
        (spec.handler())(engine, &arguments)
    }

    /// A help table with one row per command.
    pub fn help(&self) -> String {
        let rows: Vec<Vec<String>> = self
            .specs
            .iter()
            .map(|spec| {
                vec![
                    spec.name().to_string(),
                    spec.group().to_string(),
                    spec.usage().to_string(),
                    spec.summary().to_string(),
                ]
            })
            .collect();
        render::table(&["command", "group", "usage", "summary"], &rows)
    }
}

fn required<'a>(
    args: &[&'a str],
    index: usize,
    command: &'static str,
    argument: &'static str,
) -> Result<&'a str, CmdError> {
    args.get(index)
        .copied()
        .ok_or(CmdError::MissingArgument { command, argument })
}

fn expect_at_most(args: &[&str], maximum: usize, command: &'static str) -> Result<(), CmdError> {
    if args.len() > maximum {
        return Err(CmdError::TooManyArguments {
            command,
            expected: maximum,
        });
    }
    Ok(())
}

fn expect_exactly(args: &[&str], count: usize, command: &'static str) -> Result<(), CmdError> {
    if args.len() < count {
        return Err(CmdError::TooManyArguments {
            command,
            expected: count,
        });
    }
    expect_at_most(args, count, command)
}

/// Prints the effective value and the source of one setting.
#[command(
    name = "get",
    group = "read",
    summary = "Print the effective value of a setting",
    usage = "get <key>"
)]
pub fn get(engine: &mut Engine, args: &[&str]) -> CmdResult {
    let key = required(args, 0, "get", "key")?;
    expect_at_most(args, 1, "get")?;
    let spec = engine.schema().require(key)?.clone();
    let value = engine.get(spec.key())?.render();
    let source = engine
        .source_of(spec.key())
        .map_or("default", |source| source.as_str());
    let mut outcome = CmdOutcome::new();
    outcome.push(format!("{} = {value}", spec.key()));
    outcome.push(format!("source: {source}"));
    Ok(outcome)
}

/// Parses a value for one setting and stores it.
#[command(
    name = "set",
    group = "write",
    summary = "Set a setting from text",
    usage = "set <key> <value>",
    aliases = ["s"]
)]
pub fn set(engine: &mut Engine, args: &[&str]) -> CmdResult {
    let key = required(args, 0, "set", "key")?;
    ensure!(
        args.len() >= 2,
        CmdError::MissingArgument {
            command: "set",
            argument: "value"
        }
    );
    let raw = args[1..].join(" ");
    engine.set(key, &raw)?;
    let spec = engine.schema().require(key)?.clone();
    let mut outcome = CmdOutcome::new();
    outcome.push(format!(
        "{} = {}",
        spec.key(),
        engine.get(spec.key())?.render()
    ));
    Ok(outcome)
}

/// Removes the explicit value of a setting.
#[command(
    name = "unset",
    group = "write",
    summary = "Remove an explicit value and fall back to the default",
    usage = "unset <key>",
    aliases = ["clear"]
)]
pub fn unset(engine: &mut Engine, args: &[&str]) -> CmdResult {
    let key = required(args, 0, "unset", "key")?;
    expect_at_most(args, 1, "unset")?;
    let removed = engine.unset(key, Source::Override)?;
    let spec = engine.schema().require(key)?.clone();
    let mut outcome = CmdOutcome::new();
    if removed {
        outcome.push(format!("unset {}", spec.key()));
    } else {
        outcome.push(format!("{} had no explicit value", spec.key()));
    }
    if let Ok(value) = engine.get(spec.key()) {
        outcome.push(format!("effective: {}", value.render()));
    }
    Ok(outcome)
}

/// Restores the declared default of one setting, or of every setting.
#[command(
    name = "reset",
    group = "write",
    summary = "Restore declared defaults",
    usage = "reset [key]"
)]
pub fn reset(engine: &mut Engine, args: &[&str]) -> CmdResult {
    expect_at_most(args, 1, "reset")?;
    let mut outcome = CmdOutcome::new();
    match args.first() {
        Some(key) => {
            let spec = engine.schema().require(key)?.clone();
            engine.reset(spec.key())?;
            outcome.push(format!("reset {}", spec.key()));
        }
        None => {
            engine.reset_all();
            outcome.push("reset every setting to its default".to_string());
        }
    }
    Ok(outcome)
}

/// Lists effective settings, optionally filtered by key prefix.
#[command(
    name = "list",
    group = "read",
    summary = "List effective settings, optionally filtered by key prefix",
    usage = "list [prefix]"
)]
pub fn list(engine: &mut Engine, args: &[&str]) -> CmdResult {
    expect_at_most(args, 1, "list")?;
    let prefix = args.first().copied().unwrap_or("");
    let mut outcome = CmdOutcome::new();
    for spec in engine.schema().keys_matching(prefix) {
        let value = engine
            .get(spec.key())
            .map_or_else(|_| "<unset>".to_string(), |value| value.render());
        let source = engine
            .source_of(spec.key())
            .map_or("default", |source| source.as_str());
        outcome.push(format!("{} = {value} ({source})", spec.key()));
    }
    if outcome.is_empty() {
        outcome.push(format!("no settings match `{prefix}`"));
    }
    Ok(outcome)
}

/// Shows the schema entry of one setting.
#[command(
    name = "describe",
    group = "read",
    summary = "Show the schema entry of a setting",
    usage = "describe <key>"
)]
pub fn describe(engine: &mut Engine, args: &[&str]) -> CmdResult {
    let key = required(args, 0, "describe", "key")?;
    expect_at_most(args, 1, "describe")?;
    let spec = engine.schema().require(key)?.clone();
    let description = spec.describe();
    let mut outcome = CmdOutcome::new();
    outcome.push(format!("{} ({})", spec.key(), Describe::name(&spec)));
    for (field, rendered) in description.entries() {
        if field == "key" || field == "id" {
            continue;
        }
        let text = if field == "default" && spec.default().is_none() {
            "none".to_string()
        } else if field == "doc" {
            rendered.trim_matches('"').to_string()
        } else {
            rendered.clone()
        };
        outcome.push(format!("{field}: {text}"));
    }
    outcome.push(format!("accepts: {}", spec.kind().describe_kind()));
    if let Ok(value) = engine.get(spec.key()) {
        outcome.push(format!("current: {}", value.render()));
    }
    if let Some(hint) = spec.deprecation() {
        outcome.push(format!("deprecated: {hint}"));
    }
    Ok(outcome)
}

/// Converts a value between two units.
#[command(
    name = "convert",
    group = "units",
    summary = "Convert a value between units",
    usage = "convert <value> <from> <to>",
    aliases = ["conv"]
)]
pub fn convert(_engine: &mut Engine, args: &[&str]) -> CmdResult {
    let value = required(args, 0, "convert", "value")?;
    let from = required(args, 1, "convert", "from")?;
    let to = required(args, 2, "convert", "to")?;
    expect_exactly(args, 3, "convert")?;
    let number: f64 = value.parse().map_err(|_| CmdError::BadUsage {
        command: "convert",
        hint: format!("`{value}` is not a number"),
    })?;
    let source = unit::resolve(from)?;
    let target = unit::resolve(to)?;
    let converted = source.convert(number, &target)?;
    let mut outcome = CmdOutcome::new();
    outcome.push(format!(
        "{} {} = {} {}",
        util::format_float(number),
        source.symbol(),
        util::format_float(converted),
        target.symbol()
    ));
    Ok(outcome)
}

/// Lists the registered units of a dimension, or the dimensions in use.
#[command(
    name = "units",
    group = "units",
    summary = "List the units of a dimension",
    usage = "units [dimension]"
)]
pub fn units(engine: &mut Engine, args: &[&str]) -> CmdResult {
    expect_at_most(args, 1, "units")?;
    let mut outcome = CmdOutcome::new();
    match args.first() {
        Some(text) => {
            let dimension = Dimension::parse(text)?;
            let registered = unit::units_with_dimension(dimension);
            if registered.is_empty() {
                outcome.push(format!(
                    "no units registered for `{}`",
                    dimension.describe()
                ));
            }
            for unit in registered {
                outcome.push(format!("{} ({})", unit.symbol(), unit.formula()));
            }
        }
        None => {
            for (dimension, keys) in engine.schema().by_dimension() {
                outcome.push(format!("{}: {}", dimension.describe(), keys.join(", ")));
            }
        }
    }
    Ok(outcome)
}

/// Applies `key=value` assignments in one go.
#[command(
    name = "apply",
    group = "write",
    summary = "Apply key=value assignments",
    usage = "apply <key>=<value> ..."
)]
pub fn apply(engine: &mut Engine, args: &[&str]) -> CmdResult {
    ensure!(
        !args.is_empty(),
        CmdError::MissingArgument {
            command: "apply",
            argument: "key=value"
        }
    );
    let tokens: Vec<String> = args
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();
    let applied = engine.apply_tokens(&tokens)?;
    let mut outcome = CmdOutcome::new();
    outcome.push(format!(
        "applied {}",
        util::plural(applied, "assignment", "assignments")
    ));
    Ok(outcome)
}

/// Runs the cross-field validation rules.
#[command(
    name = "validate",
    group = "read",
    summary = "Run cross-field validation and report issues",
    usage = "validate"
)]
pub fn validate(engine: &mut Engine, args: &[&str]) -> CmdResult {
    expect_exactly(args, 0, "validate")?;
    let report = engine.validate();
    let mut outcome = CmdOutcome::new();
    outcome.extend(report.render());
    Ok(outcome)
}

/// Every command `macrokit` ships, as `CommandSpec` values.
pub fn builtin_specs() -> Vec<CommandSpec> {
    vec![
        __command_spec_apply,
        __command_spec_convert,
        __command_spec_describe,
        __command_spec_get,
        __command_spec_list,
        __command_spec_reset,
        __command_spec_set,
        __command_spec_unset,
        __command_spec_units,
        __command_spec_validate,
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        __command_spec_get, __command_spec_set, CmdError, CmdOutcome, CommandSpec, Registry,
        builtin_specs,
    };
    use crate::engine::{Engine, Source};
    use crate::error::ConfigError;

    fn registry() -> Registry {
        Registry::builtin()
    }

    #[test]
    fn the_builtin_registry_is_sorted_and_complete() {
        let registry = registry();
        assert_eq!(registry.len(), 10);
        assert!(!registry.is_empty());
        assert_eq!(
            registry.names(),
            vec![
                "apply", "convert", "describe", "get", "list", "reset", "set", "units", "unset",
                "validate"
            ]
        );
        let mut sorted = registry.names();
        sorted.sort_unstable();
        assert_eq!(registry.names(), sorted);
        assert_eq!(registry.groups(), vec!["read", "units", "write"]);
        assert_eq!(registry.iter().count(), 10);
        assert_eq!(builtin_specs().len(), 10);
    }

    #[test]
    fn commands_can_be_looked_up_by_name_and_alias() {
        let registry = registry();
        assert_eq!(registry.find("set").map(CommandSpec::name), Some("set"));
        assert_eq!(registry.find("s").map(CommandSpec::name), Some("set"));
        assert_eq!(
            registry.find("conv").map(CommandSpec::name),
            Some("convert")
        );
        assert_eq!(registry.find("clear").map(CommandSpec::name), Some("unset"));
        assert_eq!(registry.find(" Set ").map(CommandSpec::name), Some("set"));
        assert_eq!(registry.find("missing"), None);
        assert_eq!(registry.suggest("gte").as_deref(), Some("get"));
        assert_eq!(registry.suggest("zork").as_deref(), None);
        let spec = registry.find("get").expect("present");
        assert!(spec.matches("get"));
        assert!(!spec.matches("set"));
        assert_eq!(spec.group(), "read");
        assert_eq!(spec.usage(), "get <key>");
        assert!(spec.summary().contains("effective value"));
    }

    #[test]
    fn duplicate_commands_are_rejected() {
        let spec = __command_spec_get;
        assert!(matches!(
            Registry::new(vec![spec, spec]),
            Err(CmdError::BadUsage { .. })
        ));
        let conflicting = CommandSpec::new(
            "getter",
            "read",
            "another getter",
            "getter",
            &["get"],
            spec.handler(),
        );
        assert!(matches!(
            Registry::new(vec![spec, conflicting]),
            Err(CmdError::BadUsage { .. })
        ));
        let mut registry = registry();
        assert!(registry.extend(vec![conflicting]).is_err());
        assert_eq!(registry.len(), 10);
    }

    #[test]
    fn dispatch_reports_unknown_and_empty_lines() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        assert_eq!(
            registry.dispatch(&mut engine, "   "),
            Err(CmdError::EmptyCommandLine)
        );
        assert_eq!(
            registry.dispatch(&mut engine, "gte loop.gain"),
            Err(CmdError::UnknownCommand {
                name: "gte".to_string(),
                suggestion: Some("get".to_string())
            })
        );
        assert_eq!(
            registry.dispatch(&mut engine, "zork"),
            Err(CmdError::UnknownCommand {
                name: "zork".to_string(),
                suggestion: None
            })
        );
    }

    #[test]
    fn get_and_set_round_trip() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        let outcome = registry
            .dispatch(&mut engine, "get loop.gain")
            .expect("get works");
        assert_eq!(
            outcome.lines(),
            [
                "loop.gain = 1.25".to_string(),
                "source: default".to_string()
            ]
        );
        assert_eq!(outcome.text(), "loop.gain = 1.25\nsource: default");
        let outcome = registry
            .dispatch(&mut engine, "set loop.gain 2.5")
            .expect("set works");
        assert_eq!(outcome.lines(), ["loop.gain = 2.5".to_string()]);
        assert_eq!(engine.get_float("loop.gain").expect("float"), 2.5);
        let outcome = registry
            .dispatch(&mut engine, "get kp")
            .expect("aliases work");
        assert_eq!(
            outcome.lines(),
            [
                "loop.gain = 2.5".to_string(),
                "source: argument".to_string()
            ]
        );
        let outcome = registry
            .dispatch(&mut engine, "set site.name bay 7")
            .expect("values may contain spaces");
        assert_eq!(outcome.lines(), ["site.name = bay 7".to_string()]);
        assert_eq!(engine.get_text("site.name").expect("text"), "bay 7");
    }

    #[test]
    fn argument_errors_are_precise() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        assert_eq!(
            registry.dispatch(&mut engine, "get"),
            Err(CmdError::MissingArgument {
                command: "get",
                argument: "key"
            })
        );
        assert_eq!(
            registry.dispatch(&mut engine, "get loop.gain extra"),
            Err(CmdError::TooManyArguments {
                command: "get",
                expected: 1
            })
        );
        assert_eq!(
            registry.dispatch(&mut engine, "set loop.gain"),
            Err(CmdError::MissingArgument {
                command: "set",
                argument: "value"
            })
        );
        assert_eq!(
            registry.dispatch(&mut engine, "set loop.gain loud"),
            Err(CmdError::Config(ConfigError::InvalidValue {
                key: "loop.gain".to_string(),
                reason: "`loud` is not a finite number".to_string()
            }))
        );
        assert_eq!(
            registry.dispatch(&mut engine, "apply"),
            Err(CmdError::MissingArgument {
                command: "apply",
                argument: "key=value"
            })
        );
        assert_eq!(
            registry.dispatch(&mut engine, "apply loop.gian=1"),
            Err(CmdError::Config(ConfigError::UnknownKey {
                key: "loop.gian".to_string(),
                suggestion: Some("loop.gain".to_string())
            }))
        );
        assert!(matches!(
            registry.dispatch(&mut engine, "unset"),
            Err(CmdError::MissingArgument { .. })
        ));
        assert!(matches!(
            registry.dispatch(&mut engine, "list a b"),
            Err(CmdError::TooManyArguments { .. })
        ));
        assert!(matches!(
            registry.dispatch(&mut engine, "validate now"),
            Err(CmdError::TooManyArguments { .. })
        ));
    }

    #[test]
    fn unset_and_reset_report_the_new_state() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        registry
            .dispatch(&mut engine, "set loop.gain 3")
            .expect("set works");
        let outcome = registry
            .dispatch(&mut engine, "unset loop.gain")
            .expect("unset works");
        assert_eq!(
            outcome.lines(),
            ["unset loop.gain".to_string(), "effective: 1.25".to_string()]
        );
        let outcome = registry
            .dispatch(&mut engine, "clear loop.gain")
            .expect("alias works");
        assert_eq!(
            outcome.lines(),
            [
                "loop.gain had no explicit value".to_string(),
                "effective: 1.25".to_string()
            ]
        );
        registry
            .dispatch(&mut engine, "set loop.gain 4")
            .expect("set works");
        let outcome = registry
            .dispatch(&mut engine, "reset loop.gain")
            .expect("reset works");
        assert_eq!(outcome.lines(), ["reset loop.gain".to_string()]);
        assert_eq!(engine.get_float("loop.gain").expect("float"), 1.25);
        let outcome = registry
            .dispatch(&mut engine, "reset")
            .expect("reset all works");
        assert_eq!(
            outcome.lines(),
            ["reset every setting to its default".to_string()]
        );
        assert_eq!(engine.count_by_source()[&Source::Default], 28);
    }

    #[test]
    fn list_filters_by_prefix() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        let outcome = registry
            .dispatch(&mut engine, "list loop.")
            .expect("list works");
        assert_eq!(outcome.len(), 4);
        assert_eq!(outcome.lines()[0], "loop.enabled = true (default)");
        assert_eq!(outcome.lines()[1], "loop.gain = 1.25 (default)");
        assert_eq!(outcome.lines()[2], "loop.integral = 30 s (default)");
        assert_eq!(outcome.lines()[3], "loop.name = primary (default)");
        let outcome = registry
            .dispatch(&mut engine, "list nothing")
            .expect("list works");
        assert_eq!(outcome.lines(), ["no settings match `nothing`".to_string()]);
        let outcome = registry.dispatch(&mut engine, "list").expect("list works");
        assert_eq!(outcome.len(), 29);
    }

    #[test]
    fn describe_lists_schema_details() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        let outcome = registry
            .dispatch(&mut engine, "describe kp")
            .expect("describe works");
        assert_eq!(outcome.lines()[0], "loop.gain (setting)");
        assert!(
            outcome
                .lines()
                .iter()
                .any(|line| line == "kind: Float(Range { min: 0.0, max: 1000.0 })"),
            "unexpected lines: {:?}",
            outcome.lines()
        );
        assert!(outcome.lines().contains(&"aliases: [\"kp\"]".to_string()));
        assert!(
            outcome
                .lines()
                .contains(&"accepts: number in 0..1000".to_string())
        );
        assert!(outcome.lines().contains(&"current: 1.25".to_string()));
        assert!(
            outcome
                .lines()
                .contains(&"doc: Proportional gain of the controller.".to_string())
        );
        let outcome = registry
            .dispatch(&mut engine, "describe legacy.offset")
            .expect("describe works");
        assert!(outcome.lines().contains(&"default: none".to_string()));
        assert!(
            outcome
                .lines()
                .contains(&"deprecated: Superseded by site.elevation.".to_string())
        );
    }

    #[test]
    fn convert_uses_the_unit_registry() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        let outcome = registry
            .dispatch(&mut engine, "convert 1 bar kPa")
            .expect("convert works");
        assert_eq!(outcome.lines(), ["1 bar = 100 kPa".to_string()]);
        let outcome = registry
            .dispatch(&mut engine, "convert 0 degC K")
            .expect("convert works");
        assert_eq!(outcome.lines(), ["0 degC = 273.15 K".to_string()]);
        let outcome = registry
            .dispatch(&mut engine, "conv 1500 ms s")
            .expect("alias works");
        assert_eq!(outcome.lines(), ["1500 ms = 1.5 s".to_string()]);
        assert_eq!(
            registry.dispatch(&mut engine, "convert 1 bar s"),
            Err(CmdError::Unit(crate::error::UnitError::DimensionMismatch {
                expected: "LEN-1 MASS1 TIME-2".to_string(),
                found: "TIME1".to_string()
            }))
        );
        assert_eq!(
            registry.dispatch(&mut engine, "convert x bar kPa"),
            Err(CmdError::BadUsage {
                command: "convert",
                hint: "`x` is not a number".to_string()
            })
        );
        assert!(matches!(
            registry.dispatch(&mut engine, "convert 1 bar kPa extra"),
            Err(CmdError::TooManyArguments { .. })
        ));
        assert!(matches!(
            registry.dispatch(&mut engine, "convert 1 bar"),
            Err(CmdError::MissingArgument { .. })
        ));
    }

    #[test]
    fn units_lists_symbols_of_a_dimension() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        let outcome = registry
            .dispatch(&mut engine, "units TIME1")
            .expect("units works");
        assert_eq!(outcome.lines()[0], "day (canonical = value * 86400)");
        assert_eq!(
            outcome.lines().last().map(String::as_str),
            Some("us (canonical = value * 1e-6)")
        );
        assert!(outcome.lines().iter().all(|line| !line.is_empty()));
        let outcome = registry
            .dispatch(&mut engine, "units TEMP1")
            .expect("units works");
        assert_eq!(outcome.len(), 4);
        let outcome = registry
            .dispatch(&mut engine, "units")
            .expect("units works");
        assert!(
            outcome
                .lines()
                .iter()
                .any(|line| line.starts_with("TEMP1: sensor.return.temperature"))
        );
        assert!(matches!(
            registry.dispatch(&mut engine, "units BOGUS1"),
            Err(CmdError::Unit(_))
        ));
    }

    #[test]
    fn apply_sets_several_values() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        let outcome = registry
            .dispatch(&mut engine, "apply loop.gain=2.5 sample.averaging=8")
            .expect("apply works");
        assert_eq!(outcome.lines(), ["applied 2 assignments".to_string()]);
        assert_eq!(engine.get_float("loop.gain").expect("float"), 2.5);
        assert_eq!(engine.get_int("sample.averaging").expect("int"), 8);
        let outcome = registry
            .dispatch(&mut engine, "apply loop.gain=2.5")
            .expect("apply works");
        assert_eq!(outcome.lines(), ["applied 1 assignment".to_string()]);
    }

    #[test]
    fn validate_reports_issues() {
        let registry = registry();
        let mut engine = Engine::with_baseline();
        let outcome = registry
            .dispatch(&mut engine, "validate")
            .expect("validate works");
        assert_eq!(outcome.lines(), ["ok: 35 checks, no issues".to_string()]);
        registry
            .dispatch(&mut engine, "set fluid.flow.maximum 0.001 m3/s")
            .expect("set works");
        let outcome = registry
            .dispatch(&mut engine, "validate")
            .expect("validate works");
        assert_eq!(
            outcome.lines()[0],
            "error: fluid.flow.maximum: maximum flow must be at least the nominal flow"
        );
        assert!(outcome.lines()[1].starts_with("1 issue(s) after "));
    }

    #[test]
    fn help_and_outcomes_render() {
        let registry = registry();
        let help = registry.help();
        assert!(help.starts_with("command"));
        assert!(help.contains("convert"));
        assert!(help.contains("Convert a value between units"));
        let mut outcome = CmdOutcome::new();
        outcome.push("first");
        outcome.extend(vec!["second".to_string()]);
        assert_eq!(outcome.text(), "first\nsecond");
        assert_eq!(outcome.to_string(), "first\nsecond");
        assert_eq!(outcome.len(), 2);
        assert!(!outcome.is_empty());
        let collected: CmdOutcome = vec!["a".to_string(), "b".to_string()].into_iter().collect();
        assert_eq!(collected.lines(), ["a".to_string(), "b".to_string()]);
        let empty = CmdOutcome::new();
        assert!(empty.is_empty());
        assert_eq!(empty.text(), "");
    }

    #[test]
    fn command_specs_describe_themselves() {
        use crate::describe::Describe;
        let spec = __command_spec_set;
        assert_eq!(spec.name(), "set");
        assert_eq!(spec.aliases(), &["s"]);
        assert_eq!(CommandSpec::NAME, "command");
        assert_eq!(
            spec.field_names(),
            &["name", "group", "summary", "usage", "aliases"]
        );
        assert!(spec.describe().get("handler").is_none());
        assert_eq!(spec.describe().get("name"), Some("\"set\""));
        let handler = spec.handler();
        let mut engine = Engine::with_baseline();
        let outcome = handler(&mut engine, &["loop.gain", "7"]).expect("handler works");
        assert_eq!(outcome.lines(), ["loop.gain = 7".to_string()]);
    }

    #[test]
    fn errors_display_the_reason() {
        assert_eq!(
            CmdError::UnknownCommand {
                name: "gte".to_string(),
                suggestion: Some("get".to_string()),
            }
            .to_string(),
            "unknown command `gte`, did you mean `get`?"
        );
        assert_eq!(
            CmdError::MissingArgument {
                command: "get",
                argument: "key"
            }
            .to_string(),
            "`get` needs a <key> argument"
        );
        assert_eq!(
            CmdError::TooManyArguments {
                command: "get",
                expected: 1
            }
            .to_string(),
            "`get` accepts at most 1 argument(s)"
        );
        assert_eq!(
            CmdError::BadUsage {
                command: "convert",
                hint: "nope".to_string()
            }
            .to_string(),
            "`convert`: nope"
        );
    }
}
