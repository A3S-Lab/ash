use ash_engine::{PermitKind, Program};
use ash_platform::{
    FileAction, FileActionOutcome, FileActionState, FileTransactionFailure, FileTransactionLimits,
    MAX_FILE_TRANSACTION_FILE_BYTES, MAX_FILE_TRANSACTION_TOTAL_BYTES, TransactionControl,
    Workspace,
};
use ash_protocol::ason::{self, Key, Table, Value};
use ash_protocol::request::{REF_CASE_INSENSITIVE, REF_REGEX, RefArgs, RefFormula, Request};
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, FsState, MaterializedReference,
    RESULT_NORMALIZED_TEXT, RESULT_PARTIAL, RESULT_REDUCED, RESULT_RETAINED, RESULT_TRUNCATED,
    ReferenceMatch, ReferenceResult, ReferenceSlice, ReleasedReference, ResultData, RetryClass,
    Status,
};
use ash_store::ResultLease;
use regex::{Regex, RegexBuilder};

use crate::OperationError;
use crate::projection::{
    charge, intern_paths, largest_prefix, presentation_limit, temporary_paths,
};

const MAX_REFERENCE_MATCHES: usize = 1_000_000;
const MAX_PROJECTED_LINE_BYTES: usize = 4 * 1024;
const MAX_MATCH_PROJECTION_BYTES: usize = 64 * 1024 * 1024;
const MAX_STRUCTURED_REFERENCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REFERENCE_SLICE_BYTES: u64 = 128 * 1024 * 1024;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &RefArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    if matches!(arguments.formula(), RefFormula::Release) {
        return release(request, arguments.reference(), program);
    }

    let lease = program.store().get(arguments.reference())?;
    if let RefFormula::Materialize { path } = arguments.formula() {
        return materialize(
            workspace,
            request,
            arguments.reference(),
            path,
            lease,
            program,
        )
        .await;
    }
    let _compute = program.acquire(PermitKind::Compute).await?;
    let cancellation = program.cancellation().clone();
    let result = match arguments.formula() {
        RefFormula::Bytes { offset, length } => {
            let (offset, length) = (*offset, *length);
            let bytes = lease
                .read_range(offset, length, MAX_REFERENCE_SLICE_BYTES)
                .await?;
            program
                .compute_pool()
                .run(move || raw_slice(offset, &bytes))
                .await?
                .map(RawResult::Slice)?
        }
        RefFormula::Lines { offset, length } => {
            let (offset, length) = (*offset, *length);
            let bytes = lease.read_all(program.store().limits().max_bytes).await?;
            program
                .compute_pool()
                .run(move || select_lines(&bytes, offset, length, &cancellation))
                .await?
                .map(RawResult::Slice)?
        }
        RefFormula::Search {
            offset,
            length,
            query,
            flags,
        } => {
            let (offset, length, query, flags) = (*offset, *length, query.to_owned(), *flags);
            let bytes = lease.read_all(program.store().limits().max_bytes).await?;
            program
                .compute_pool()
                .run(move || search_bytes(&bytes, offset, length, &query, flags, &cancellation))
                .await?
                .map(RawResult::Search)?
        }
        RefFormula::Project {
            table,
            offset,
            length,
            columns,
        } => {
            let (table, offset, length, columns) =
                (table.to_owned(), *offset, *length, columns.clone());
            let bytes = lease
                .read_all(MAX_STRUCTURED_REFERENCE_BYTES as u64)
                .await?;
            program
                .compute_pool()
                .run(move || project_ason(&bytes, offset, length, &table, &columns, &cancellation))
                .await?
                .map(RawResult::Projection)?
        }
        RefFormula::Release | RefFormula::Materialize { .. } => {
            return Err(OperationError::WorkLimit);
        }
    };
    drop(lease);
    check_cancelled(program)?;
    match result {
        RawResult::Slice(slice) => slice_response(request, arguments.reference(), slice, program),
        RawResult::Search(matches) => {
            search_response(request, arguments.reference(), &matches, program)
        }
        RawResult::Projection(table) => {
            projection_response(request, arguments.reference(), &table, program)
        }
    }
}

