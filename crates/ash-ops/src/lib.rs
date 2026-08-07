#![forbid(unsafe_code)]

//! Portable operation semantics for ash.

mod authorization;
mod batch;
mod error;
mod exec;
mod filesystem;
mod list;
mod patch;
mod projection;
mod read;
mod reducer;
mod reference;
mod search;
pub mod semantic;
mod snapshot;

use ash_engine::Program;
use ash_platform::Workspace;
use ash_protocol::Operation;
use ash_protocol::request::{Arguments, Request};
use ash_protocol::response::FinalResponse;

pub use authorization::{AuthorizationError, AuthorizationPolicy, PermitAuthority};
pub use error::OperationError;
pub use reducer::{
    ErrorFocusedReduction, RepeatedBlockReduction, RepeatedLineReduction, collapse_repeated_blocks,
    collapse_repeated_lines, focus_error_output,
};
pub use semantic::{
    ListQuery, MAX_READ_FILE_BYTES, NativeFileSystem, ReadQuery, SearchQuery, SemanticEntry,
    SemanticEntryKind, SemanticError, SemanticFileSystem, SemanticListFilter, SemanticListResult,
    SemanticPath, SemanticRead, SemanticReadMode, SemanticReadResult, SemanticSearchMatch,
    SemanticSearchPattern, SemanticSearchResult, SemanticServices, SemanticWalkOptions,
};

/// Portable operations bound to one native workspace capability.
#[derive(Clone, Debug)]
pub struct PortableOperations {
    workspace: Workspace,
    semantic_services: SemanticServices<Workspace>,
    authorization: AuthorizationPolicy,
}

#[cfg(test)]
mod tests;

impl PortableOperations {
    #[must_use]
    pub fn new(workspace: Workspace) -> Self {
        Self {
            semantic_services: SemanticServices::new(workspace.clone()),
            workspace,
            authorization: AuthorizationPolicy::default(),
        }
    }

    #[must_use]
    pub const fn capability_mask() -> u64 {
        ash_protocol::ALL_CAPABILITY_MASK
    }

    #[must_use]
    pub fn with_authorization(workspace: Workspace, authorization: AuthorizationPolicy) -> Self {
        Self {
            semantic_services: SemanticServices::new(workspace.clone()),
            workspace,
            authorization,
        }
    }

    /// Returns the raw semantic services used by the ASH/1 adapters.
    #[must_use]
    pub const fn semantic_services(&self) -> &SemanticServices<Workspace> {
        &self.semantic_services
    }

    #[must_use]
    pub const fn operation_mask() -> u64 {
        Operation::Exec.mask()
            | Operation::Read.mask()
            | Operation::List.mask()
            | Operation::Search.mask()
            | Operation::Patch.mask()
            | Operation::Fs.mask()
            | Operation::Batch.mask()
            | Operation::RefBytes.mask()
            | Operation::Snapshot.mask()
    }

    pub async fn execute(
        &self,
        request: &Request,
        program: &Program,
    ) -> Result<FinalResponse, OperationError> {
        match authorization::authorize(&self.authorization, request, program) {
            Ok(Some(response)) => return Ok(response),
            Ok(None) => {}
            Err(error) => return error.into_response(request.id()),
        }
        let result = match request.arguments() {
            Arguments::Batch(arguments) => batch::execute(self, request, arguments, program).await,
            _ => self.execute_leaf(request, program).await,
        };
        match result {
            Ok(response) => Ok(response),
            Err(error) => error.into_response(request.id()),
        }
    }

    async fn execute_leaf(
        &self,
        request: &Request,
        program: &Program,
    ) -> Result<FinalResponse, OperationError> {
        match request.arguments() {
            Arguments::Exec(arguments) => {
                exec::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Read(arguments) => {
                read::execute(&self.semantic_services, request, arguments, program).await
            }
            Arguments::List(arguments) => {
                list::execute(&self.semantic_services, request, arguments, program).await
            }
            Arguments::Search(arguments) => {
                search::execute(&self.semantic_services, request, arguments, program).await
            }
            Arguments::Patch(arguments) => {
                patch::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Fs(arguments) => {
                filesystem::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Snapshot(arguments) => {
                snapshot::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Ref(arguments) => {
                reference::execute(&self.workspace, request, arguments, program).await
            }
            Arguments::Batch(_) | Arguments::Cancel(_) => Err(OperationError::Unsupported),
        }
    }

    async fn execute_leaf_response(
        &self,
        request: &Request,
        program: &Program,
    ) -> Result<FinalResponse, OperationError> {
        match self.execute_leaf(request, program).await {
            Ok(response) => Ok(response),
            Err(error) => error.into_response(request.id()),
        }
    }
}
