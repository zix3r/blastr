//! Background jobs: which one is current, how it is cancelled, and how its
//! progress reaches the window.
//!
//! Nothing here starts a thread. A [`TaskSlot`] only keeps the books for one
//! kind of job, so that a result arriving from a job that was replaced or
//! cancelled is recognised and dropped; the window owns the executor.

use std::sync::{Arc, Mutex};

use blastr_model::CancelToken;

use super::loadable::Progress;

/// What a job is given when it starts.
#[derive(Debug, Clone)]
pub struct Ticket {
    /// Identifies the job; results carry it back.
    pub generation: u64,
    /// Cancelled when the job is replaced or the user stops it.
    pub cancel: CancelToken,
    /// Where the job reports its progress.
    pub progress: ProgressCell,
}

/// One kind of background job, of which only the newest counts.
#[derive(Debug, Default)]
pub struct TaskSlot {
    generation: u64,
    running: Option<(CancelToken, ProgressCell)>,
}

impl TaskSlot {
    /// Starts a new job, cancelling the one that was running.
    pub fn begin(&mut self) -> Ticket {
        self.cancel();
        self.generation += 1;
        let cancel = CancelToken::new();
        let progress = ProgressCell::default();
        self.running = Some((cancel.clone(), progress.clone()));
        Ticket {
            generation: self.generation,
            cancel,
            progress,
        }
    }

    /// Cancels the running job. Returns whether there was one.
    pub fn cancel(&mut self) -> bool {
        match self.running.take() {
            Some((cancel, _)) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Whether `generation` is the job that is running now.
    pub fn is_current(&self, generation: u64) -> bool {
        self.running.is_some() && self.generation == generation
    }

    /// Whether a job is running.
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// The generation of the job that is running, or of the newest one when
    /// none is.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Marks the job as finished. Returns whether it was the current one.
    pub fn finish(&mut self, generation: u64) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.running = None;
        true
    }

    /// The newest progress of the running job, when it changed since `seen`.
    pub fn poll_progress(&self, seen: &mut u64) -> Option<Progress> {
        let (_, progress) = self.running.as_ref()?;
        progress.take_if_changed(seen)
    }
}

/// The latest progress report of one job, written from a worker thread and
/// read from the window.
#[derive(Debug, Clone, Default)]
pub struct ProgressCell {
    inner: Arc<Mutex<(u64, Progress)>>,
}

impl ProgressCell {
    /// The latest report, when there is one newer than the version in `seen`.
    pub fn take_if_changed(&self, seen: &mut u64) -> Option<Progress> {
        let guard = self.inner.lock().ok()?;
        if guard.0 == *seen {
            return None;
        }
        *seen = guard.0;
        Some(guard.1.clone())
    }
}

impl blastr_model::Progress for ProgressCell {
    fn report(&self, phase: &str, done: u64, total: Option<u64>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.0 += 1;
            if guard.1.phase != phase {
                guard.1.phase = phase.to_owned();
            }
            guard.1.done = done;
            guard.1.total = total;
        }
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::Progress as _;

    use super::*;

    #[test]
    fn a_new_job_cancels_the_previous_one() {
        let mut slot = TaskSlot::default();
        let first = slot.begin();
        assert!(slot.is_current(first.generation));
        assert!(!first.cancel.is_cancelled());

        let second = slot.begin();

        assert!(first.cancel.is_cancelled());
        assert!(!second.cancel.is_cancelled());
        assert!(!slot.is_current(first.generation));
        assert!(slot.is_current(second.generation));
    }

    #[test]
    fn a_finished_job_is_no_longer_current() {
        let mut slot = TaskSlot::default();
        let ticket = slot.begin();

        assert!(slot.finish(ticket.generation));

        assert!(!slot.is_running());
        assert!(!slot.is_current(ticket.generation));
        assert!(
            !slot.finish(ticket.generation),
            "finishing twice is ignored"
        );
        assert!(!ticket.cancel.is_cancelled());
    }

    #[test]
    fn a_stale_job_cannot_finish_the_current_one() {
        let mut slot = TaskSlot::default();
        let old = slot.begin();
        let new = slot.begin();

        assert!(!slot.finish(old.generation));

        assert!(slot.is_current(new.generation));
    }

    #[test]
    fn cancelling_stops_the_job_and_reports_whether_one_ran() {
        let mut slot = TaskSlot::default();
        assert!(!slot.cancel());
        let ticket = slot.begin();

        assert!(slot.cancel());

        assert!(ticket.cancel.is_cancelled());
        assert!(!slot.is_running());
    }

    #[test]
    fn progress_is_read_once_per_report() {
        let mut slot = TaskSlot::default();
        let ticket = slot.begin();
        let mut seen = 0;
        assert_eq!(slot.poll_progress(&mut seen), None);

        ticket.progress.report("extract head", 3, Some(9));

        let progress = slot.poll_progress(&mut seen).expect("a report");
        assert_eq!(progress.phase, "extract head");
        assert_eq!((progress.done, progress.total), (3, Some(9)));
        assert_eq!(slot.poll_progress(&mut seen), None);
    }
}