enum RawResult {
    Slice(RawSlice),
    Search(Vec<RawMatch>),
    Projection(Table),
}

struct RawSlice {
    offset: u64,
    length: u64,
    bytes: Vec<u8>,
    digest: String,
}

fn project_ason(
    bytes: &[u8],
    offset: u64,
    length: u64,
    table: &str,
    selected: &[String],
    cancellation: &ash_engine::CancellationToken,
) -> Result<Table, OperationError> {
    if bytes.len() > MAX_STRUCTURED_REFERENCE_BYTES {
        return Err(OperationError::WorkLimit);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| OperationError::WrongFileType)?;
    let document = ason::decode(text).map_err(|_| OperationError::WrongFileType)?;
    let Value::Table(source) = document.get(table).ok_or(OperationError::InvalidArgument)? else {
        return Err(OperationError::WrongFileType);
    };

    let mut indexes = Vec::with_capacity(selected.len());
    let mut columns = Vec::with_capacity(selected.len());
    for selected in selected {
        let index = source
            .columns()
            .iter()
            .position(|column| column.as_str() == selected.as_str())
            .ok_or(OperationError::InvalidArgument)?;
        indexes.push(index);
        columns.push(Key::new(selected.clone())?);
    }

    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(source.rows().len());
    let requested = usize::try_from(length).unwrap_or(usize::MAX);
    let end = start.saturating_add(requested).min(source.rows().len());
    let mut rows = Vec::with_capacity(end.saturating_sub(start));
    for (index, row) in source.rows()[start..end].iter().enumerate() {
        if index % 4096 == 0 && cancellation.is_cancelled() {
            return Err(OperationError::Cancelled);
        }
        rows.push(indexes.iter().map(|index| row[*index].clone()).collect());
    }
    Ok(Table::new(columns, rows)?)
}

fn projection_response(
    request: &Request,
    reference: u64,
    table: &Table,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let limit = presentation_limit(program);
    let record_limit = program.budget().remaining().records as usize;
    let total = table.rows().len();
    let prefix = largest_prefix(total, record_limit, limit, |length, truncated| {
        make_projection_response(request.id(), reference, table, length, truncated)
    })?;
    let response =
        make_projection_response(request.id(), reference, table, prefix, prefix < total)?;
    charge(program, &response, prefix)?;
    Ok(response)
}

fn make_projection_response(
    request_id: u64,
    reference: u64,
    table: &Table,
    length: usize,
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let projected = Table::new(table.columns().to_vec(), table.rows()[..length].to_vec())?;
    let flags = RESULT_RETAINED | RESULT_REDUCED | if truncated { RESULT_TRUNCATED } else { 0 };
    Ok(FinalResponse::success(
        request_id,
        vec![],
        ResultData::Reference(ReferenceResult::Projection(projected)),
        flags,
        Some(reference),
    )?)
}

fn select_lines(
    bytes: &[u8],
    offset: u64,
    length: u64,
    cancellation: &ash_engine::CancellationToken,
) -> Result<RawSlice, OperationError> {
    if bytes.is_empty() {
        return raw_slice(offset, &[]);
    }
    let target = offset.saturating_sub(1);
    let mut cursor = 0_usize;
    let mut start = None;
    let mut selected = 0_u64;
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if index % 4096 == 0 && cancellation.is_cancelled() {
            return Err(OperationError::Cancelled);
        }
        let line_index = index as u64;
        if line_index == target {
            start = Some(cursor);
        }
        if line_index >= target && selected < length {
            selected += 1;
        }
        cursor += line.len();
        if selected == length {
            break;
        }
    }
    let Some(start) = start else {
        return raw_slice(offset, &[]);
    };
    raw_slice(offset, &bytes[start..cursor])
}

fn raw_slice(offset: u64, bytes: &[u8]) -> Result<RawSlice, OperationError> {
    Ok(RawSlice {
        offset,
        length: bytes.len() as u64,
        digest: blake3::hash(bytes).to_hex().to_string(),
        bytes: bytes.to_vec(),
    })
}

