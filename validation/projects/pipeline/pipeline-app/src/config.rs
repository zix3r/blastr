//! Parses the tiny pipeline language used by the demo binary.
//!
//! A spec is a `|`-separated list of `kind` or `kind:argument` tokens:
//!
//! ```text
//! level | filter:error | dedup | truncate:40
//! ```

use pipeline_core::{Pipeline, PipelineError, Stage};
use pipeline_stages::{
    ByLevel, BySource, Dedup, Enrich, Filter, Group, Limit, Map, NormalizeLevel, Redact, Tag, Tap,
    Truncate, predicate_by_name, transform_by_name,
};

use crate::validate::Validate;

/// One parsed `kind:argument` token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpec {
    /// The stage family, for example `filter`.
    pub kind: String,
    /// The optional argument, for example `error`.
    pub argument: Option<String>,
}

impl StageSpec {
    /// Parses a single token.
    pub fn parse(token: &str) -> Result<Self, PipelineError> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return Err(PipelineError::config("empty stage in spec"));
        }
        match trimmed.split_once(':') {
            Some((kind, argument)) => {
                let kind = kind.trim();
                if kind.is_empty() {
                    return Err(PipelineError::config(format!(
                        "stage `{trimmed}` has no name"
                    )));
                }
                let argument = argument.trim();
                Ok(Self {
                    kind: kind.to_string(),
                    argument: (!argument.is_empty()).then(|| argument.to_string()),
                })
            }
            None => Ok(Self {
                kind: trimmed.to_string(),
                argument: None,
            }),
        }
    }

    /// The argument, or an error naming the stage that needed it.
    pub fn require_argument(&self) -> Result<&str, PipelineError> {
        self.argument.as_deref().ok_or_else(|| {
            PipelineError::config(format!("stage `{}` needs an argument", self.kind))
        })
    }

    /// The argument as a number, or `default` when the argument is absent.
    pub fn number(&self, default: usize) -> Result<usize, PipelineError> {
        match &self.argument {
            Some(argument) => Ok(argument.parse::<usize>()?),
            None => Ok(default),
        }
    }

    /// Parses an argument of the form `number` or `number:flag`.
    ///
    /// The flag is optional and defaults to `true`, which keeps the common
    /// `truncate:40` spelling short.
    pub fn number_and_flag(&self) -> Result<(usize, bool), PipelineError> {
        let argument = self.require_argument()?;
        match argument.split_once(':') {
            Some((number, flag)) => Ok((
                number.trim().parse::<usize>()?,
                flag.trim().parse::<bool>()?,
            )),
            None => Ok((argument.parse::<usize>()?, true)),
        }
    }

    /// Splits a `key=value` argument.
    pub fn assignment(&self) -> Result<(&str, &str), PipelineError> {
        let argument = self.require_argument()?;
        argument.split_once('=').ok_or_else(|| {
            PipelineError::config(format!(
                "stage `{}` expects `key=value`, got `{argument}`",
                self.kind
            ))
        })
    }
}

/// Parses a whole spec, rejecting specs without stages.
pub fn parse_spec(spec: &str) -> Result<Vec<StageSpec>, PipelineError> {
    let mut specs = Vec::new();
    for token in spec.split('|') {
        if token.trim().is_empty() {
            continue;
        }
        specs.push(StageSpec::parse(token)?);
    }
    if specs.is_empty() {
        return Err(PipelineError::config("spec contains no stages"));
    }
    Ok(specs)
}

/// Builds a single stage from a parsed spec.
pub fn build_stage(spec: &StageSpec) -> Result<Box<dyn Stage>, PipelineError> {
    let stage: Box<dyn Stage> = match spec.kind.as_str() {
        "filter" => {
            let name = spec.require_argument()?;
            let predicate = predicate_by_name(name)
                .ok_or_else(|| PipelineError::config(format!("unknown predicate `{name}`")))?;
            Box::new(Filter::from_fn(format!("filter:{name}"), predicate))
        }
        "map" => {
            let name = spec.require_argument()?;
            let transform = transform_by_name(name)
                .ok_or_else(|| PipelineError::config(format!("unknown transformation `{name}`")))?;
            Box::new(Map::from_fn(format!("map:{name}"), transform))
        }
        "tag" => {
            let (key, value) = spec.assignment()?;
            Box::new(Tag::new(key, value))
        }
        "truncate" => {
            let (width, ellipsis) = match spec.argument {
                Some(_) => spec.number_and_flag()?,
                None => (60, true),
            };
            if width < Truncate::MIN_WIDTH {
                return Err(PipelineError::config(format!(
                    "stage `truncate` needs at least {} characters",
                    Truncate::MIN_WIDTH
                )));
            }
            Box::new(Truncate::new(width).with_ellipsis(ellipsis))
        }
        "limit" => Box::new(Limit::new(spec.number(25)?)),
        "validate" => {
            let keys = spec.require_argument()?.split(',').map(str::trim);
            Box::new(Validate::new(keys))
        }
        "level" => Box::new(NormalizeLevel::new()),
        "group" => match spec.require_argument()? {
            "source" => Box::new(Group::new(BySource)),
            "level" => Box::new(Group::new(ByLevel)),
            other => {
                return Err(PipelineError::config(format!(
                    "stage `group` cannot group by `{other}`"
                )));
            }
        },
        "redact" => Box::new(Redact::new()),
        "dedup" => Box::new(Dedup::new()),
        "tap" => Box::new(Tap::new()),
        "enrich" => Box::new(Enrich::new()),
        other => {
            return Err(PipelineError::config(format!("unknown stage `{other}`")));
        }
    };
    Ok(stage)
}

