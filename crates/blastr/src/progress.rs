//! Progress and timing output on stderr.

use std::io::{IsTerminal, Write};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use blastr_core::Progress;

/// Reports the phases of an analysis on stderr and remembers when each began.
///
/// On a terminal every phase is printed once, as it starts. The phase times are
/// recorded either way, so `--timings` can print them after the result.
pub struct PhaseProgress {
    /// Whether stderr is a terminal and phase names should be shown.
    terminal: bool,
    /// The current phase and the start time of every phase so far.
    inner: Mutex<Inner>,
}

/// The state guarded by the mutex.
struct Inner {
    /// The name of the phase being reported.
    current: Option<String>,
    /// Each phase name with its start and end time, in the order they began.
    phases: Vec<Phase>,
}

/// One phase and its time span.
struct Phase {
    /// The name the engine reported.
    name: String,
    /// When the phase started.
    started: Instant,
    /// When the next phase started, or when [`PhaseProgress::finish`] was called.
    ended: Option<Instant>,
}

impl PhaseProgress {
    /// Creates a reporter; `terminal` tells whether phase names are shown.
    pub fn new(terminal: bool) -> Self {
        PhaseProgress {
            terminal,
            inner: Mutex::new(Inner {
                current: None,
                phases: Vec::new(),
            }),
        }
    }

    /// Creates a reporter that prints phase names when stderr is a terminal.
    pub fn from_stderr() -> Self {
        PhaseProgress::new(std::io::stderr().is_terminal())
    }

    /// Closes the phase that is still running.
    pub fn finish(&self) {
        let mut inner = self.lock();
        if let Some(last) = inner.phases.last_mut() {
            last.ended = Some(Instant::now());
        }
    }

    /// Prints the duration of every phase, and the total, to stderr.
    pub fn print_timings(&self) {
        let text = self.timings_text();
        if text.is_empty() {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(text.as_bytes());
        let _ = stderr.flush();
    }

    /// The timing lines, one per completed phase plus a total; empty when no
    /// phase was reported.
    pub fn timings_text(&self) -> String {
        let inner = self.lock();
        let mut entries: Vec<(String, Duration)> = Vec::new();
        let mut total = Duration::ZERO;
        for phase in &inner.phases {
            let Some(ended) = phase.ended else {
                continue;
            };
            let duration = ended.saturating_duration_since(phase.started);
            total += duration;
            entries.push((phase.name.clone(), duration));
        }
        if entries.is_empty() {
            return String::new();
        }
        let width = entries
            .iter()
            .map(|(name, _)| name.chars().count())
            .max()
            .unwrap_or(0)
            .max("total".len());
        let mut out = String::new();
        for (name, duration) in &entries {
            let seconds = duration.as_secs_f64();
            out.push_str(&format!("  {name:<width$}  {seconds:.2}s\n"));
        }
        let label = "total";
        let seconds = total.as_secs_f64();
        out.push_str(&format!("  {label:<width$}  {seconds:.2}s\n"));
        out
    }

    /// Locks the state, ignoring poisoning: the data is still consistent.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Progress for PhaseProgress {
    fn report(&self, phase: &str, _done: u64, _total: Option<u64>) {
        let fresh = {
            let mut inner = self.lock();
            if inner.current.as_deref() == Some(phase) {
                return;
            }
            let now = Instant::now();
            if let Some(last) = inner.phases.last_mut()
                && last.ended.is_none()
            {
                last.ended = Some(now);
            }
            inner.current = Some(phase.to_owned());
            let fresh = !inner.phases.iter().any(|known| known.name == phase);
            if fresh {
                inner.phases.push(Phase {
                    name: phase.to_owned(),
                    started: now,
                    ended: None,
                });
            }
            fresh
        };
        if fresh && self.terminal {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "  {phase}...");
            let _ = stderr.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_phase_is_recorded_once() {
        let progress = PhaseProgress::new(false);
        progress.report("snapshot", 1, Some(2));
        progress.report("snapshot", 2, Some(2));
        progress.report("classify", 0, None);
        progress.finish();

        let text = progress.timings_text();
        assert_eq!(text.matches("snapshot").count(), 1, "{text}");
        assert!(text.contains("  classify"), "{text}");
        assert!(text.contains("  total "), "{text}");
        assert!(text.ends_with("s\n"), "{text}");
    }

    #[test]
    fn no_phases_means_no_timings() {
        assert!(PhaseProgress::new(false).timings_text().is_empty());
    }
}
