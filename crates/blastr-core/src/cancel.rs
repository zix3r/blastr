//! Cancellation of long-running operations.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{Error, Result};

/// A cheap, cloneable flag used to cancel a running operation from another thread.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    /// Creates a token that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. All clones observe it.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Reports whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Returns `Err(Error::Cancelled)` once cancellation has been requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        assert!(token.check().is_ok());
    }

    #[test]
    fn cancel_is_observed_by_clones() {
        let token = CancelToken::new();
        let clone = token.clone();

        clone.cancel();

        assert!(token.is_cancelled());
        assert!(matches!(token.check(), Err(Error::Cancelled)));
    }
}
