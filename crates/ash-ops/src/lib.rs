#![forbid(unsafe_code)]

//! Portable operation semantics for ash.

mod error;
mod exec;
mod list;
mod patch;
mod projection;
mod read;
mod reference;
mod search;
mod snapshot;

use ash_engine::Program;
use ash_platform::Workspace;
use ash_protocol::Operation;
use ash_protocol::request::{Arguments, Request};
use ash_protocol::response::FinalResponse;

pub use error::OperationError;

/// Portable M1 operations bound to one native workspace capability.
#[derive(Clone, Debug)]
pub struct PortableOperations {
    workspace: Workspace,
}

#[cfg(test)]
mod tests;

impl PortableOperations {
    #[must_use]
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }

    #[must_use]
    pub const fn operation_mask() -> u64 {
        Operation::Exec.mask()
            | Operation::Read.mask()
            | Operation::List.mask()
            | Operation::Search.mask()
            | Operation::Patch.mask()
            | Operation::Ref.mask()
            | Operation::Snapshot.mask()
    }

    pub async fn execute(
        &self,
        request: &Request,
        program: &Program,
    ) -> Result<FinalResponse, OperationError> {
        let result = match request.arguments() {
            Arguments::Exec(arguments) => {
                exec::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Read(arguments) => {
                read::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::List(arguments) => {
                list::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Search(arguments) => {
                search::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Patch(arguments) => {
                patch::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Snapshot(arguments) => {
                snapshot::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Ref(arguments) => reference::execute(request, arguments, program).await,
            Arguments::Cancel(_) => Err(OperationError::Unsupported),
        };
        match result {
            Ok(response) => Ok(response),
            Err(error) => error.into_response(request.id()),
        }
    }
}
