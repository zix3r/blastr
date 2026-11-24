//! A value that arrives later: nothing yet, on its way, there, or failed.

use blastr_model::protocol::{Failure, Report};

/// How far a job has got, as the engine reported it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Progress {
    /// The phase, such as `extract head`.
    pub phase: String,
    /// Units of the phase that are done.
    pub done: u64,
    /// Units the phase has in all, when known.
    pub total: Option<u64>,
}

impl From<Report> for Progress {
    fn from(report: Report) -> Progress {
        Progress {
            phase: report.phase,
            done: report.done,
            total: report.total,
        }
    }
}

impl Progress {
    /// How much of the phase is done, between 0 and 1, when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|total| *total > 0)?;
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// The phase in words.
    pub fn label(&self) -> String {
        let text = match self.phase.as_str() {
            "snapshot" => "Preparing the revision",
            "load base" => "Loading the project",
            "load head" => "Loading the working tree",
            "extract base" => "Indexing the older side",
            "extract head" => "Indexing the newer side",
            "base" => "Reading the older side",
            "head" => "Reading the newer side",
            "apply" => "Reading the saved files",
            "classify" => "Classifying the changes",
            "impact" => "Tracing impact",
            "" => "Starting",
            other => {
                let mut characters = other.chars();
                return match characters.next() {
                    Some(first) => first.to_uppercase().chain(characters).collect(),
                    None => String::new(),
                };
            }
        };
        text.to_owned()
    }
}

/// A value that is loaded in the background.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Loadable<T> {
    /// Nothing was asked for.
    #[default]
    Empty,
    /// On its way.
    Loading(Progress),
    /// Arrived.
    Ready(T),
    /// Could not be loaded.
    Failed(Failure),
}

impl<T> Loadable<T> {
    /// The value, when it has arrived.
    pub fn ready(&self) -> Option<&T> {
        match self {
            Loadable::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// The progress, while it is on its way.
    pub fn progress(&self) -> Option<&Progress> {
        match self {
            Loadable::Loading(progress) => Some(progress),
            _ => None,
        }
    }

    /// Whether it is on its way.
    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading(_))
    }

    /// Why it could not be loaded.
    pub fn failure(&self) -> Option<&Failure> {
        match self {
            Loadable::Failed(failure) => Some(failure),
            _ => None,
        }
    }
}
