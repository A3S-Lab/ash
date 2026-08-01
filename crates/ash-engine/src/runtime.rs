use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ash_protocol::request::Request;
use ash_store::{PathDictionary, PathDictionaryError, ResultStore, StoreError, StoreLimits};
use thiserror::Error;

use crate::{
    BudgetError, BudgetTracker, CancellationToken, ComputePool, Governor, GovernorError,
    GovernorLimits, HierarchicalGovernor, HierarchicalPermit, Parallelism, ParallelismError,
    PermitKind,
};

pub struct Engine {
    parallelism: Parallelism,
    compute: Arc<ComputePool>,
    governor: Arc<Governor>,
}

impl Engine {
    pub fn new(parallelism: Parallelism) -> Result<Self, EngineError> {
        let compute = Arc::new(ComputePool::new(parallelism)?);
        let governor = Arc::new(Governor::new(GovernorLimits::from_parallelism(parallelism)));
        Ok(Self {
            parallelism,
            compute,
            governor,
        })
    }

    pub fn open_session(&self, config: SessionConfig) -> Result<Session, EngineError> {
        config.validate()?;
        let store = Arc::new(ResultStore::new(config.store_limits)?);
        let paths = Arc::new(PathDictionary::new(config.max_paths)?);
        let governor = HierarchicalGovernor::new(Arc::clone(&self.governor), config.governor);
        Ok(Session {
            inner: Arc::new(SessionInner {
                id: config.id,
                workspace: config.workspace,
                max_output_bytes: config.max_output_bytes,
                max_paths: config.max_paths,
                compute: Arc::clone(&self.compute),
                governor,
                store,
                paths,
                closed: AtomicBool::new(false),
                active: Mutex::new(HashMap::new()),
            }),
        })
    }

    #[must_use]
    pub const fn parallelism(&self) -> Parallelism {
        self.parallelism
    }
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub id: u64,
    pub workspace: String,
    pub max_output_bytes: u64,
    pub store_limits: StoreLimits,
    pub max_paths: usize,
    pub governor: GovernorLimits,
}

impl SessionConfig {
    #[must_use]
    pub fn new(
        id: u64,
        workspace: impl Into<String>,
        max_output_bytes: u64,
        parallelism: Parallelism,
    ) -> Self {
        Self {
            id,
            workspace: workspace.into(),
            max_output_bytes,
            store_limits: StoreLimits::default(),
            max_paths: 65_536,
            governor: GovernorLimits::from_parallelism(parallelism),
        }
    }

    fn validate(&self) -> Result<(), EngineError> {
        if self.id == 0
            || self.workspace.is_empty()
            || self.workspace.len() > 4096
            || self.workspace.contains('\0')
            || self.max_output_bytes == 0
            || self.max_paths == 0
        {
            Err(EngineError::InvalidSession)
        } else {
            Ok(())
        }
    }
}

pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    id: u64,
    workspace: String,
    max_output_bytes: u64,
    max_paths: usize,
    compute: Arc<ComputePool>,
    governor: HierarchicalGovernor,
    store: Arc<ResultStore>,
    paths: Arc<PathDictionary>,
    closed: AtomicBool,
    active: Mutex<HashMap<u64, CancellationToken>>,
}

impl Session {
    pub async fn begin(&self, request: &Request) -> Result<Program, EngineError> {
        self.register(request)?.start().await
    }

