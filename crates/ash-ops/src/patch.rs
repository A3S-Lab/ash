use std::sync::Arc;

use ash_engine::{PermitKind, Program};
use ash_platform::{
    FileAction, FileActionState, FileTransactionFailure, FileTransactionLimits, PlatformError,
    TransactionControl, Workspace,
};
use ash_protocol::request::{PatchArgs, PatchContent, Request};
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, PatchResult, PatchState, RESULT_PARTIAL,
    ResultData, RetryClass, Status,
};

use crate::OperationError;
use crate::projection::{charge, intern_paths, presentation_limit, temporary_paths};

const MAX_PATCH_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PATCH_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &PatchArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    if arguments.paths().len() > program.budget().remaining().records as usize {
        return Err(OperationError::OutputBudget);
    }

    let edits = resolve_edits(arguments, program)?;
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let sizes = mutation_sizes(workspace, arguments.paths(), program).await?;
    let tasks = build_tasks(arguments, edits, sizes)?;
    let workspace_for_prepare = workspace.clone();
    let cancellation = program.cancellation().clone();
    let prepared = program
        .compute_pool()
        .map_ordered_owned(tasks, move |task| {
            prepare_file(&workspace_for_prepare, task, &cancellation)
        })
        .await?;

    let mut files = Vec::with_capacity(prepared.len());
    let mut conflict_rows = Vec::with_capacity(prepared.len());
    let mut has_conflict = false;
    for result in prepared {
        match result? {
            Preparation::Ready(file) => {
                conflict_rows.push(Row::new(PatchState::Skipped, None));
                files.push(Some(file));
            }
            Preparation::Conflict { actual_digest } => {
                has_conflict = true;
                conflict_rows.push(Row::new(PatchState::Conflict, Some(actual_digest)));
                files.push(None);
            }
        }
    }
    check_cancelled(program)?;

    if has_conflict {
        return emit_without_mutation(
            request.id(),
            arguments.paths(),
            conflict_rows,
            Failure::Conflict,
            program,
        );
    }
    let files = files
        .into_iter()
        .map(|file| file.ok_or(OperationError::InvalidArgument))
        .collect::<Result<Vec<_>, _>>()?;

    let (ids, mappings) = reserve_transaction_response(request.id(), arguments.paths(), program)?;
    let outcome = run_transaction(workspace, &files, program);
    make_response(request.id(), mappings, &ids, &outcome.rows, outcome.failure)
}

#[derive(Clone)]
struct ResolvedEdit {
    offset: u64,
    delete_length: u64,
    replacement: Arc<[u8]>,
}

struct PatchTask {
    path: String,
    expected_digest: [u8; 32],
    observed_size: u64,
    result_size: usize,
    edits: Vec<ResolvedEdit>,
}

struct PreparedFile {
    path: String,
    expected_digest: [u8; 32],
    replacement: Vec<u8>,
}

enum Preparation {
    Ready(PreparedFile),
    Conflict { actual_digest: [u8; 32] },
}

#[derive(Clone)]
struct Row {
    state: PatchState,
    digest: Option<[u8; 32]>,
}

impl Row {
    const fn new(state: PatchState, digest: Option<[u8; 32]>) -> Self {
        Self { state, digest }
    }
}

struct TransactionOutcome {
    rows: Vec<Row>,
    failure: Option<Failure>,
}

#[derive(Clone, Copy)]
enum Failure {
    Conflict,
    Cancelled,
    TimedOut,
    Filesystem,
    RecoveryRequired,
}

