use ash_engine::{Engine, Parallelism, Session, SessionConfig};
use ash_ops::{AuthorizationPolicy, OperationError, PortableOperations};
use ash_platform::Workspace;
use ash_protocol::Operation;
use ash_protocol::request::{Arguments, CancelArgs, Request};
use ash_protocol::response::{
    CancelResult, CancellationState, ErrorCode, ErrorRecord, ErrorStage, FinalResponse, ResultData,
    RetryClass, Status,
};
use tokio::sync::oneshot;

use crate::cli_error::CliError;

pub struct ExecutionSession {
    session: Session,
    operations: PortableOperations,
}

impl ExecutionSession {
    #[must_use]
    pub const fn operation_mask() -> u64 {
        PortableOperations::operation_mask() | Operation::Cancel.mask()
    }

    #[must_use]
    pub const fn capability_mask() -> u64 {
        PortableOperations::capability_mask()
    }

    pub fn open(
        session_id: u64,
        workspace: &str,
        max_output_bytes: u64,
        parallelism: Parallelism,
        capability_mask: u64,
    ) -> Result<Self, CliError> {
        let engine = Engine::new(parallelism)?;
        let session = engine.open_session(SessionConfig::new(
            session_id,
            workspace,
            max_output_bytes,
            parallelism,
        ))?;
        let operations = PortableOperations::with_authorization(
            Workspace::new(workspace)?,
            AuthorizationPolicy::allow(capability_mask).map_err(OperationError::from)?,
        );
        Ok(Self {
            session,
            operations,
        })
    }

    pub async fn execute(&self, request: &Request) -> Result<FinalResponse, CliError> {
        if let Arguments::Cancel(arguments) = request.arguments() {
            return self.cancel(request, *arguments);
        }
        let program = match self.session.begin(request).await {
            Ok(program) => program,
            Err(error) => {
                return engine_error_response(request.id(), error);
            }
        };
        Ok(self.operations.execute(request, &program).await?)
    }

    /// Registers before notifying the transport, then waits for execution
    /// capacity. This makes queued and running requests visible to `cancel`.
    pub async fn execute_registered(
        &self,
        request: Request,
        registered: oneshot::Sender<()>,
    ) -> Result<FinalResponse, CliError> {
        let registration = match self.session.register(&request) {
            Ok(registration) => registration,
            Err(error) => {
                let _ = registered.send(());
                return engine_error_response(request.id(), error);
            }
        };
        let _ = registered.send(());
        let program = match registration.start().await {
            Ok(program) => program,
            Err(error) => return engine_error_response(request.id(), error),
        };
        Ok(self.operations.execute(&request, &program).await?)
    }

    pub fn cancel(
        &self,
        request: &Request,
        arguments: CancelArgs,
    ) -> Result<FinalResponse, CliError> {
        let _registration = match self.session.register(request) {
            Ok(registration) => registration,
            Err(error) => return engine_error_response(request.id(), error),
        };
        let state = if self.session.cancel(arguments.target_id())? {
            CancellationState::Signaled
        } else {
            CancellationState::NotActive
        };
        Ok(FinalResponse::success(
            request.id(),
            vec![],
            ResultData::Cancel(CancelResult {
                target_id: arguments.target_id(),
                state,
            }),
            0,
            None,
        )?)
    }

    pub fn close(&self) -> Result<(), CliError> {
        Ok(self.session.close()?)
    }
}

fn engine_error_response(
    request_id: u64,
    error: ash_engine::EngineError,
) -> Result<FinalResponse, CliError> {
    Ok(OperationError::from(error).into_response(request_id)?)
}

pub fn invalid_request(request_id: u64) -> Result<FinalResponse, CliError> {
    Ok(FinalResponse::failure(
        request_id,
        Status::InvalidRequest,
        ErrorRecord {
            code: ErrorCode::InvalidArgument,
            retry: RetryClass::CorrectRequest,
            stage: ErrorStage::Validate,
            evidence: None,
            argument: None,
        },
        vec![],
        None,
        0,
        None,
    )?)
}

pub fn capacity_exceeded(request_id: u64) -> Result<FinalResponse, CliError> {
    Ok(FinalResponse::failure(
        request_id,
        Status::BudgetExceeded,
        ErrorRecord {
            code: ErrorCode::ConcurrencyBudget,
            retry: RetryClass::RetrySame,
            stage: ErrorStage::Execute,
            evidence: None,
            argument: None,
        },
        vec![],
        None,
        0,
        None,
    )?)
}
