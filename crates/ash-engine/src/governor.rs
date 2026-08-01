use std::num::NonZeroUsize;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::time::{Instant, timeout_at};

use crate::{CancellationToken, Parallelism};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GovernorLimits {
    pub programs: NonZeroUsize,
    pub nodes: NonZeroUsize,
    pub processes: NonZeroUsize,
    pub filesystem: NonZeroUsize,
    pub compute: NonZeroUsize,
}

impl GovernorLimits {
    #[must_use]
    pub fn from_parallelism(parallelism: Parallelism) -> Self {
        let nodes = parallelism
            .compute_workers()
            .get()
            .saturating_add(parallelism.max_filesystem_ops().get());
        Self {
            programs: non_zero(parallelism.io_workers().get().saturating_mul(4)),
            nodes: non_zero(nodes),
            processes: parallelism.max_processes(),
            filesystem: parallelism.max_filesystem_ops(),
            compute: parallelism.compute_workers(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermitKind {
    Program,
    Node,
    Process,
    Filesystem,
    Compute,
}

pub struct Governor {
    limits: GovernorLimits,
    programs: Arc<Semaphore>,
    nodes: Arc<Semaphore>,
    processes: Arc<Semaphore>,
    filesystem: Arc<Semaphore>,
    compute: Arc<Semaphore>,
}

impl Governor {
    #[must_use]
    pub fn new(limits: GovernorLimits) -> Self {
        Self {
            limits,
            programs: semaphore(limits.programs),
            nodes: semaphore(limits.nodes),
            processes: semaphore(limits.processes),
            filesystem: semaphore(limits.filesystem),
            compute: semaphore(limits.compute),
        }
    }

    #[must_use]
    pub const fn limits(&self) -> GovernorLimits {
        self.limits
    }

    pub async fn acquire(
        &self,
        kind: PermitKind,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<GovernorPermit, GovernorError> {
        let semaphore = self.semaphore(kind);
        let acquire = timeout_at(deadline, semaphore.acquire_owned());
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(GovernorError::Cancelled),
            result = acquire => match result {
                Ok(Ok(permit)) => Ok(GovernorPermit { kind, _permit: permit }),
                Ok(Err(_)) => Err(GovernorError::Closed),
                Err(_) => Err(GovernorError::Deadline),
            }
        }
    }

    pub fn try_acquire(&self, kind: PermitKind) -> Result<GovernorPermit, GovernorError> {
        match self.semaphore(kind).try_acquire_owned() {
            Ok(permit) => Ok(GovernorPermit {
                kind,
                _permit: permit,
            }),
            Err(TryAcquireError::NoPermits) => Err(GovernorError::NoPermits),
            Err(TryAcquireError::Closed) => Err(GovernorError::Closed),
        }
    }

    fn semaphore(&self, kind: PermitKind) -> Arc<Semaphore> {
        match kind {
            PermitKind::Program => Arc::clone(&self.programs),
            PermitKind::Node => Arc::clone(&self.nodes),
            PermitKind::Process => Arc::clone(&self.processes),
            PermitKind::Filesystem => Arc::clone(&self.filesystem),
            PermitKind::Compute => Arc::clone(&self.compute),
        }
    }
}

pub struct GovernorPermit {
    kind: PermitKind,
    _permit: OwnedSemaphorePermit,
}

impl GovernorPermit {
    #[must_use]
    pub const fn kind(&self) -> PermitKind {
        self.kind
    }
}

pub struct HierarchicalGovernor {
    global: Arc<Governor>,
    session: Governor,
}

impl HierarchicalGovernor {
    #[must_use]
    pub fn new(global: Arc<Governor>, session_limits: GovernorLimits) -> Self {
        Self {
            global,
            session: Governor::new(session_limits),
        }
    }

    pub async fn acquire(
        &self,
        kind: PermitKind,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<HierarchicalPermit, GovernorError> {
        let session = self.session.acquire(kind, deadline, cancellation).await?;
        let global = self.global.acquire(kind, deadline, cancellation).await?;
        Ok(HierarchicalPermit { global, session })
    }

    #[must_use]
    pub const fn session_limits(&self) -> GovernorLimits {
        self.session.limits()
    }
}

pub struct HierarchicalPermit {
    global: GovernorPermit,
    session: GovernorPermit,
}

impl HierarchicalPermit {
    #[must_use]
    pub const fn kind(&self) -> PermitKind {
        debug_assert!(matches_same_kind(self.global.kind(), self.session.kind()));
        self.global.kind()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum GovernorError {
    #[error("resource acquisition was cancelled")]
    Cancelled,
    #[error("resource acquisition exceeded the program deadline")]
    Deadline,
    #[error("resource governor is closed")]
    Closed,
    #[error("resource governor has no immediately available permit")]
    NoPermits,
}

fn semaphore(limit: NonZeroUsize) -> Arc<Semaphore> {
    Arc::new(Semaphore::new(limit.get()))
}

const fn non_zero(value: usize) -> NonZeroUsize {
    let value = if value == 0 { 1 } else { value };
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => unreachable!(),
    }
}

const fn matches_same_kind(left: PermitKind, right: PermitKind) -> bool {
    matches!(
        (left, right),
        (PermitKind::Program, PermitKind::Program)
            | (PermitKind::Node, PermitKind::Node)
            | (PermitKind::Process, PermitKind::Process)
            | (PermitKind::Filesystem, PermitKind::Filesystem)
            | (PermitKind::Compute, PermitKind::Compute)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::time::{Duration, Instant};

    use super::{Governor, GovernorError, GovernorLimits, PermitKind};
    use crate::{CancellationToken, Parallelism};

    #[tokio::test]
    async fn permits_are_bounded_and_released_by_raii() {
        let governor = Governor::new(GovernorLimits::from_parallelism(
            Parallelism::for_available_cpus(1),
        ));
        let first = governor
            .try_acquire(PermitKind::Process)
            .expect("first permit");
        assert_eq!(
            governor.try_acquire(PermitKind::Process).err(),
            Some(GovernorError::NoPermits)
        );
        drop(first);
        assert!(governor.try_acquire(PermitKind::Process).is_ok());

        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert_eq!(
            governor
                .acquire(
                    PermitKind::Filesystem,
                    Instant::now() + Duration::from_secs(1),
                    &cancelled,
                )
                .await
                .err(),
            Some(GovernorError::Cancelled)
        );
    }

    #[tokio::test]
    async fn hierarchical_acquisition_holds_both_levels() {
        let limits = GovernorLimits::from_parallelism(Parallelism::for_available_cpus(1));
        let global = Arc::new(Governor::new(limits));
        let hierarchy = super::HierarchicalGovernor::new(Arc::clone(&global), limits);
        let cancellation = CancellationToken::default();
        let permit = hierarchy
            .acquire(
                PermitKind::Process,
                Instant::now() + Duration::from_secs(1),
                &cancellation,
            )
            .await
            .expect("permit");
        assert_eq!(permit.kind(), PermitKind::Process);
        assert_eq!(
            global.try_acquire(PermitKind::Process).err(),
            Some(GovernorError::NoPermits)
        );
    }
}
