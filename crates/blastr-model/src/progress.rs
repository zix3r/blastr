//! Progress reporting for long-running operations.

/// Receives progress reports from long-running operations.
pub trait Progress: Send + Sync {
    /// Called when `phase` has completed `done` units out of `total` (if known).
    fn report(&self, phase: &str, done: u64, total: Option<u64>);
}

impl<F> Progress for F
where
    F: Fn(&str, u64, Option<u64>) + Send + Sync,
{
    fn report(&self, phase: &str, done: u64, total: Option<u64>) {
        self(phase, done, total);
    }
}

/// A [`Progress`] that ignores every report.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _phase: &str, _done: u64, _total: Option<u64>) {}
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn closure_receives_reports_through_dyn_progress() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let progress: &dyn Progress = &|phase: &str, done: u64, total: Option<u64>| {
            sink.lock().unwrap().push((phase.to_owned(), done, total));
        };

        progress.report("index", 1, Some(4));
        progress.report("index", 4, None);

        let seen = seen.lock().unwrap();
        assert_eq!(
            *seen,
            vec![
                ("index".to_owned(), 1, Some(4)),
                ("index".to_owned(), 4, None)
            ]
        );
    }

    #[test]
    fn no_progress_can_be_used_as_dyn_progress() {
        let progress: &dyn Progress = &NoProgress;
        progress.report("anything", 0, None);
    }
}
