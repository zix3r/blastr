//! The `key=value` command syntax the front end speaks.

use std::fmt;

use domain::{Cents, DomainError};
use storage::StorageError;

/// A parse or execution failure in the command layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    message: String,
}

impl CommandError {
    /// A free form failure.
    pub fn new(message: impl Into<String>) -> CommandError {
        CommandError {
            message: message.into(),
        }
    }

    /// The message without the `error: ` prefix the script runner adds.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

impl From<DomainError> for CommandError {
    fn from(error: DomainError) -> CommandError {
        CommandError::new(error.message())
    }
}

impl From<StorageError> for CommandError {
    fn from(error: StorageError) -> CommandError {
        CommandError::new(error.message())
    }
}

/// One parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    name: String,
    pairs: Vec<(String, String)>,
}

impl Command {
    /// Splits `line` into a command name and its `key=value` arguments.
    pub fn parse(line: &str) -> Result<Command, CommandError> {
        let mut tokens = tokenize(line)?.into_iter();
        let name = tokens
            .next()
            .ok_or_else(|| CommandError::new("empty command"))?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for token in tokens {
            let (key, value) = token
                .split_once('=')
                .ok_or_else(|| CommandError::new(format!("expected key=value, found '{token}'")))?;
            if key.is_empty() {
                return Err(CommandError::new(format!("empty key in '{token}'")));
            }
            if pairs.iter().any(|(existing, _)| existing == key) {
                return Err(CommandError::new(format!("duplicate key '{key}'")));
            }
            pairs.push((key.to_string(), value.to_string()));
        }
        Ok(Command {
            name: name.to_ascii_lowercase(),
            pairs,
        })
    }

    /// The command name, lower cased.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw value of `key`, if the line carried it.
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// The value of `key`, or an error naming the command.
    pub fn required(&self, key: &str) -> Result<&str, CommandError> {
        self.raw(key).ok_or_else(|| {
            CommandError::new(format!("missing '{key}=' for command '{}'", self.name))
        })
    }

    /// The value of `key` as a whole number.
    pub fn number(&self, key: &str) -> Result<u64, CommandError> {
        let text = self.required(key)?;
        text.parse().map_err(|_| {
            CommandError::new(format!("'{key}' must be a whole number, found '{text}'"))
        })
    }

    /// The value of `key` as a non negative count.
    pub fn count(&self, key: &str) -> Result<u32, CommandError> {
        let text = self.required(key)?;
        text.parse()
            .map_err(|_| CommandError::new(format!("'{key}' must be a count, found '{text}'")))
    }

    /// The value of `key` as a non negative amount of cents.
    pub fn cents(&self, key: &str) -> Result<Cents, CommandError> {
        let text = self.required(key)?;
        let cents: Cents = text
            .parse()
            .map_err(|_| CommandError::new(format!("'{key}' must be cents, found '{text}'")))?;
        if cents < 0 {
            return Err(CommandError::new(format!("'{key}' must not be negative")));
        }
        Ok(cents)
    }

    /// Whether `key` is present and switched on.
    pub fn flag(&self, key: &str) -> bool {
        match self.raw(key) {
            Some(value) => {
                value.eq_ignore_ascii_case("on")
                    || value.eq_ignore_ascii_case("true")
                    || value.eq_ignore_ascii_case("yes")
            }
            None => false,
        }
    }
}

/// Splits a line on whitespace, honouring double quoted runs.
pub(crate) fn tokenize(line: &str) -> Result<Vec<String>, CommandError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            whitespace if whitespace.is_whitespace() && !quoted => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            other => {
                current.push(other);
                started = true;
            }
        }
    }
    if quoted {
        return Err(CommandError::new("unterminated quoted value"));
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_keeps_quoted_runs_together() {
        assert_eq!(
            tokenize("add-product name=\"Blue Mug\" sku=MUG-1").unwrap(),
            vec!["add-product", "name=Blue Mug", "sku=MUG-1"]
        );
        assert_eq!(tokenize("   ").unwrap(), Vec::<String>::new());
        assert!(tokenize("name=\"open").is_err());
    }

    #[test]
    fn parse_splits_name_and_pairs() {
        let command = Command::parse("ADD-Product sku=mug-1 price=1299").unwrap();
        assert_eq!(command.name(), "add-product");
        assert_eq!(command.raw("sku"), Some("mug-1"));
        assert_eq!(command.raw("missing"), None);
        assert_eq!(command.number("price"), Ok(1299));
        assert_eq!(
            command.required("missing").unwrap_err().message(),
            "missing 'missing=' for command 'add-product'"
        );
    }

    #[test]
    fn parse_rejects_malformed_arguments() {
        assert!(Command::parse("add-product sku").is_err());
        assert!(Command::parse("add-product =1").is_err());
        assert_eq!(
            Command::parse("report threshold=low")
                .unwrap()
                .count("threshold")
                .unwrap_err()
                .message(),
            "'threshold' must be a count, found 'low'"
        );
        assert_eq!(
            Command::parse("report threshold=-2")
                .unwrap()
                .cents("threshold")
                .unwrap_err()
                .message(),
            "'threshold' must not be negative"
        );
    }

    #[test]
    fn flags_accept_the_usual_spellings() {
        let command = Command::parse("add-product sku=MUG-1 hazard=yes express=off").unwrap();
        assert!(command.flag("hazard"));
        assert!(!command.flag("express"));
        assert!(!command.flag("absent"));
        assert_eq!(command.cents("hooks").unwrap_or_default(), 0);
        assert_eq!(
            command.count("sku").unwrap_err().message(),
            "'sku' must be a count, found 'MUG-1'"
        );
    }
}