/// Builds a pipeline from a spec.
pub fn build_pipeline(spec: &str) -> Result<Pipeline, PipelineError> {
    let specs = parse_spec(spec)?;
    let label = specs
        .iter()
        .map(|spec| spec.kind.as_str())
        .collect::<Vec<&str>>()
        .join(">");
    let mut pipeline = Pipeline::new(label);
    for spec in &specs {
        pipeline.push_boxed(build_stage(spec)?);
    }
    Ok(pipeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tokens_with_and_without_arguments() {
        let spec = StageSpec::parse(" filter:error ").unwrap();
        assert_eq!(spec.kind, "filter");
        assert_eq!(spec.argument.as_deref(), Some("error"));

        let spec = StageSpec::parse("dedup").unwrap();
        assert_eq!(spec.kind, "dedup");
        assert_eq!(spec.argument, None);

        let spec = StageSpec::parse("truncate:").unwrap();
        assert_eq!(spec.argument, None);
    }

    #[test]
    fn rejects_empty_tokens() {
        assert_eq!(
            StageSpec::parse("   ").unwrap_err(),
            PipelineError::config("empty stage in spec")
        );
        assert_eq!(
            StageSpec::parse(":error").unwrap_err(),
            PipelineError::config("stage `:error` has no name")
        );
    }

    #[test]
    fn argument_helpers() {
        let spec = StageSpec::parse("truncate:40").unwrap();
        assert_eq!(spec.number(60).unwrap(), 40);
        assert_eq!(
            StageSpec::parse("truncate").unwrap().number(60).unwrap(),
            60
        );
        assert_eq!(
            StageSpec::parse("truncate:many")
                .unwrap()
                .number(60)
                .unwrap_err(),
            PipelineError::Config(
                "expected a positive number: invalid digit found in string".to_string()
            )
        );
        assert_eq!(
            StageSpec::parse("dedup")
                .unwrap()
                .require_argument()
                .unwrap_err(),
            PipelineError::config("stage `dedup` needs an argument")
        );
        assert_eq!(
            StageSpec::parse("tag:env")
                .unwrap()
                .assignment()
                .unwrap_err(),
            PipelineError::config("stage `tag` expects `key=value`, got `env`")
        );
        assert_eq!(
            StageSpec::parse("tag:env=prod")
                .unwrap()
                .assignment()
                .unwrap(),
            ("env", "prod")
        );
        assert_eq!(
            StageSpec::parse("truncate:24:false")
                .unwrap()
                .number_and_flag()
                .unwrap(),
            (24, false)
        );
        assert_eq!(
            StageSpec::parse("truncate:24")
                .unwrap()
                .number_and_flag()
                .unwrap(),
            (24, true)
        );
        assert!(
            StageSpec::parse("truncate:24:maybe")
                .unwrap()
                .number_and_flag()
                .is_err()
        );
    }

    #[test]
    fn spec_parsing_skips_blanks() {
        let specs = parse_spec(" level | | dedup |truncate:12 ").unwrap();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].kind, "level");
        assert_eq!(specs[2].argument.as_deref(), Some("12"));
        assert_eq!(
            parse_spec("  ").unwrap_err(),
            PipelineError::config("spec contains no stages")
        );
    }

    #[test]
    fn pipeline_is_labelled_after_its_stages() {
        let pipeline = build_pipeline("level|dedup|truncate:20").unwrap();
        assert_eq!(pipeline.label(), "level>dedup>truncate");
        assert_eq!(pipeline.len(), 3);
        assert_eq!(
            pipeline.names(),
            vec!["normalize-level", "dedup", "truncate"]
        );
        assert_eq!(
            pipeline.describe(),
            "level>dedup>truncate: normalize-level(annotate) -> dedup(reduce) -> truncate(20 chars, ellipsis=true)"
        );
    }

    #[test]
    fn unknown_stages_and_arguments_are_configuration_errors() {
        assert_eq!(
            build_pipeline("level|shout").unwrap_err(),
            PipelineError::config("unknown stage `shout`")
        );
        assert_eq!(
            build_pipeline("filter:loud").unwrap_err(),
            PipelineError::config("unknown predicate `loud`")
        );
        assert_eq!(
            build_pipeline("map:reverse").unwrap_err(),
            PipelineError::config("unknown transformation `reverse`")
        );
        assert_eq!(
            build_pipeline("tag:env").unwrap_err(),
            PipelineError::config("stage `tag` expects `key=value`, got `env`")
        );
        let bad_width = build_pipeline("truncate:wide").unwrap_err();
        assert!(bad_width.is_config());
    }
}
