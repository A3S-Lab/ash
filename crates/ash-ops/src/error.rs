use std::io::{self, ErrorKind};

use ash_engine::{BudgetError, EngineError, GovernorError, ParallelismError};
use ash_platform::PlatformError;
use ash_protocol::ason::BuildError;
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, ResponseError, RetryClass, Status,
};
use ash_store::{PathDictionaryError, StoreError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OperationError {
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
    #[error(transparent)]
    Parallelism(#[from] ParallelismError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Paths(#[from] PathDictionaryError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    Response(#[from] ResponseError),
    #[error(transparent)]
    Regex(#[from] regex::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
    #[error("operation is not available in this implementation stage")]
    Unsupported,
    #[error("operation was cancelled")]
    Cancelled,
    #[error("operation input exceeds its bounded work ceiling")]
    WorkLimit,
    #[error("immediate response cannot fit the negotiated output budget")]
    OutputBudget,
}

impl OperationError {
    pub fn into_response(self, request_id: u64) -> Result<FinalResponse, Self> {
        let (status, code, retry, stage) = self.classification();
        FinalResponse::failure(
            request_id,
            status,
            ErrorRecord {
                code,
                retry,
                stage,
                evidence: None,
                argument: None,
            },
            vec![],
            None,
            0,
            None,
        )
        .map_err(Self::Response)
    }

    fn classification(&self) -> (Status, ErrorCode, RetryClass, ErrorStage) {
        match self {
            Self::Unsupported => (
                Status::Unsupported,
                ErrorCode::UnsupportedOperation,
                RetryClass::CorrectRequest,
                ErrorStage::Validate,
            ),
            Self::Cancelled | Self::Engine(EngineError::Governor(GovernorError::Cancelled)) => (
                Status::Cancelled,
                ErrorCode::ProcessCancelled,
                RetryClass::Never,
                ErrorStage::Execute,
            ),
            Self::Engine(EngineError::Governor(GovernorError::Deadline))
            | Self::Engine(EngineError::Budget(BudgetError::Deadline))
            | Self::Budget(BudgetError::Deadline) => (
                Status::TimedOut,
                ErrorCode::ProcessTimedOut,
                RetryClass::RetrySame,
                ErrorStage::Execute,
            ),
            Self::Platform(PlatformError::InvalidLogicalPath) | Self::Regex(_) => (
                Status::InvalidRequest,
                ErrorCode::InvalidArgument,
                RetryClass::CorrectRequest,
                ErrorStage::Validate,
            ),
            Self::Platform(PlatformError::WorkspaceEscape) => (
                Status::Denied,
                ErrorCode::WorkspaceEscape,
                RetryClass::Never,
                ErrorStage::Authorize,
            ),
            Self::Platform(PlatformError::Io(error)) if error.kind() == ErrorKind::NotFound => (
                Status::NotFound,
                ErrorCode::PathNotFound,
                RetryClass::CorrectRequest,
                ErrorStage::Resolve,
            ),
            Self::Platform(PlatformError::InputTooLarge { .. }) | Self::WorkLimit => (
                Status::BudgetExceeded,
                ErrorCode::StorageBudget,
                RetryClass::CorrectRequest,
                ErrorStage::Execute,
            ),
            Self::OutputBudget
            | Self::Engine(EngineError::Budget(BudgetError::Output { .. }))
            | Self::Engine(EngineError::Budget(BudgetError::Records { .. }))
            | Self::Budget(BudgetError::Output { .. } | BudgetError::Records { .. }) => (
                Status::BudgetExceeded,
                ErrorCode::OutputBudget,
                RetryClass::CorrectRequest,
                ErrorStage::Reduce,
            ),
            Self::Store(StoreError::ByteQuota { .. } | StoreError::EntryQuota { .. }) => (
                Status::BudgetExceeded,
                ErrorCode::StorageBudget,
                RetryClass::CorrectRequest,
                ErrorStage::Retain,
            ),
            Self::Platform(_) => (
                Status::Failed,
                ErrorCode::Filesystem,
                RetryClass::RetrySame,
                ErrorStage::Execute,
            ),
            Self::Engine(EngineError::Parallelism(ParallelismError::WorkerLost))
            | Self::Parallelism(ParallelismError::WorkerLost) => (
                Status::Internal,
                ErrorCode::Internal,
                RetryClass::RetrySame,
                ErrorStage::Execute,
            ),
            Self::Engine(_)
            | Self::Budget(_)
            | Self::Parallelism(_)
            | Self::Store(_)
            | Self::Paths(_)
            | Self::Build(_)
            | Self::Response(_) => (
                Status::Internal,
                ErrorCode::Internal,
                RetryClass::RetrySame,
                ErrorStage::Encode,
            ),
            Self::Io(_) | Self::Task(_) => (
                Status::Internal,
                ErrorCode::Internal,
                RetryClass::RetrySame,
                ErrorStage::Execute,
            ),
        }
    }
}
