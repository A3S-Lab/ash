use ash_engine::{Engine, Parallelism, Session, SessionConfig};
use ash_ops::{OperationError, PortableOperations};
use ash_platform::Workspace;
use ash_protocol::request::Request;
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, RetryClass, Status,
};

use crate::cli_error::CliError;

pub struct ExecutionSession {
    session: Session,
    operations: PortableOperations,
}

impl ExecutionSession {
    pub fn open(
        session_id: u64,
        workspace: &str,
        max_output_bytes: u64,
        parallelism: Parallelism,
    ) -> Result<Self, CliError> {
        let engine = Engine::new(parallelism)?;
        let session = engine.open_session(SessionConfig::new(
            session_id,
            workspace,
            max_output_bytes,
            parallelism,
        ))?;
        let operations = PortableOperations::new(Workspace::new(workspace)?);
        Ok(Self {
            session,
            operations,
        })
    }

    pub async fn execute(&self, request: &Request) -> Result<FinalResponse, CliError> {
        let program = match self.session.begin(request).await {
            Ok(program) => program,
            Err(error) => {
                return Ok(OperationError::from(error).into_response(request.id())?);
            }
        };
        Ok(self.operations.execute(request, &program).await?)
    }

    pub fn close(&self) -> Result<(), CliError> {
        Ok(self.session.close()?)
    }
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