fn slice_response(
    request: &Request,
    reference: u64,
    slice: RawSlice,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let limit = presentation_limit(program);
    let text = std::str::from_utf8(&slice.bytes).is_ok();
    let projection_ceiling = if text { limit } else { limit / 2 }.min(slice.bytes.len());
    let mut low = 0_usize;
    let mut high = projection_ceiling;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let projected = projection_boundary(&slice.bytes, middle, text);
        let response = make_slice_response(request.id(), reference, &slice, projected, text)?;
        if response.encode()?.encode().len() <= limit {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let projected = projection_boundary(&slice.bytes, low, text);
    let response = make_slice_response(request.id(), reference, &slice, projected, text)?;
    if response.encode()?.encode().len() > limit {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &response, 1)?;
    Ok(response)
}

fn projection_boundary(bytes: &[u8], mut length: usize, text: bool) -> usize {
    length = length.min(bytes.len());
    if text {
        while length > 0 && std::str::from_utf8(&bytes[..length]).is_err() {
            length -= 1;
        }
    }
    length
}

fn make_slice_response(
    request_id: u64,
    reference: u64,
    slice: &RawSlice,
    projected: usize,
    text: bool,
) -> Result<FinalResponse, OperationError> {
    let prefix = &slice.bytes[..projected];
    let (text, hex) = if text {
        let projection = std::str::from_utf8(prefix)
            .map_err(|_| OperationError::WrongFileType)?
            .to_owned();
        (Some(projection), None)
    } else {
        (None, Some(hex(prefix)))
    };
    let truncated = projected < slice.bytes.len();
    let flags = RESULT_RETAINED
        | if truncated {
            RESULT_TRUNCATED | RESULT_REDUCED
        } else {
            0
        };
    Ok(FinalResponse::success(
        request_id,
        vec![],
        ResultData::Reference(ReferenceResult::Slice(ReferenceSlice {
            offset: slice.offset,
            length: slice.length,
            projected_bytes: projected as u64,
            digest: slice.digest.clone(),
            text,
            hex,
        })),
        flags,
        Some(reference),
    )?)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[derive(Clone)]
enum Matcher {
    Literal(String),
    Regex(Regex),
}

impl Matcher {
    fn new(query: &str, flags: u32) -> Result<Self, OperationError> {
        let regex = flags & REF_REGEX != 0;
        let insensitive = flags & REF_CASE_INSENSITIVE != 0;
        if regex || insensitive {
            let pattern = if regex {
                query.to_owned()
            } else {
                regex::escape(query)
            };
            Ok(Self::Regex(
                RegexBuilder::new(&pattern)
                    .case_insensitive(insensitive)
                    .size_limit(16 * 1024 * 1024)
                    .build()?,
            ))
        } else {
            Ok(Self::Literal(query.to_owned()))
        }
    }

    fn visit_matches<F>(&self, line: &str, mut visit: F) -> Result<(), OperationError>
    where
        F: FnMut(usize, usize) -> Result<(), OperationError>,
    {
        match self {
            Self::Literal(query) => {
                for (start, value) in line.match_indices(query) {
                    visit(start, start + value.len())?;
                }
            }
            Self::Regex(regex) => {
                for matched in regex.find_iter(line) {
                    visit(matched.start(), matched.end())?;
                }
            }
        }
        Ok(())
    }
}

struct RawMatch {
    offset: u64,
    line: u64,
    column: u64,
    text: String,
    normalized: bool,
    reduced: bool,
}

fn search_bytes(
    bytes: &[u8],
    offset: u64,
    length: u64,
    query: &str,
    flags: u32,
    cancellation: &ash_engine::CancellationToken,
) -> Result<Vec<RawMatch>, OperationError> {
    let text = std::str::from_utf8(bytes).map_err(|_| OperationError::WrongFileType)?;
    let matcher = Matcher::new(query, flags)?;
    let requested_start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let requested_end = requested_start
        .saturating_add(usize::try_from(length).unwrap_or(usize::MAX))
        .min(bytes.len());
    let range_start = ceil_char_boundary(text, requested_start);
    let range_end = floor_char_boundary(text, requested_end);
    let mut matches = Vec::new();
    let mut projected_bytes = 0_usize;
    let mut line_start = 0_usize;
    for (line_index, raw_line) in text.split_inclusive('\n').enumerate() {
        if line_index % 4096 == 0 && cancellation.is_cancelled() {
            return Err(OperationError::Cancelled);
        }
        if line_start >= range_end {
            break;
        }
        let without_lf = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = without_lf.strip_suffix('\r').unwrap_or(without_lf);
        let normalized = line.len() != without_lf.len();
        matcher.visit_matches(line, |start, end| {
            let absolute_start = line_start.saturating_add(start);
            let absolute_end = line_start.saturating_add(end);
            if absolute_start >= range_start
                && absolute_start < range_end
                && absolute_end <= range_end
            {
                let (projection, reduced) = project_line(line, start, end);
                projected_bytes = projected_bytes
                    .checked_add(projection.len())
                    .ok_or(OperationError::WorkLimit)?;
                if projected_bytes > MAX_MATCH_PROJECTION_BYTES {
                    return Err(OperationError::WorkLimit);
                }
                matches.push(RawMatch {
                    offset: absolute_start as u64,
                    line: line_index as u64 + 1,
                    column: start as u64 + 1,
                    text: projection,
                    normalized,
                    reduced,
                });
                if matches.len() > MAX_REFERENCE_MATCHES {
                    return Err(OperationError::WorkLimit);
                }
            }
            Ok(())
        })?;
        line_start = line_start.saturating_add(raw_line.len());
    }
    Ok(matches)
}

fn project_line(line: &str, match_start: usize, match_end: usize) -> (String, bool) {
    if line.len() <= MAX_PROJECTED_LINE_BYTES {
        return (line.to_owned(), false);
    }
    let target = MAX_PROJECTED_LINE_BYTES.saturating_sub(6);
    let before = target / 2;
    let after = target.saturating_sub(before);
    let start = floor_char_boundary(line, match_start.saturating_sub(before));
    let mut end = ceil_char_boundary(line, match_end.saturating_add(after).min(line.len()));
    if end.saturating_sub(start) > target {
        end = floor_char_boundary(line, start.saturating_add(target));
    }
    let leading = start != 0;
    let trailing = end != line.len();
    let mut projection = String::with_capacity(end.saturating_sub(start).saturating_add(6));
    if leading {
        projection.push_str("...");
    }
    projection.push_str(&line[start..end]);
    if trailing {
        projection.push_str("...");
    }
    (projection, true)
}

fn search_response(
    request: &Request,
    reference: u64,
    matches: &[RawMatch],
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let limit = presentation_limit(program);
    let record_limit = program.budget().remaining().records as usize;
    let prefix = largest_prefix(matches.len(), record_limit, limit, |length, truncated| {
        make_search_response(request.id(), reference, &matches[..length], truncated)
    })?;
    let response = make_search_response(
        request.id(),
        reference,
        &matches[..prefix],
        prefix < matches.len(),
    )?;
    charge(program, &response, prefix)?;
    Ok(response)
}

fn make_search_response(
    request_id: u64,
    reference: u64,
    matches: &[RawMatch],
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let normalized = matches.iter().any(|entry| entry.normalized);
    let reduced = truncated || matches.iter().any(|entry| entry.reduced);
    let flags = RESULT_RETAINED
        | if normalized {
            RESULT_NORMALIZED_TEXT
        } else {
            0
        }
        | if reduced { RESULT_REDUCED } else { 0 }
        | if truncated { RESULT_TRUNCATED } else { 0 };
    Ok(FinalResponse::success(
        request_id,
        vec![],
        ResultData::Reference(ReferenceResult::Search(
            matches
                .iter()
                .map(|entry| ReferenceMatch {
                    offset: entry.offset,
                    line: entry.line,
                    column: entry.column,
                    text: entry.text.clone(),
                })
                .collect(),
        )),
        flags,
        Some(reference),
    )?)
}

async fn materialize(
    workspace: &Workspace,
    request: &Request,
    reference: u64,
    path: &str,
    lease: ResultLease,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    if lease.len() > MAX_FILE_TRANSACTION_FILE_BYTES {
        return Err(OperationError::WorkLimit);
    }
    let bytes = lease.read_all(MAX_FILE_TRANSACTION_FILE_BYTES).await?;
    let path = path.to_owned();
    let size = bytes.len() as u64;
    let action = FileAction::create(path.clone(), bytes.to_vec());
    workspace.validate_file_actions(std::slice::from_ref(&action))?;
    let limits = FileTransactionLimits::new(
        MAX_FILE_TRANSACTION_FILE_BYTES,
        MAX_FILE_TRANSACTION_TOTAL_BYTES,
    )?;
    let (path_id, mappings) =
        reserve_materialization(request.id(), reference, &path, size, program)?;

    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let workspace = workspace.clone();
    let cancellation = program.cancellation().clone();
    let budget = program.budget().clone();
    let outcome = program
        .compute_pool()
        .run(move || {
            workspace.file_transaction(vec![action], limits, || {
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
    drop(bytes);
    drop(lease);
    let result = outcome
        .actions
        .first()
        .ok_or(OperationError::InvalidArgument)?;
    materialization_response(
        request.id(),
        reference,
        mappings,
        path_id,
        size,
        result,
        outcome.failure,
    )
}

fn reserve_materialization(
    request_id: u64,
    reference: u64,
    path: &str,
    size: u64,
    program: &Program,
) -> Result<(u64, Vec<ash_protocol::response::PathMapping>), OperationError> {
    let paths = vec![path.to_owned()];
    let (temporary_ids, temporary_mappings) = temporary_paths(&paths);
    let worst = FileActionOutcome {
        state: FileActionState::RecoveryRequired,
        digest: Some([0xff; 32]),
    };
    let response = materialization_response(
        request_id,
        reference,
        temporary_mappings,
        temporary_ids[0],
        size,
        &worst,
        Some(FileTransactionFailure::RecoveryRequired),
    )?;
    if response.encode()?.encode().len() > presentation_limit(program) {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &response, 1)?;
    let (ids, mappings) = intern_paths(program, &paths)?;
    Ok((ids[0], mappings))
}

fn materialization_response(
    request_id: u64,
    reference: u64,
    mappings: Vec<ash_protocol::response::PathMapping>,
    path: u64,
    size: u64,
    outcome: &FileActionOutcome,
    failure: Option<FileTransactionFailure>,
) -> Result<FinalResponse, OperationError> {
    let data = ResultData::Reference(ReferenceResult::Materialized(MaterializedReference {
        path,
        state: protocol_state(outcome.state),
        size,
        digest: outcome.digest.map(hex_digest),
    }));
    if let Some(failure) = failure {
        let (status, error) = materialization_error(failure);
        Ok(FinalResponse::failure(
            request_id,
            status,
            error,
            mappings,
            Some(data),
            RESULT_RETAINED
                | if failure == FileTransactionFailure::RecoveryRequired {
                    RESULT_PARTIAL
                } else {
                    0
                },
            Some(reference),
        )?)
    } else {
        Ok(FinalResponse::success(
            request_id,
            mappings,
            data,
            RESULT_RETAINED,
            Some(reference),
        )?)
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

const fn materialization_error(failure: FileTransactionFailure) -> (Status, ErrorRecord) {
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

fn hex_digest(digest: [u8; 32]) -> String {
    blake3::Hash::from_bytes(digest).to_hex().to_string()
}

fn release(
    request: &Request,
    reference: u64,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let response = FinalResponse::success(
        request.id(),
        vec![],
        ResultData::Reference(ReferenceResult::Released(ReleasedReference { reference })),
        0,
        None,
    )?;
    if response.encode()?.encode().len() > presentation_limit(program) {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &response, 1)?;
    program.store().release(reference)?;
    Ok(response)
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
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
    use super::{hex, project_line};

    #[test]
    fn binary_projection_is_lowercase_hex_and_long_lines_keep_the_match() {
        assert_eq!(hex(&[0, 15, 16, 255]), "000f10ff");
        let line = format!("{}needle{}", "a".repeat(5_000), "b".repeat(5_000));
        let (projection, reduced) = project_line(&line, 5_000, 5_006);
        assert!(reduced);
        assert!(projection.contains("needle"));
        assert!(projection.len() <= 4_102);
    }
}
