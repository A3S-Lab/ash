use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Cloneable, race-free cooperative cancellation signal.
#[derive(Clone, Default)]
pub struct CancellationToken {
    state: Arc<State>,
}

#[derive(Default)]
struct State {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    /// Cancels the token. Returns `true` only for the first caller.
    pub fn cancel(&self) -> bool {
        let first = !self.state.cancelled.swap(true, Ordering::AcqRel);
        if first {
            self.state.notify.notify_waiters();
        }
        first
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CancellationToken;

    #[tokio::test]
    async fn cancellation_before_and_after_wait_registration_is_observed() {
        let early = CancellationToken::default();
        assert!(early.cancel());
        early.cancelled().await;
        assert!(!early.cancel());

        let late = CancellationToken::default();
        let waiter = {
            let late = late.clone();
            tokio::spawn(async move { late.cancelled().await })
        };
        tokio::task::yield_now().await;
        assert!(late.cancel());
        waiter.await.expect("waiter");
    }
}
