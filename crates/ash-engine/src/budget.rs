use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use ash_protocol::request::Budget;
use thiserror::Error;
use tokio::time::Instant;

/// Concurrent counters and a fixed monotonic deadline for one program.
pub struct BudgetTracker {
    tokens: u32,
    output_limit: u64,
    record_limit: u32,
    deadline: Instant,
    output_used: AtomicU64,
    records_used: AtomicU32,
}

impl BudgetTracker {
    pub fn new(request: Budget, output_limit: u64) -> Result<Self, BudgetError> {
        if output_limit == 0 {
            return Err(BudgetError::InvalidOutputLimit);
        }
        Ok(Self {
            tokens: request.tokens(),
            output_limit,
            record_limit: request.records(),
            deadline: Instant::now() + Duration::from_millis(request.millis()),
            output_used: AtomicU64::new(0),
            records_used: AtomicU32::new(0),
        })
    }

    pub fn reserve_output(&self, bytes: u64) -> Result<(), BudgetError> {
        reserve_atomic_u64(&self.output_used, bytes, self.output_limit)
            .map_err(|requested| BudgetError::Output { requested })
    }

    pub fn reserve_records(&self, records: u32) -> Result<(), BudgetError> {
        reserve_atomic_u32(&self.records_used, records, self.record_limit)
            .map_err(|requested| BudgetError::Records { requested })
    }

    pub fn check_deadline(&self) -> Result<(), BudgetError> {
        if Instant::now() >= self.deadline {
            Err(BudgetError::Deadline)
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn remaining(&self) -> BudgetRemaining {
        BudgetRemaining {
            tokens: self.tokens,
            output_bytes: self
                .output_limit
                .saturating_sub(self.output_used.load(Ordering::Acquire)),
            records: self
                .record_limit
                .saturating_sub(self.records_used.load(Ordering::Acquire)),
            deadline: self.deadline,
        }
    }

    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BudgetRemaining {
    pub tokens: u32,
    pub output_bytes: u64,
    pub records: u32,
    pub deadline: Instant,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BudgetError {
    #[error("program output byte limit must be non-zero")]
    InvalidOutputLimit,
    #[error("output reservation would raise usage to {requested} bytes")]
    Output { requested: u64 },
    #[error("record reservation would raise usage to {requested} records")]
    Records { requested: u32 },
    #[error("program deadline has elapsed")]
    Deadline,
}

fn reserve_atomic_u64(counter: &AtomicU64, amount: u64, limit: u64) -> Result<(), u64> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let requested = current.checked_add(amount).ok_or(u64::MAX)?;
        if requested > limit {
            return Err(requested);
        }
        match counter.compare_exchange_weak(current, requested, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Ok(()),
            Err(actual) => current = actual,
        }
    }
}

fn reserve_atomic_u32(counter: &AtomicU32, amount: u32, limit: u32) -> Result<(), u32> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let requested = current.checked_add(amount).ok_or(u32::MAX)?;
        if requested > limit {
            return Err(requested);
        }
        match counter.compare_exchange_weak(current, requested, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Ok(()),
            Err(actual) => current = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use ash_protocol::request::Budget;

    use super::{BudgetError, BudgetTracker};

    #[test]
    fn concurrent_reservations_never_exceed_the_declared_limits() {
        let tracker = Arc::new(
            BudgetTracker::new(Budget::new(100, 10, 30_000).expect("budget"), 100)
                .expect("tracker"),
        );
        let threads: Vec<_> = (0..20)
            .map(|_| {
                let tracker = Arc::clone(&tracker);
                thread::spawn(move || tracker.reserve_output(10).is_ok())
            })
            .collect();
        let accepted = threads
            .into_iter()
            .map(|thread| thread.join().expect("join"))
            .filter(|accepted| *accepted)
            .count();
        assert_eq!(accepted, 10);
        assert_eq!(tracker.remaining().output_bytes, 0);
        assert!(matches!(
            tracker.reserve_output(1),
            Err(BudgetError::Output { .. })
        ));
    }

    #[test]
    fn failed_record_reservation_does_not_consume_quota() {
        let tracker =
            BudgetTracker::new(Budget::new(100, 2, 30_000).expect("budget"), 100).expect("tracker");
        assert!(tracker.reserve_records(3).is_err());
        assert_eq!(tracker.remaining().records, 2);
        tracker.reserve_records(2).expect("reserve exact limit");
    }
}
