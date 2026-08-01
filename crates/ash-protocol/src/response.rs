//! Typed ASH/1 final results and compact machine errors.

use std::collections::HashSet;

use thiserror::Error;

use crate::ason::{Atom, BuildError, Cell, Document, Field, Key, Record, Table, Value};

pub const RESULT_TRUNCATED: u32 = 1 << 0;
pub const RESULT_REDUCED: u32 = 1 << 1;
pub const RESULT_NORMALIZED_TEXT: u32 = 1 << 2;
pub const RESULT_RETAINED: u32 = 1 << 3;
pub const RESULT_PARTIAL: u32 = 1 << 4;
pub const RESULT_REDACTED: u32 = 1 << 5;
pub const ALL_RESULT_FLAGS: u32 = (1 << 6) - 1;

const PATH_COLUMNS: &[&str] = &["i", "v"];
const EXEC_COLUMNS: &[&str] = &["k", "c", "ms", "o", "e", "ro", "re"];
const READ_COLUMNS: &[&str] = &["p", "o", "n", "h", "t", "r"];
const LIST_COLUMNS: &[&str] = &["p", "k", "z", "m"];
const SEARCH_COLUMNS: &[&str] = &["p", "l", "c", "t"];
const CANCEL_COLUMNS: &[&str] = &["i", "z"];
const ERROR_COLUMNS: &[&str] = &["c", "q", "p", "x", "a"];

/// Stable final status discriminants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Status {
    Success = 0,
    InvalidRequest = 1,
    Unsupported = 2,
    Denied = 3,
    NotFound = 4,
    Failed = 5,
    TimedOut = 6,
    Cancelled = 7,
    Conflict = 8,
    BudgetExceeded = 9,
    Internal = 10,
}

