use ash_engine::{PermitKind, Program};
use ash_platform::{
    FileAction, FileActionOutcome, FileActionState, FileTransactionFailure, FileTransactionLimits,
    MAX_FILE_TRANSACTION_FILE_BYTES, MAX_FILE_TRANSACTION_TOTAL_BYTES, TransactionControl,
    Workspace,
};
use ash_protocol::request::{FsAction, FsActionKind, FsArgs, PatchContent, Request};
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, FsResult, FsState, RESULT_PARTIAL,
    ResultData, RetryClass, Status,
};

use crate::OperationError;
use crate::projection::{charge, intern_paths, presentation_limit, temporary_paths};

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &FsArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_control(program)?;
    let actions = resolve_actions(arguments, program).await?;
    workspace.validate_file_actions(&actions)?;
    let limits = FileTransactionLimits::new(
        MAX_FILE_TRANSACTION_FILE_BYTES,
        MAX_FILE_TRANSACTION_TOTAL_BYTES,
    )?;
    let (ids, mappings) = reserve_response(request.id(), arguments.actions(), program)?;

    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let workspace = workspace.clone();
    let cancellation = program.cancellation().clone();
    let budget = program.budget().clone();
    let outcome = program
        .compute_pool()
        .run(move || {
            workspace.file_transaction(actions, limits, || {
                if cancellation.is_cancelled() {
                    TransactionControl::Cancelled
                } else if budget.check_deadline().is_err() {
                    TransactionControl::TimedOut
                } else {
                    TransactionControl::Continue
                }
            })
        })
        .await??;

    make_response(
        request.id(),
        mappings,
        &ids,
        arguments.actions(),
        &outcome.actions,
        outcome.failure,
    )
}

async fn resolve_actions(
    arguments: &FsArgs,
    program: &Program,
) -> Result<Vec<FileAction>, OperationError> {
    let mut total_content = 0_u64;
    let mut resolved = Vec::with_capacity(arguments.actions().len());
    for action in arguments.actions() {
        let file_action = match action.kind() {
            FsActionKind::Create => {
                let bytes = match action.content().ok_or(OperationError::InvalidArgument)? {
                    PatchContent::Inline(value) => value.as_bytes().to_vec(),
                    PatchContent::Reference(reference) => {
                        let retained = program.store().get(*reference)?;
                        if retained.len() > MAX_FILE_TRANSACTION_FILE_BYTES {
                            return Err(OperationError::WorkLimit);
                        }
                        retained
                            .read_all(MAX_FILE_TRANSACTION_FILE_BYTES)
                            .await?
                            .to_vec()
                    }
                };
                total_content = total_content
                    .checked_add(bytes.len() as u64)
                    .ok_or(OperationError::WorkLimit)?;
                if bytes.len() as u64 > MAX_FILE_TRANSACTION_FILE_BYTES
                    || total_content > MAX_FILE_TRANSACTION_TOTAL_BYTES
                {
                    return Err(OperationError::WorkLimit);
                }
                FileAction::create(action.path(), bytes)
            }
            FsActionKind::Copy => FileAction::copy(
                action.path(),
                action
                    .destination()
                    .ok_or(OperationError::InvalidArgument)?,
                expected_digest(action)?,
            ),
            FsActionKind::Move => FileAction::move_file(
                action.path(),
                action
                    .destination()
                    .ok_or(OperationError::InvalidArgument)?,
                expected_digest(action)?,
            ),
            FsActionKind::Remove => FileAction::remove(action.path(), expected_digest(action)?),
        };
        resolved.push(file_action);
    }
    Ok(resolved)
}

fn expected_digest(action: &FsAction) -> Result<[u8; 32], OperationError> {
    parse_digest(
        action
            .expected_digest()
            .ok_or(OperationError::InvalidArgument)?,
    )
    .ok_or(OperationError::InvalidArgument)
}

fn reserve_response(
    request_id: u64,
    actions: &[FsAction],
    program: &Program,
) -> Result<(Vec<u64>, Vec<ash_protocol::response::PathMapping>), OperationError> {
    let paths = response_paths(actions);
    let (temporary_ids, temporary_mappings) = temporary_paths(&paths);
    let worst = vec![
        FileActionOutcome {
            state: FileActionState::RecoveryRequired,
            digest: Some([0xff; 32]),
        };
        actions.len()
    ];
    let response = make_response(
        request_id,
        temporary_mappings,
        &temporary_ids,
        actions,
        &worst,
        Some(FileTransactionFailure::RecoveryRequired),
    )?;
    if response.encode()?.encode().len() > presentation_limit(program) {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &response, actions.len())?;
    intern_paths(program, &paths)
}