    /// Registers a request synchronously before it is scheduled.
    ///
    /// The split registration/start lifecycle lets the transport keep reading
    /// control frames while a request waits for a program permit. A cancel
    /// request can therefore target queued work as well as running work.
    pub fn register(&self, request: &Request) -> Result<RegisteredProgram, EngineError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(EngineError::SessionClosed);
        }
        let cancellation = CancellationToken::default();
        let budget = Arc::new(BudgetTracker::new(
            request.budget(),
            self.inner.max_output_bytes,
        )?);
        {
            let mut active = self.inner.lock_active()?;
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(EngineError::SessionClosed);
            }
            if active.contains_key(&request.id()) {
                return Err(EngineError::DuplicateRequest(request.id()));
            }
            active.insert(request.id(), cancellation.clone());
        }
        Ok(RegisteredProgram {
            session: Arc::clone(&self.inner),
            request_id: request.id(),
            cancellation,
            budget,
            registered: true,
        })
    }

    pub fn cancel(&self, request_id: u64) -> Result<bool, EngineError> {
        let active = self.inner.lock_active()?;
        Ok(active
            .get(&request_id)
            .is_some_and(CancellationToken::cancel))
    }

    pub fn close(&self) -> Result<(), EngineError> {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            for cancellation in self.inner.lock_active()?.values() {
                cancellation.cancel();
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn id(&self) -> u64 {
        self.inner.id
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.inner.workspace
    }

    #[must_use]
    pub fn store(&self) -> &Arc<ResultStore> {
        &self.inner.store
    }

    #[must_use]
    pub fn paths(&self) -> &Arc<PathDictionary> {
        &self.inner.paths
    }
}

pub struct RegisteredProgram {
    session: Arc<SessionInner>,
    request_id: u64,
    cancellation: CancellationToken,
    budget: Arc<BudgetTracker>,
    registered: bool,
}

impl RegisteredProgram {
    pub async fn start(mut self) -> Result<Program, EngineError> {
        let permit = self
            .session
            .governor
            .acquire(
                PermitKind::Program,
                self.budget.deadline(),
                &self.cancellation,
            )
            .await?;
        let program = Program {
            lease: Arc::new(ProgramLease {
                session: Arc::clone(&self.session),
                root_request_id: self.request_id,
                _program_permit: permit,
            }),
            request_id: self.request_id,
            cancellation: self.cancellation.clone(),
            budget: Arc::clone(&self.budget),
            paths: Arc::clone(&self.session.paths),
        };
        self.registered = false;
        Ok(program)
    }
}

impl Drop for RegisteredProgram {
    fn drop(&mut self) {
        if self.registered {
            self.session.unregister(self.request_id);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub struct Program {
    lease: Arc<ProgramLease>,
    request_id: u64,
    cancellation: CancellationToken,
    budget: Arc<BudgetTracker>,
    paths: Arc<PathDictionary>,
}

struct ProgramLease {
    session: Arc<SessionInner>,
    root_request_id: u64,
    _program_permit: HierarchicalPermit,
}

impl Program {
    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.lease.session.workspace
    }

    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    #[must_use]
    pub fn budget(&self) -> &Arc<BudgetTracker> {
        &self.budget
    }

    #[must_use]
    pub fn compute_pool(&self) -> &Arc<ComputePool> {
        &self.lease.session.compute
    }

    #[must_use]
    pub fn store(&self) -> &Arc<ResultStore> {
        &self.lease.session.store
    }

    #[must_use]
    pub fn paths(&self) -> &Arc<PathDictionary> {
        &self.paths
    }

    /// Creates a batch-node program with isolated counters and the parent's
    /// cancellation, session resources, deadline, and program permit lease.
    pub fn child(
        &self,
        request_id: u64,
        budget: ash_protocol::request::Budget,
        output_limit: u64,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            lease: Arc::clone(&self.lease),
            request_id,
            cancellation: self.cancellation.clone(),
            budget: Arc::new(BudgetTracker::new_with_deadline(
                budget,
                output_limit,
                self.budget.deadline(),
            )?),
            // A child response is retained as a self-contained document. A
            // node-local dictionary makes its path IDs deterministic even
            // when sibling nodes discover paths concurrently.
            paths: Arc::new(PathDictionary::new(self.lease.session.max_paths)?),
        })
    }

    pub async fn acquire(&self, kind: PermitKind) -> Result<HierarchicalPermit, EngineError> {
        Ok(self
            .lease
            .session
            .governor
            .acquire(kind, self.budget.deadline(), &self.cancellation)
            .await?)
    }
}

impl Drop for ProgramLease {
    fn drop(&mut self) {
        self.session.unregister(self.root_request_id);
    }
}

impl SessionInner {
    fn lock_active(&self) -> Result<MutexGuard<'_, HashMap<u64, CancellationToken>>, EngineError> {
        self.active.lock().map_err(|_| EngineError::Poisoned)
    }

    fn unregister(&self, request_id: u64) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&request_id);
        }
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Parallelism(#[from] ParallelismError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Paths(#[from] PathDictionaryError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
    #[error(transparent)]
    Governor(#[from] GovernorError),
    #[error("session configuration is invalid")]
    InvalidSession,
    #[error("session is closed")]
    SessionClosed,
    #[error("request identifier {0} is already active")]
    DuplicateRequest(u64),
    #[error("session state lock was poisoned")]
    Poisoned,
}

#[cfg(test)]
mod tests {
    use ash_protocol::request::{Arguments, Budget, Request, SearchArgs};

    use super::{Engine, EngineError, SessionConfig};
    use crate::{GovernorError, Parallelism};

    fn request(id: u64) -> Request {
        Request::new(
            id,
            Arguments::Search(SearchArgs::new("needle", vec![".".to_owned()], 0).expect("search")),
            Budget::new(100, 10, 30_000).expect("budget"),
        )
        .expect("request")
    }

    #[tokio::test]
    async fn session_rejects_duplicate_active_ids_and_releases_on_drop() {
        let parallelism = Parallelism::for_available_cpus(2);
        let engine = Engine::new(parallelism).expect("engine");
        let session = engine
            .open_session(SessionConfig::new(1, ".", 4096, parallelism))
            .expect("session");
        let first = session.begin(&request(7)).await.expect("program");
        assert!(matches!(
            session.begin(&request(7)).await,
            Err(EngineError::DuplicateRequest(7))
        ));
        let child = first
            .child(8, Budget::new(10, 2, 1_000).expect("child budget"), 32)
            .expect("child");
        drop(first);
        assert!(matches!(
            session.begin(&request(7)).await,
            Err(EngineError::DuplicateRequest(7))
        ));
        assert_eq!(child.request_id(), 8);
        assert_eq!(child.budget().remaining().output_bytes, 32);
        child
            .paths()
            .intern(&["child.txt".to_owned()])
            .expect("child path");
        assert!(session.paths().is_empty().expect("root dictionary"));
        drop(child);
        assert!(session.begin(&request(7)).await.is_ok());
    }

    #[tokio::test]
    async fn explicit_cancel_and_session_close_propagate_to_programs() {
        let parallelism = Parallelism::for_available_cpus(2);
        let engine = Engine::new(parallelism).expect("engine");
        let session = engine
            .open_session(SessionConfig::new(1, ".", 4096, parallelism))
            .expect("session");
        let first = session.begin(&request(1)).await.expect("program");
        assert!(session.cancel(1).expect("cancel"));
        first.cancellation().cancelled().await;

        let second = session.begin(&request(2)).await.expect("program");
        session.close().expect("close");
        second.cancellation().cancelled().await;
        assert!(matches!(
            session.begin(&request(3)).await,
            Err(EngineError::SessionClosed)
        ));
    }

    #[tokio::test]
    async fn registered_work_can_be_cancelled_before_it_starts() {
        let parallelism = Parallelism::for_available_cpus(2);
        let engine = Engine::new(parallelism).expect("engine");
        let session = engine
            .open_session(SessionConfig::new(1, ".", 4096, parallelism))
            .expect("session");
        let registered = session.register(&request(9)).expect("register");
        assert!(session.cancel(9).expect("cancel"));
        assert!(matches!(
            registered.start().await,
            Err(EngineError::Governor(GovernorError::Cancelled))
        ));
        assert!(!session.cancel(9).expect("registration released"));
    }
}