impl Failure {
    const fn error(self) -> (Status, ErrorRecord) {
        match self {
            Self::Conflict => (
                Status::Conflict,
                ErrorRecord {
                    code: ErrorCode::ContentConflict,
                    retry: RetryClass::CorrectRequest,
                    stage: ErrorStage::Execute,
                    evidence: None,
                    argument: None,
                },
            ),
            Self::Cancelled => (
                Status::Cancelled,
                ErrorRecord {
                    code: ErrorCode::ProcessCancelled,
                    retry: RetryClass::Never,
                    stage: ErrorStage::Execute,
                    evidence: None,
                    argument: None,
                },
            ),
            Self::TimedOut => (
                Status::TimedOut,
                ErrorRecord {
                    code: ErrorCode::ProcessTimedOut,
                    retry: RetryClass::RetrySame,
                    stage: ErrorStage::Execute,
                    evidence: None,
                    argument: None,
                },
            ),
            Self::Filesystem => (
                Status::Failed,
                ErrorRecord {
                    code: ErrorCode::Filesystem,
                    retry: RetryClass::RetrySame,
                    stage: ErrorStage::Execute,
                    evidence: None,
                    argument: None,
                },
            ),
            Self::RecoveryRequired => (
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
}

fn resolve_edits(
    arguments: &PatchArgs,
    program: &Program,
) -> Result<Vec<Vec<ResolvedEdit>>, OperationError> {
    let mut by_file = vec![Vec::new(); arguments.paths().len()];
    let mut replacement_bytes = 0_u64;
    for edit in arguments.edits() {
        let replacement: Arc<[u8]> = match edit.replacement() {
            PatchContent::Inline(value) => Arc::from(value.as_bytes()),
            PatchContent::Reference(reference) => program.store().get(*reference)?,
        };
        replacement_bytes = replacement_bytes
            .checked_add(replacement.len() as u64)
            .ok_or(OperationError::WorkLimit)?;
        if replacement_bytes > MAX_PATCH_TOTAL_BYTES {
            return Err(OperationError::WorkLimit);
        }
        by_file
            .get_mut(usize::from(edit.file_index()))
            .ok_or(OperationError::InvalidArgument)?
            .push(ResolvedEdit {
                offset: edit.offset(),
                delete_length: edit.delete_length(),
                replacement,
            });
    }
    Ok(by_file)
}

async fn mutation_sizes(
    workspace: &Workspace,
    paths: &[String],
    program: &Program,
) -> Result<Vec<u64>, OperationError> {
    let workspace = workspace.clone();
    let paths = paths.to_vec();
    let cancellation = program.cancellation().clone();
    let sizes = program
        .compute_pool()
        .run(move || {
            paths
                .iter()
                .map(|path| {
                    if cancellation.is_cancelled() {
                        Err(OperationError::Cancelled)
                    } else {
                        Ok(workspace.mutation_file_size(path)?)
                    }
                })
                .collect::<Result<Vec<_>, OperationError>>()
        })
        .await??;
    let total = sizes.iter().try_fold(0_u64, |total, size| {
        if *size > MAX_PATCH_FILE_BYTES {
            return Err(OperationError::WorkLimit);
        }
        total.checked_add(*size).ok_or(OperationError::WorkLimit)
    })?;
    if total > MAX_PATCH_TOTAL_BYTES {
        return Err(OperationError::WorkLimit);
    }
    Ok(sizes)
}

fn build_tasks(
    arguments: &PatchArgs,
    edits: Vec<Vec<ResolvedEdit>>,
    sizes: Vec<u64>,
) -> Result<Vec<PatchTask>, OperationError> {
    let mut total_result_bytes = 0_u64;
    arguments
        .paths()
        .iter()
        .zip(arguments.expected_digests())
        .zip(edits)
        .zip(sizes)
        .map(|(((path, digest), edits), observed_size)| {
            let deleted = edits.iter().try_fold(0_u64, |total, edit| {
                let end = edit
                    .offset
                    .checked_add(edit.delete_length)
                    .ok_or(OperationError::InvalidArgument)?;
                if end > observed_size {
                    return Err(OperationError::InvalidArgument);
                }
                total
                    .checked_add(edit.delete_length)
                    .ok_or(OperationError::WorkLimit)
            })?;
            let inserted = edits.iter().try_fold(0_u64, |total, edit| {
                total
                    .checked_add(edit.replacement.len() as u64)
                    .ok_or(OperationError::WorkLimit)
            })?;
            let result_size = observed_size
                .checked_sub(deleted)
                .and_then(|size| size.checked_add(inserted))
                .ok_or(OperationError::WorkLimit)?;
            if result_size > MAX_PATCH_FILE_BYTES {
                return Err(OperationError::WorkLimit);
            }
            total_result_bytes = total_result_bytes
                .checked_add(result_size)
                .ok_or(OperationError::WorkLimit)?;
            if total_result_bytes > MAX_PATCH_TOTAL_BYTES {
                return Err(OperationError::WorkLimit);
            }
            Ok(PatchTask {
                path: path.clone(),
                expected_digest: parse_digest(digest).ok_or(OperationError::InvalidArgument)?,
                observed_size,
                result_size: usize::try_from(result_size).map_err(|_| OperationError::WorkLimit)?,
                edits,
            })
        })
        .collect()
}

fn prepare_file(
    workspace: &Workspace,
    task: &PatchTask,
    cancellation: &ash_engine::CancellationToken,
) -> Result<Preparation, OperationError> {
    if cancellation.is_cancelled() {
        return Err(OperationError::Cancelled);
    }
    let original = workspace.read_mutation_limited(&task.path, task.observed_size)?;
    let actual_digest = *blake3::hash(&original).as_bytes();
    if actual_digest != task.expected_digest {
        return Ok(Preparation::Conflict { actual_digest });
    }
    let mut replacement = Vec::with_capacity(task.result_size);
    let mut cursor = 0_usize;
    for edit in &task.edits {
        if cancellation.is_cancelled() {
            return Err(OperationError::Cancelled);
        }
        let start = usize::try_from(edit.offset).map_err(|_| OperationError::InvalidArgument)?;
        let end = usize::try_from(
            edit.offset
                .checked_add(edit.delete_length)
                .ok_or(OperationError::InvalidArgument)?,
        )
        .map_err(|_| OperationError::InvalidArgument)?;
        replacement.extend_from_slice(
            original
                .get(cursor..start)
                .ok_or(OperationError::InvalidArgument)?,
        );
        replacement.extend_from_slice(&edit.replacement);
        cursor = end;
    }
    replacement.extend_from_slice(
        original
            .get(cursor..)
            .ok_or(OperationError::InvalidArgument)?,
    );
    if replacement.len() != task.result_size {
        return Err(OperationError::InvalidArgument);
    }
    Ok(Preparation::Ready(PreparedFile {
        path: task.path.clone(),
        expected_digest: task.expected_digest,
        replacement,
    }))
}

fn emit_without_mutation(
    request_id: u64,
    paths: &[String],
    rows: Vec<Row>,
    failure: Failure,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let (temporary_ids, temporary_mappings) = temporary_paths(paths);
    let temporary = make_response(
        request_id,
        temporary_mappings,
        &temporary_ids,
        &rows,
        Some(failure),
    )?;
    if temporary.encode()?.encode().len() > presentation_limit(program) {
        return Err(OperationError::OutputBudget);
    }
    let (ids, mappings) = intern_paths(program, paths)?;
    let response = make_response(request_id, mappings, &ids, &rows, Some(failure))?;
    charge(program, &response, rows.len())?;
    Ok(response)
}

fn reserve_transaction_response(
    request_id: u64,
    paths: &[String],
    program: &Program,
) -> Result<(Vec<u64>, Vec<ash_protocol::response::PathMapping>), OperationError> {
    let (temporary_ids, temporary_mappings) = temporary_paths(paths);
    let rows = vec![Row::new(PatchState::RecoveryRequired, Some([0xff; 32])); paths.len()];
    let worst = make_response(
        request_id,
        temporary_mappings,
        &temporary_ids,
        &rows,
        Some(Failure::RecoveryRequired),
    )?;
    if worst.encode()?.encode().len() > presentation_limit(program) {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &worst, paths.len())?;
    intern_paths(program, paths)
}

fn run_transaction(
    workspace: &Workspace,
    files: &[PreparedFile],
    program: &Program,
) -> TransactionOutcome {
    let actions = files
        .iter()
        .map(|file| FileAction::replace(&file.path, file.expected_digest, file.replacement.clone()))
        .collect();
    let limits = match FileTransactionLimits::new(MAX_PATCH_FILE_BYTES, MAX_PATCH_TOTAL_BYTES) {
        Ok(limits) => limits,
        Err(_) => {
            return TransactionOutcome {
                rows: vec![Row::new(PatchState::Skipped, None); files.len()],
                failure: Some(Failure::Filesystem),
            };
        }
    };
    let outcome = match workspace.file_transaction(actions, limits, || {
        if program.cancellation().is_cancelled() {
            TransactionControl::Cancelled
        } else if program.budget().check_deadline().is_err() {
            TransactionControl::TimedOut
        } else {
            TransactionControl::Continue
        }
    }) {
        Ok(outcome) => outcome,
        Err(PlatformError::JournalCorrupt | PlatformError::RecoveryRequired) => {
            return TransactionOutcome {
                rows: vec![Row::new(PatchState::RecoveryRequired, None); files.len()],
                failure: Some(Failure::RecoveryRequired),
            };
        }
        Err(_) => {
            return TransactionOutcome {
                rows: vec![Row::new(PatchState::Skipped, None); files.len()],
                failure: Some(Failure::Filesystem),
            };
        }
    };
    let rows = outcome
        .actions
        .into_iter()
        .map(|action| {
            let state = match action.state {
                FileActionState::Committed => PatchState::Committed,
                FileActionState::Conflict => PatchState::Conflict,
                FileActionState::RolledBack => PatchState::RolledBack,
                FileActionState::RecoveryRequired => PatchState::RecoveryRequired,
                FileActionState::Skipped => PatchState::Skipped,
            };
            Row::new(state, action.digest)
        })
        .collect();
    let failure = outcome.failure.map(|failure| match failure {
        FileTransactionFailure::Conflict => Failure::Conflict,
        FileTransactionFailure::Cancelled => Failure::Cancelled,
        FileTransactionFailure::TimedOut => Failure::TimedOut,
        FileTransactionFailure::Filesystem => Failure::Filesystem,
        FileTransactionFailure::RecoveryRequired => Failure::RecoveryRequired,
    });
    TransactionOutcome { rows, failure }
}

fn make_response(
    request_id: u64,
    mappings: Vec<ash_protocol::response::PathMapping>,
    ids: &[u64],
    rows: &[Row],
    failure: Option<Failure>,
) -> Result<FinalResponse, OperationError> {
    if ids.len() != rows.len() {
        return Err(OperationError::InvalidArgument);
    }
    let data = ResultData::Patch(
        ids.iter()
            .zip(rows)
            .map(|(path, row)| PatchResult {
                path: *path,
                state: row.state,
                digest: row.digest.map(hex_digest),
            })
            .collect(),
    );
    if let Some(failure) = failure {
        let (status, error) = failure.error();
        Ok(FinalResponse::failure(
            request_id,
            status,
            error,
            mappings,
            Some(data),
            if matches!(failure, Failure::RecoveryRequired) {
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

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn hex_digest(digest: [u8; 32]) -> String {
    blake3::Hash::from_bytes(digest).to_hex().to_string()
}

fn check_cancelled(program: &Program) -> Result<(), OperationError> {
    if program.cancellation().is_cancelled() {
        Err(OperationError::Cancelled)
    } else {
        program.budget().check_deadline()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use ash_engine::{Engine, Parallelism, SessionConfig};
    use ash_platform::Workspace;
    use ash_protocol::request::{Arguments, Budget, PatchArgs, PatchContent, PatchEdit, Request};
    use ash_protocol::response::PatchState;

    use super::{Failure, PreparedFile, ResolvedEdit, parse_digest, run_transaction};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("ash-patch-transaction-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn digest_parser_and_edit_storage_are_exact() {
        let digest = "0123456789abcdef".repeat(4);
        let parsed = parse_digest(&digest).expect("digest");
        assert_eq!(
            &parsed[..8],
            &[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
        assert!(parse_digest(&digest.to_uppercase()).is_none());
        let edit = ResolvedEdit {
            offset: 1,
            delete_length: 2,
            replacement: Arc::from(&b"x"[..]),
        };
        assert_eq!(edit.replacement.as_ref(), b"x");
    }

    #[tokio::test]
    async fn transaction_preflight_conflict_leaves_every_file_untouched() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("a"), b"one").expect("write");
        fs::write(directory.0.join("b"), b"two").expect("write");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let parallelism = Parallelism::for_available_cpus(2);
        let engine = Engine::new(parallelism).expect("engine");
        let session = engine
            .open_session(SessionConfig::new(1, ".", 4096, parallelism))
            .expect("session");
        let request = Request::new(
            1,
            Arguments::Patch(
                PatchArgs::new(
                    vec!["a".to_owned(), "b".to_owned()],
                    vec!["a".repeat(64), "b".repeat(64)],
                    vec![
                        PatchEdit::new(0, 0, 1, PatchContent::Inline("A".to_owned()))
                            .expect("edit"),
                        PatchEdit::new(1, 0, 1, PatchContent::Inline("B".to_owned()))
                            .expect("edit"),
                    ],
                    0,
                )
                .expect("patch"),
            ),
            Budget::new(256, 8, 30_000).expect("budget"),
        )
        .expect("request");
        let program = session.begin(&request).await.expect("program");
        let first_new = b"One".to_vec();
        let files = vec![
            PreparedFile {
                path: "a".to_owned(),
                expected_digest: *blake3::hash(b"one").as_bytes(),
                replacement: first_new,
            },
            PreparedFile {
                path: "b".to_owned(),
                expected_digest: *blake3::hash(b"stale").as_bytes(),
                replacement: b"Bwo".to_vec(),
            },
        ];
        let outcome = run_transaction(&workspace, &files, &program);
        assert!(matches!(outcome.failure, Some(Failure::Conflict)));
        assert_eq!(outcome.rows[0].state, PatchState::Skipped);
        assert_eq!(outcome.rows[1].state, PatchState::Conflict);
        assert_eq!(fs::read(directory.0.join("a")).expect("read"), b"one");
        assert_eq!(fs::read(directory.0.join("b")).expect("read"), b"two");
    }
}