fn response_paths(actions: &[FsAction]) -> Vec<String> {
    let mut paths = Vec::with_capacity(actions.len().saturating_mul(2));
    for action in actions {
        paths.push(action.path().to_owned());
        if let Some(destination) = action.destination() {
            paths.push(destination.to_owned());
        }
    }
    paths
}

fn make_response(
    request_id: u64,
    mappings: Vec<ash_protocol::response::PathMapping>,
    path_ids: &[u64],
    actions: &[FsAction],
    outcomes: &[FileActionOutcome],
    failure: Option<FileTransactionFailure>,
) -> Result<FinalResponse, OperationError> {
    if actions.len() != outcomes.len() {
        return Err(OperationError::InvalidArgument);
    }
    let mut ids = path_ids.iter().copied();
    let rows = actions
        .iter()
        .zip(outcomes)
        .map(|(action, outcome)| {
            let path = ids.next().ok_or(OperationError::InvalidArgument)?;
            let destination = action
                .destination()
                .map(|_| ids.next().ok_or(OperationError::InvalidArgument))
                .transpose()?;
            Ok(FsResult {
                id: action.id(),
                kind: action.kind(),
                path,
                destination,
                state: protocol_state(outcome.state),
                digest: outcome.digest.map(hex_digest),
            })
        })
        .collect::<Result<Vec<_>, OperationError>>()?;
    if ids.next().is_some() {
        return Err(OperationError::InvalidArgument);
    }
    let data = ResultData::Fs(rows);
    if let Some(failure) = failure {
        let (status, error) = failure_error(failure);
        Ok(FinalResponse::failure(
            request_id,
            status,
            error,
            mappings,
            Some(data),
            if failure == FileTransactionFailure::RecoveryRequired {
                RESULT_PARTIAL
            } else {
                0
            },
            None,
        )?)
    } else {
        Ok(FinalResponse::success(request_id, mappings, data, 0, None)?)
    }
}

const fn protocol_state(state: FileActionState) -> FsState {
    match state {
        FileActionState::Committed => FsState::Committed,
        FileActionState::Conflict => FsState::Conflict,
        FileActionState::RolledBack => FsState::RolledBack,
        FileActionState::RecoveryRequired => FsState::RecoveryRequired,
        FileActionState::Skipped => FsState::Skipped,
    }
}

const fn failure_error(failure: FileTransactionFailure) -> (Status, ErrorRecord) {
    match failure {
        FileTransactionFailure::Conflict => (
            Status::Conflict,
            ErrorRecord {
                code: ErrorCode::ContentConflict,
                retry: RetryClass::CorrectRequest,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
        ),
        FileTransactionFailure::Cancelled => (
            Status::Cancelled,
            ErrorRecord {
                code: ErrorCode::ProcessCancelled,
                retry: RetryClass::Never,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
        ),
        FileTransactionFailure::TimedOut => (
            Status::TimedOut,
            ErrorRecord {
                code: ErrorCode::ProcessTimedOut,
                retry: RetryClass::RetrySame,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
        ),
        FileTransactionFailure::Filesystem => (
            Status::Failed,
            ErrorRecord {
                code: ErrorCode::Filesystem,
                retry: RetryClass::RetrySame,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
        ),
        FileTransactionFailure::RecoveryRequired => (
            Status::Failed,
            ErrorRecord {
                code: ErrorCode::RecoveryRequired,
                retry: RetryClass::Approval,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
        ),
    }
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = hex_nibble(pair[0])?.checked_mul(16)? + hex_nibble(pair[1])?;
    }
    Some(digest)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn hex_digest(digest: [u8; 32]) -> String {
    blake3::Hash::from_bytes(digest).to_hex().to_string()
}

fn check_control(program: &Program) -> Result<(), OperationError> {
    if program.cancellation().is_cancelled() {
        Err(OperationError::Cancelled)
    } else {
        program.budget().check_deadline()?;
        Ok(())
    }
}
