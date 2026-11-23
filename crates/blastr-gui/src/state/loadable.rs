//! A value that arrives later, and what is known about it meanwhile.

/// How far a running task has got.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Progress {
    /// The phase the engine reported last, such as `extract head`.
    pub phase: String,
    /// Units of the phase that are done.
    pub done: u64,
    /// Units the phase has in all, when the engine knows.
    pub total: Option<u64>,
}

impl Progress {
    /// Progress at the start of `phase`, with nothing counted yet.
    pub fn phase(phase: impl Into<String>) -> Progress {
        Progress {
            phase: phase.into(),
            done: 0,
            total: None,
        }
    }

    /// The finished share of the phase, between 0 and 1, when it is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// The phase in words a person reads in the status bar.
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
            other => return capitalised(other),
        };
        text.to_owned()
    }
}

/// `text` with its first letter in upper case.
fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// Why a task did not produce its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What went wrong.
    pub message: String,
    /// What to do next, when there is something useful to say.
    pub hint: Option<String>,
}

impl Failure {
    /// A failure with a message and no hint.
    pub fn new(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            hint: None,
        }
    }
}

/// A value a background task produces.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Loadable<T> {
    /// Nothing was asked for yet.
    #[default]
    Empty,
    /// The task is running.
    Loading(Progress),
    /// The value arrived.
    Ready(T),
    /// The task failed.
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

    /// The progress of the running task.
    pub fn progress(&self) -> Option<&Progress> {
        match self {
            Loadable::Loading(progress) => Some(progress),
            _ => None,
        }
    }

    /// Whether the task is still running.
    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading(_))
    }

    /// Why the task failed.
    pub fn failure(&self) -> Option<&Failure> {
        match self {
            Loadable::Failed(failure) => Some(failure),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fraction_needs_a_total() {
        assert_eq!(Progress::phase("impact").fraction(), None);
        let half = Progress {
            phase: "extract head".to_owned(),
            done: 5,
            total: Some(10),
        };
        assert_eq!(half.fraction(), Some(0.5));
        let over = Progress {
            done: 12,
            ..half.clone()
        };
        assert_eq!(over.fraction(), Some(1.0));
        let empty = Progress {
            total: Some(0),
            ..half
        };
        assert_eq!(empty.fraction(), None);
    }

    #[test]
    fn phases_read_as_plain_words() {
        assert_eq!(Progress::phase("impact").label(), "Tracing impact");
        assert_eq!(Progress::phase("link").label(), "Link");
        assert_eq!(Progress::phase("").label(), "Starting");
    }

    #[test]
    fn only_a_ready_value_is_handed_out() {
        let mut value: Loadable<u32> = Loadable::Empty;
        assert_eq!(value.ready(), None);
        value = Loadable::Loading(Progress::phase("impact"));
        assert!(value.is_loading());
        assert_eq!(
            value.progress().map(|progress| progress.label()),
            Some("Tracing impact".to_owned())
        );
        value = Loadable::Ready(3);
        assert_eq!(value.ready(), Some(&3));
        value = Loadable::Failed(Failure::new("no"));
        assert_eq!(value.failure().map(|f| f.message.as_str()), Some("no"));
    }
}
