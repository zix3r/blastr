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

    /// The phase and the step within it: the engine names a step of a
    /// phase as `load head: database`.
    fn parts(&self) -> (&str, Option<&str>) {
        match self.phase.split_once(": ") {
            Some((phase, step)) => (phase, Some(step)),
            None => (self.phase.as_str(), None),
        }
    }

    /// The phase in words.
    pub fn label(&self) -> String {
        let text = match self.parts().0 {
            "snapshot" => "Preparing the revision",
            "load base" => "Loading the base project",
            "load head" => "Loading the head project",
            "extract base" => "Extracting base",
            "extract head" => "Extracting head",
            "base" => "Reading base",
            "head" => "Reading head",
            "apply" => "Applying the saved files",
            "classify" => "Comparing",
            "impact" => "Tracing impact",
            "" => "Starting",
            other => return capitalised(other),
        };
        text.to_owned()
    }

    /// The step within the phase in words, when the engine named one.
    pub fn detail(&self) -> Option<String> {
        let text = match self.parts().1? {
            "manifest" => "Reading the manifest",
            "metadata" => "Running cargo metadata",
            "build scripts" => "Building build scripts and procedural macros",
            "database" => "Building rust-analyzer's database",
            other => return Some(capitalised(other)),
        };
        Some(text.to_owned())
    }
}

/// The stages a comparison goes through, in order, as the progress view
/// lists them.
pub const STAGES: [&str; 4] = ["Load", "Extract", "Compare", "Trace impact"];

impl Progress {
    /// Which of [`STAGES`] the phase belongs to.
    pub fn stage(&self) -> usize {
        let phase = self.parts().0;
        if phase.starts_with("extract") || phase == "apply" {
            1
        } else if phase == "classify" {
            2
        } else if phase == "impact" {
            3
        } else {
            0
        }
    }
}

/// A running time in a few characters: `8 s`, `2 min 05 s`.
pub fn elapsed_text(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min {:02} s", seconds / 60, seconds % 60)
    }
}

/// `text` with a capital first letter.
fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(phase: &str, done: u64, total: Option<u64>) -> Progress {
        Progress {
            phase: phase.to_owned(),
            done,
            total,
        }
    }

    #[test]
    fn a_phase_and_its_step_are_put_in_words() {
        let loading = progress("load head: build scripts", 0, None);
        assert_eq!(loading.label(), "Loading the head project");
        assert_eq!(
            loading.detail().as_deref(),
            Some("Building build scripts and procedural macros")
        );
        assert_eq!(loading.fraction(), None);
        let extracting = progress("extract base", 3, Some(12));
        assert_eq!(extracting.label(), "Extracting base");
        assert_eq!(extracting.detail(), None);
        assert_eq!(extracting.fraction(), Some(0.25));
        assert_eq!(progress("classify", 0, None).label(), "Comparing");
        assert_eq!(progress("something new", 0, None).label(), "Something new");
    }

    #[test]
    fn phases_fall_into_their_stages() {
        let stage = |phase: &str| progress(phase, 0, None).stage();
        assert_eq!(stage(""), 0);
        assert_eq!(stage("load base: metadata"), 0);
        assert_eq!(stage("extract head"), 1);
        assert_eq!(stage("classify"), 2);
        assert_eq!(stage("impact"), 3);
        assert_eq!(STAGES.len(), 4);
    }

    #[test]
    fn running_times_are_short() {
        use std::time::Duration;
        assert_eq!(elapsed_text(Duration::from_millis(8_900)), "8 s");
        assert_eq!(elapsed_text(Duration::from_secs(125)), "2 min 05 s");
    }
}