impl Status {
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RetryClass {
    Never = 0,
    CorrectRequest = 1,
    RetrySame = 2,
    Approval = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ErrorStage {
    Decode = 0,
    Validate = 1,
    Resolve = 2,
    Authorize = 3,
    Execute = 4,
    Reduce = 5,
    Retain = 6,
    Encode = 7,
}

/// Stable M1 error codes. New codes may be added without renumbering these.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ErrorCode {
    InvalidArgument = 100,
    UnsupportedOperation = 101,
    DuplicateRequest = 102,
    WorkspaceEscape = 200,
    PathNotFound = 201,
    WrongFileType = 202,
    CapabilityDenied = 300,
    SpawnFailed = 400,
    ProcessFailed = 401,
    ProcessTimedOut = 402,
    ProcessCancelled = 403,
    Filesystem = 500,
    OutputBudget = 600,
    StorageBudget = 601,
    ConcurrencyBudget = 602,
    UnknownReference = 700,
    Internal = 900,
}

impl ErrorCode {
    #[must_use]
    pub const fn code(self) -> u16 {
        self as u16
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorRecord {
    pub code: ErrorCode,
    pub retry: RetryClass,
    pub stage: ErrorStage,
    pub evidence: Option<u64>,
    pub argument: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathMapping {
    pub id: u64,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalResponse {
    request_id: u64,
    status: Status,
    paths: Vec<PathMapping>,
    data: Option<ResultData>,
    error: Option<ErrorRecord>,
    flags: u32,
    reference: Option<u64>,
}

impl FinalResponse {
    pub fn success(
        request_id: u64,
        paths: Vec<PathMapping>,
        data: ResultData,
        flags: u32,
        reference: Option<u64>,
    ) -> Result<Self, ResponseError> {
        Self::new(
            request_id,
            Status::Success,
            paths,
            Some(data),
            None,
            flags,
            reference,
        )
    }

    pub fn failure(
        request_id: u64,
        status: Status,
        error: ErrorRecord,
        paths: Vec<PathMapping>,
        data: Option<ResultData>,
        flags: u32,
        reference: Option<u64>,
    ) -> Result<Self, ResponseError> {
        Self::new(
            request_id,
            status,
            paths,
            data,
            Some(error),
            flags,
            reference,
        )
    }

    fn new(
        request_id: u64,
        status: Status,
        paths: Vec<PathMapping>,
        data: Option<ResultData>,
        error: Option<ErrorRecord>,
        flags: u32,
        reference: Option<u64>,
    ) -> Result<Self, ResponseError> {
        if request_id == 0 {
            return Err(ResponseError::InvalidRequestId);
        }
        if (status == Status::Success) != error.is_none() {
            return Err(ResponseError::StatusErrorMismatch);
        }
        if status == Status::Success && data.is_none() {
            return Err(ResponseError::MissingData);
        }
        if flags & !ALL_RESULT_FLAGS != 0 {
            return Err(ResponseError::InvalidFlags);
        }
        validate_paths(&paths)?;
        if let Some(data) = &data {
            data.validate()?;
        }
        validate_reference(reference)?;
        if let Some(error) = &error {
            validate_reference(error.evidence)?;
            validate_reference(error.argument)?;
        }
        let has_retained = reference.is_some()
            || data.as_ref().is_some_and(ResultData::has_reference)
            || error
                .as_ref()
                .is_some_and(|error| error.evidence.is_some() || error.argument.is_some());
        if (flags & RESULT_RETAINED != 0) != has_retained {
            return Err(ResponseError::RetainedFlagMismatch);
        }
        if flags & RESULT_TRUNCATED != 0
            && reference.is_none()
            && !data.as_ref().is_some_and(ResultData::has_reference)
        {
            return Err(ResponseError::MissingRetainedReference);
        }
        Ok(Self {
            request_id,
            status,
            paths,
            data,
            error,
            flags,
            reference,
        })
    }

    pub fn encode(&self) -> Result<Document, BuildError> {
        let mut fields = vec![
            scalar_field("t", "3")?,
            scalar_field("i", &self.request_id.to_string())?,
            scalar_field("s", &self.status.code().to_string())?,
        ];
        if !self.paths.is_empty() {
            fields.push(Field::new(Key::new("p")?, encode_paths(&self.paths)?));
        }
        if let Some(data) = &self.data {
            fields.push(Field::new(Key::new("d")?, data.encode()?));
        }
        if let Some(error) = &self.error {
            fields.push(Field::new(Key::new("e")?, encode_error(error)?));
        }
        fields.push(scalar_field("z", &self.flags.to_string())?);
        fields.push(Field::new(
            Key::new("r")?,
            Value::Scalar(optional_reference(self.reference)),
        ));
        Document::new(fields)
    }

    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    #[must_use]
    pub const fn flags(&self) -> u32 {
        self.flags
    }

    #[must_use]
    pub const fn reference(&self) -> Option<u64> {
        self.reference
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResultData {
    Exec(ProcessResult),
    Read(Vec<ReadResult>),
    List(Vec<ListEntry>),
    Search(Vec<SearchMatch>),
    Cancel(CancelResult),
}

impl ResultData {
    fn encode(&self) -> Result<Value, BuildError> {
        match self {
            Self::Exec(result) => result.encode(),
            Self::Read(results) => encode_read(results),
            Self::List(entries) => encode_list(entries),
            Self::Search(matches) => encode_search(matches),
            Self::Cancel(result) => result.encode(),
        }
    }

    fn has_reference(&self) -> bool {
        match self {
            Self::Exec(result) => {
                result.stdout.reference.is_some() || result.stderr.reference.is_some()
            }
            Self::Read(results) => results.iter().any(|result| result.reference.is_some()),
            Self::List(_) | Self::Search(_) | Self::Cancel(_) => false,
        }
    }

    fn validate(&self) -> Result<(), ResponseError> {
        match self {
            Self::Exec(result) => {
                validate_reference(result.stdout.reference)?;
                validate_reference(result.stderr.reference)?;
                if matches!(
                    result.termination,
                    TerminationKind::Exited | TerminationKind::Signaled
                ) && result.code.is_none()
                {
                    return Err(ResponseError::InvalidData);
                }
            }
            Self::Read(results) => {
                for result in results {
                    validate_reference(result.reference)?;
                    if result.path == 0
                        || result.digest.len() != 64
                        || !result
                            .digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                        || (result.text.is_none() && result.reference.is_none())
                    {
                        return Err(ResponseError::InvalidData);
                    }
                }
            }
            Self::List(entries) => {
                if entries.iter().any(|entry| entry.path == 0) {
                    return Err(ResponseError::InvalidData);
                }
            }
            Self::Search(matches) => {
                if matches
                    .iter()
                    .any(|entry| entry.path == 0 || entry.line == 0 || entry.column == 0)
                {
                    return Err(ResponseError::InvalidData);
                }
            }
            Self::Cancel(result) => {
                if result.target_id == 0 {
                    return Err(ResponseError::InvalidData);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CancellationState {
    NotActive = 0,
    Signaled = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelResult {
    pub target_id: u64,
    pub state: CancellationState,
}

impl CancelResult {
    fn encode(self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(CANCEL_COLUMNS)?,
            vec![
                unsigned_cell(self.target_id),
                unsigned_cell(u64::from(self.state as u8)),
            ],
        )?))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TerminationKind {
    Exited = 0,
    Signaled = 1,
    TimedOut = 2,
    Cancelled = 3,
    Killed = 4,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamResult {
    pub projection: Option<String>,
    pub reference: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessResult {
    pub termination: TerminationKind,
    pub code: Option<i64>,
    pub elapsed_millis: u64,
    pub stdout: StreamResult,
    pub stderr: StreamResult,
}

impl ProcessResult {
    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(EXEC_COLUMNS)?,
            vec![
                unsigned_cell(u64::from(self.termination as u8)),
                optional_signed(self.code),
                unsigned_cell(self.elapsed_millis),
                optional_text(self.stdout.projection.as_deref()),
                optional_text(self.stderr.projection.as_deref()),
                optional_reference_cell(self.stdout.reference),
                optional_reference_cell(self.stderr.reference),
            ],
        )?))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadResult {
    pub path: u64,
    pub offset: u64,
    pub length: u64,
    pub digest: String,
    pub text: Option<String>,
    pub reference: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FileKind {
    File = 0,
    Directory = 1,
    Symlink = 2,
    Other = 3,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListEntry {
    pub path: u64,
    pub kind: FileKind,
    pub size: u64,
    pub modified_millis: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub path: u64,
    pub line: u64,
    pub column: u64,
    pub text: String,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ResponseError {
    #[error("response request identifier must be non-zero")]
    InvalidRequestId,
    #[error("success and error fields do not agree")]
    StatusErrorMismatch,
    #[error("a successful response requires result data")]
    MissingData,
    #[error("response contains unknown result flag bits")]
    InvalidFlags,
    #[error("response contains an invalid retained reference")]
    InvalidReference,
    #[error("path mappings require unique non-zero identifiers and non-empty values")]
    InvalidPaths,
    #[error("typed result data violates its operation schema")]
    InvalidData,
    #[error("the retained flag does not match the response references")]
    RetainedFlagMismatch,
    #[error("truncated output requires a retained reference")]
    MissingRetainedReference,
}

fn encode_paths(paths: &[PathMapping]) -> Result<Value, BuildError> {
    let rows = paths
        .iter()
        .map(|path| vec![unsigned_cell(path.id), text_cell(&path.value)])
        .collect();
    Ok(Value::Table(Table::new(keys(PATH_COLUMNS)?, rows)?))
}

fn encode_error(error: &ErrorRecord) -> Result<Value, BuildError> {
    Ok(Value::Record(Record::new(
        keys(ERROR_COLUMNS)?,
        vec![
            unsigned_cell(u64::from(error.code.code())),
            unsigned_cell(u64::from(error.retry as u8)),
            unsigned_cell(u64::from(error.stage as u8)),
            optional_reference_cell(error.evidence),
            optional_reference_cell(error.argument),
        ],
    )?))
}

fn encode_read(results: &[ReadResult]) -> Result<Value, BuildError> {
    let rows = results
        .iter()
        .map(|result| {
            vec![
                unsigned_cell(result.path),
                unsigned_cell(result.offset),
                unsigned_cell(result.length),
                text_cell(&result.digest),
                optional_text(result.text.as_deref()),
                optional_reference_cell(result.reference),
            ]
        })
        .collect();
    Ok(Value::Table(Table::new(keys(READ_COLUMNS)?, rows)?))
}

fn encode_list(entries: &[ListEntry]) -> Result<Value, BuildError> {
    let rows = entries
        .iter()
        .map(|entry| {
            vec![
                unsigned_cell(entry.path),
                unsigned_cell(u64::from(entry.kind as u8)),
                unsigned_cell(entry.size),
                optional_signed(entry.modified_millis),
            ]
        })
        .collect();
    Ok(Value::Table(Table::new(keys(LIST_COLUMNS)?, rows)?))
}

fn encode_search(matches: &[SearchMatch]) -> Result<Value, BuildError> {
    let rows = matches
        .iter()
        .map(|entry| {
            vec![
                unsigned_cell(entry.path),
                unsigned_cell(entry.line),
                unsigned_cell(entry.column),
                text_cell(&entry.text),
            ]
        })
        .collect();
    Ok(Value::Table(Table::new(keys(SEARCH_COLUMNS)?, rows)?))
}

fn validate_paths(paths: &[PathMapping]) -> Result<(), ResponseError> {
    let mut ids = HashSet::new();
    let mut values = HashSet::new();
    if paths.windows(2).all(|pair| pair[0].id < pair[1].id)
        && paths.iter().all(|path| {
            path.id != 0
                && !path.value.is_empty()
                && path.value.len() <= 4096
                && !path.value.contains('\0')
                && ids.insert(path.id)
                && values.insert(path.value.as_str())
        })
    {
        Ok(())
    } else {
        Err(ResponseError::InvalidPaths)
    }
}

fn validate_reference(reference: Option<u64>) -> Result<(), ResponseError> {
    if reference == Some(0) {
        Err(ResponseError::InvalidReference)
    } else {
        Ok(())
    }
}

fn keys(values: &[&str]) -> Result<Vec<Key>, BuildError> {
    values.iter().map(|value| Key::new(*value)).collect()
}

fn scalar_field(key: &str, value: &str) -> Result<Field, BuildError> {
    Ok(Field::new(Key::new(key)?, Value::Scalar(Atom::text(value))))
}

fn unsigned_cell(value: u64) -> Cell {
    Cell::Atom(Atom::text(value.to_string()))
}

fn optional_signed(value: Option<i64>) -> Cell {
    value.map_or_else(null_cell, |value| Cell::Atom(Atom::text(value.to_string())))
}

fn text_cell(value: &str) -> Cell {
    Cell::Atom(Atom::text(value))
}

fn optional_text(value: Option<&str>) -> Cell {
    value.map_or_else(null_cell, text_cell)
}

fn optional_reference(value: Option<u64>) -> Atom {
    value.map_or(Atom::Null, Atom::reference)
}

fn optional_reference_cell(value: Option<u64>) -> Cell {
    Cell::Atom(optional_reference(value))
}

fn null_cell() -> Cell {
    Cell::Atom(Atom::Null)
}

#[cfg(test)]
mod tests;
