//! Typed ASH/1 operation requests and resource budgets.

use std::collections::{HashMap, HashSet, VecDeque};

use thiserror::Error;

use crate::ason::{Atom, BuildError, Cell, Document, Field, Key, Record, Table, Value};
use crate::{ApprovalToken, Capability, Operation};

const ENVELOPE_FIELDS: &[&str] = &["t", "i", "o", "a", "u"];
const PERMITTED_ENVELOPE_FIELDS: &[&str] = &["t", "i", "o", "a", "u", "v"];
const BUDGET_COLUMNS: &[&str] = &["tok", "rec", "ms"];
const EXEC_COLUMNS: &[&str] = &["x", "v", "c", "e", "in", "f"];
const READ_COLUMNS: &[&str] = &["p", "m", "o", "n"];
const LIST_COLUMNS: &[&str] = &["p", "d", "f"];
const SEARCH_COLUMNS: &[&str] = &["q", "p", "f"];
const PATCH_COLUMNS: &[&str] = &["p", "h", "i", "o", "n", "v", "f"];
const FS_COLUMNS: &[&str] = &["i", "k", "p", "q", "h", "v"];
const SNAPSHOT_COLUMNS: &[&str] = &["p", "d", "m", "r", "f"];
const CANCEL_COLUMNS: &[&str] = &["i"];
const BATCH_COLUMNS: &[&str] = &["i", "d", "o", "a"];

pub const MAX_REQUEST_TOKENS: u32 = 1_048_576;
pub const MAX_REQUEST_RECORDS: u32 = 1_000_000;
pub const MAX_REQUEST_MILLIS: u64 = 86_400_000;
pub const MAX_ARGUMENT_ITEMS: usize = 1024;
pub const MAX_BATCH_NODES: usize = 256;
pub const MAX_BATCH_EDGES: usize = 4096;
pub const MAX_FS_ACTIONS: usize = 256;

pub const EXEC_CLEAR_ENVIRONMENT: u32 = 1 << 0;
pub const LIST_INCLUDE_HIDDEN: u32 = 1 << 0;
pub const LIST_FILES_ONLY: u32 = 1 << 1;
pub const LIST_DIRECTORIES_ONLY: u32 = 1 << 2;
pub const SEARCH_REGEX: u32 = 1 << 0;
pub const SEARCH_CASE_INSENSITIVE: u32 = 1 << 1;
pub const SEARCH_INCLUDE_HIDDEN: u32 = 1 << 2;
pub const REF_REGEX: u32 = 1 << 0;
pub const REF_CASE_INSENSITIVE: u32 = 1 << 1;
pub const SNAPSHOT_INCLUDE_HIDDEN: u32 = 1 << 0;

const EXEC_FLAGS: u32 = EXEC_CLEAR_ENVIRONMENT;
const LIST_FLAGS: u32 = LIST_INCLUDE_HIDDEN | LIST_FILES_ONLY | LIST_DIRECTORIES_ONLY;
const SEARCH_FLAGS: u32 = SEARCH_REGEX | SEARCH_CASE_INSENSITIVE | SEARCH_INCLUDE_HIDDEN;
const REF_SEARCH_FLAGS: u32 = REF_REGEX | REF_CASE_INSENSITIVE;
const PATCH_FLAGS: u32 = 0;
const SNAPSHOT_FLAGS: u32 = SNAPSHOT_INCLUDE_HIDDEN;
const MAX_REF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_REF_LINES: u64 = 1_000_000;
const MAX_PATCH_INLINE_BYTES: usize = 8 * 1024 * 1024;

/// Per-request presentation and wall-clock ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    tokens: u32,
    records: u32,
    millis: u64,
}

impl Budget {
    pub fn new(tokens: u32, records: u32, millis: u64) -> Result<Self, RequestError> {
        let budget = Self {
            tokens,
            records,
            millis,
        };
        budget.validate()?;
        Ok(budget)
    }

    #[must_use]
    pub const fn tokens(self) -> u32 {
        self.tokens
    }

    #[must_use]
    pub const fn records(self) -> u32 {
        self.records
    }

    #[must_use]
    pub const fn millis(self) -> u64 {
        self.millis
    }

    fn validate(self) -> Result<(), RequestError> {
        if self.tokens == 0 || self.tokens > MAX_REQUEST_TOKENS {
            return Err(RequestError::InvalidLimit("tok"));
        }
        if self.records == 0 || self.records > MAX_REQUEST_RECORDS {
            return Err(RequestError::InvalidLimit("rec"));
        }
        if self.millis == 0 || self.millis > MAX_REQUEST_MILLIS {
            return Err(RequestError::InvalidLimit("ms"));
        }
        Ok(())
    }
}

/// Validated request envelope shared by one-shot and persistent transports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    id: u64,
    arguments: Arguments,
    budget: Budget,
    permit: Option<ApprovalToken>,
}

impl Request {
    pub fn new(id: u64, arguments: Arguments, budget: Budget) -> Result<Self, RequestError> {
        if id == 0 {
            return Err(RequestError::InvalidUnsigned("i"));
        }
        if matches!(&arguments, Arguments::Cancel(cancel) if cancel.target_id() == id) {
            return Err(RequestError::UnexpectedValue("i"));
        }
        arguments.validate()?;
        if let Arguments::Batch(batch) = &arguments {
            let nodes =
                u32::try_from(batch.nodes.len()).map_err(|_| RequestError::InvalidLimit("a"))?;
            if nodes > budget.tokens || nodes > budget.records {
                return Err(RequestError::InvalidLimit("u"));
            }
        }
        if let Arguments::Fs(filesystem) = &arguments {
            let actions = u32::try_from(filesystem.actions.len())
                .map_err(|_| RequestError::InvalidLimit("a"))?;
            if actions > budget.tokens || actions > budget.records {
                return Err(RequestError::InvalidLimit("u"));
            }
        }
        budget.validate()?;
        Ok(Self {
            id,
            arguments,
            budget,
            permit: None,
        })
    }

    pub fn decode(document: &Document) -> Result<Self, RequestError> {
        let has_permit = if expect_fields(document, ENVELOPE_FIELDS).is_ok() {
            false
        } else {
            expect_fields(document, PERMITTED_ENVELOPE_FIELDS)?;
            true
        };
        expect_unsigned(document.get("t"), "t", 1)?;
        let id = unsigned(document.get("i"), "i")?;
        let operation_text = text(document.get("o"), "o")?;
        let operation = operation_text
            .as_bytes()
            .first()
            .copied()
            .filter(|_| operation_text.len() == 1)
            .and_then(Operation::from_id)
            .ok_or(RequestError::UnsupportedOperation)?;
        let arguments =
            Arguments::decode(operation, document.get("a").ok_or(RequestError::Fields)?)?;
        let budget_record = record(document.get("u"), "u")?;
        expect_columns(budget_record, BUDGET_COLUMNS)?;
        let budget_values = budget_record.values();
        let budget = Budget::new(
            narrow_u32(unsigned_cell(&budget_values[0], "tok")?, "tok")?,
            narrow_u32(unsigned_cell(&budget_values[1], "rec")?, "rec")?,
            unsigned_cell(&budget_values[2], "ms")?,
        )?;
        let mut request = Self::new(id, arguments, budget)?;
        if has_permit {
            request.permit = Some(
                ApprovalToken::parse(text(document.get("v"), "v")?)
                    .map_err(|_| RequestError::InvalidText("v"))?,
            );
        }
        Ok(request)
    }

    /// Extracts a usable request identifier before full schema validation.
    #[must_use]
    pub fn id_hint(document: &Document) -> Option<u64> {
        unsigned(document.get("i"), "i").ok().filter(|id| *id != 0)
    }

    pub fn encode(&self) -> Result<Document, BuildError> {
        let operation = char::from(self.operation().id()).to_string();
        let budget = self.budget;
        let mut fields = vec![
            scalar_field("t", "1")?,
            scalar_field("i", &self.id.to_string())?,
            scalar_field("o", &operation)?,
            Field::new(Key::new("a")?, self.arguments.encode()?),
            Field::new(
                Key::new("u")?,
                Value::Record(Record::new(
                    keys(BUDGET_COLUMNS)?,
                    vec![
                        unsigned_value(u64::from(budget.tokens)),
                        unsigned_value(u64::from(budget.records)),
                        unsigned_value(budget.millis),
                    ],
                )?),
            ),
        ];
        if let Some(permit) = &self.permit {
            fields.push(scalar_field("v", &permit.encode())?);
        }
        Document::new(fields)
    }

    /// Attaches an opaque approval token to a semantic retry.
    #[must_use]
    pub fn with_permit(mut self, permit: ApprovalToken) -> Self {
        self.permit = Some(permit);
        self
    }

    /// Canonical action-only representation used for approval binding.
    pub fn authorization_target(&self) -> Result<Document, BuildError> {
        Document::new(vec![
            scalar_field("v", "1")?,
            scalar_field("o", &char::from(self.operation().id()).to_string())?,
            Field::new(Key::new("a")?, self.arguments.encode()?),
            scalar_field("c", &self.required_capabilities().to_string())?,
        ])
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub const fn operation(&self) -> Operation {
        self.arguments.operation()
    }

    #[must_use]
    pub const fn arguments(&self) -> &Arguments {
        &self.arguments
    }

    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    #[must_use]
    pub const fn permit(&self) -> Option<&ApprovalToken> {
        self.permit.as_ref()
    }

    #[must_use]
    pub fn required_capabilities(&self) -> u64 {
        self.arguments.required_capabilities()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Arguments {
    Exec(ExecArgs),
    Read(ReadArgs),
    List(ListArgs),
    Search(SearchArgs),
    Patch(PatchArgs),
    Fs(FsArgs),
    Batch(BatchArgs),
    Snapshot(SnapshotArgs),
    Ref(RefArgs),
    Cancel(CancelArgs),
}

impl Arguments {
    #[must_use]
    pub const fn operation(&self) -> Operation {
        match self {
            Self::Exec(_) => Operation::Exec,
            Self::Read(_) => Operation::Read,
            Self::List(_) => Operation::List,
            Self::Search(_) => Operation::Search,
            Self::Patch(_) => Operation::Patch,
            Self::Fs(_) => Operation::Fs,
            Self::Batch(_) => Operation::Batch,
            Self::Snapshot(_) => Operation::Snapshot,
            Self::Ref(_) => Operation::Ref,
            Self::Cancel(_) => Operation::Cancel,
        }
    }

    #[must_use]
    pub fn required_capabilities(&self) -> u64 {
        match self {
            Self::Exec(_) => Capability::HostProcess.mask(),
            Self::Read(_) | Self::List(_) | Self::Search(_) | Self::Snapshot(_) => {
                Capability::WorkspaceRead.mask()
            }
            Self::Patch(_) | Self::Fs(_) => Capability::WorkspaceWrite.mask(),
            Self::Batch(arguments) => arguments.nodes.iter().fold(0, |mask, node| {
                mask | node.arguments.required_capabilities()
            }),
            Self::Ref(arguments) => {
                Capability::RetainedResult.mask()
                    | if matches!(arguments.formula(), RefFormula::Materialize { .. }) {
                        Capability::WorkspaceWrite.mask()
                    } else {
                        0
                    }
            }
            // Cancellation must remain available even when the target is
            // waiting for a capability-gated program permit.
            Self::Cancel(_) => 0,
        }
    }

    fn decode(operation: Operation, value: &Value) -> Result<Self, RequestError> {
        if matches!(operation, Operation::Fs | Operation::Batch) {
            let Value::Table(table) = value else {
                return Err(RequestError::ExpectedTable("a"));
            };
            return match operation {
                Operation::Fs => FsArgs::decode(table).map(Self::Fs),
                Operation::Batch => BatchArgs::decode(table).map(Self::Batch),
                _ => unreachable!(),
            };
        }
        let Value::Record(record) = value else {
            return Err(RequestError::ExpectedRecord("a"));
        };
        Self::decode_record(operation, record)
    }

    fn decode_record(operation: Operation, record: &Record) -> Result<Self, RequestError> {
        match operation {
            Operation::Exec => ExecArgs::decode(record).map(Self::Exec),
            Operation::Read => ReadArgs::decode(record).map(Self::Read),
            Operation::List => ListArgs::decode(record).map(Self::List),
            Operation::Search => SearchArgs::decode(record).map(Self::Search),
            Operation::Patch => PatchArgs::decode(record).map(Self::Patch),
            Operation::Snapshot => SnapshotArgs::decode(record).map(Self::Snapshot),
            Operation::Ref => RefArgs::decode(record).map(Self::Ref),
            Operation::Cancel => CancelArgs::decode(record).map(Self::Cancel),
            _ => Err(RequestError::UnsupportedOperation),
        }
    }

    fn encode(&self) -> Result<Value, BuildError> {
        match self {
            Self::Exec(arguments) => arguments.encode(),
            Self::Read(arguments) => arguments.encode(),
            Self::List(arguments) => arguments.encode(),
            Self::Search(arguments) => arguments.encode(),
            Self::Patch(arguments) => arguments.encode(),
            Self::Fs(arguments) => arguments.encode(),
            Self::Batch(arguments) => arguments.encode(),
            Self::Snapshot(arguments) => arguments.encode(),
            Self::Ref(arguments) => arguments.encode(),
            Self::Cancel(arguments) => arguments.encode(),
        }
    }

    fn validate(&self) -> Result<(), RequestError> {
        match self {
            Self::Exec(arguments) => arguments.validate(),
            Self::Read(arguments) => arguments.validate(),
            Self::List(arguments) => arguments.validate(),
            Self::Search(arguments) => arguments.validate(),
            Self::Patch(arguments) => arguments.validate(),
            Self::Fs(arguments) => arguments.validate(),
            Self::Batch(arguments) => arguments.validate(),
            Self::Snapshot(arguments) => arguments.validate(),
            Self::Ref(arguments) => arguments.validate(),
            Self::Cancel(arguments) => arguments.validate(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecArgs {
    executable: String,
    argv: Vec<String>,
    cwd: String,
    environment: Vec<String>,
    stdin: InputSource,
    flags: u32,
}

impl ExecArgs {
    pub fn new(
        executable: impl Into<String>,
        argv: Vec<String>,
        cwd: impl Into<String>,
        environment: Vec<String>,
        stdin: InputSource,
        flags: u32,
    ) -> Result<Self, RequestError> {
        let arguments = Self {
            executable: executable.into(),
            argv,
            cwd: cwd.into(),
            environment,
            stdin,
            flags,
        };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        expect_columns(record, EXEC_COLUMNS)?;
        let values = record.values();
        Self::new(
            text_cell(&values[0], "x")?,
            text_vector(&values[1], "v")?,
            text_cell(&values[2], "c")?,
            text_vector(&values[3], "e")?,
            InputSource::decode(&values[4])?,
            narrow_u32(unsigned_cell(&values[5], "f")?, "f")?,
        )
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(EXEC_COLUMNS)?,
            vec![
                text_value(&self.executable),
                text_vector_value(&self.argv),
                text_value(&self.cwd),
                text_vector_value(&self.environment),
                self.stdin.encode(),
                unsigned_value(u64::from(self.flags)),
            ],
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        validate_text(&self.executable, "x", 4096)?;
        validate_text(&self.cwd, "c", 4096)?;
        validate_text_items(&self.argv, "v", MAX_ARGUMENT_ITEMS, 1024 * 1024)?;
        validate_environment(&self.environment)?;
        if let InputSource::Inline(value) = &self.stdin {
            validate_bounded_text(value, "in", 1024 * 1024)?;
        } else if matches!(self.stdin, InputSource::Reference(0)) {
            return Err(RequestError::InvalidUnsigned("in"));
        }
        validate_flags(self.flags, EXEC_FLAGS)
    }

    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    #[must_use]
    pub fn environment(&self) -> &[String] {
        &self.environment
    }

    #[must_use]
    pub const fn stdin(&self) -> &InputSource {
        &self.stdin
    }

    #[must_use]
    pub const fn flags(&self) -> u32 {
        self.flags
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputSource {
    None,
    Inline(String),
    Reference(u64),
}

impl InputSource {
    fn decode(cell: &Cell) -> Result<Self, RequestError> {
        match cell {
            Cell::Atom(Atom::Null) => Ok(Self::None),
            Cell::Atom(Atom::Text(value)) => Ok(Self::Inline(value.clone())),
            Cell::Atom(Atom::Reference(id)) => Ok(Self::Reference(*id)),
            Cell::Vector(_) => Err(RequestError::ExpectedScalar("in")),
        }
    }

    fn encode(&self) -> Cell {
        match self {
            Self::None => Cell::Atom(Atom::Null),
            Self::Inline(value) => text_value(value),
            Self::Reference(id) => Cell::Atom(Atom::reference(*id)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadMode {
    Bytes,
    Lines,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadArgs {
    paths: Vec<String>,
    mode: ReadMode,
    offset: u64,
    length: u64,
}

impl ReadArgs {
    pub fn new(
        paths: Vec<String>,
        mode: ReadMode,
        offset: u64,
        length: u64,
    ) -> Result<Self, RequestError> {
        let arguments = Self {
            paths,
            mode,
            offset,
            length,
        };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        expect_columns(record, READ_COLUMNS)?;
        let values = record.values();
        let mode = match unsigned_cell(&values[1], "m")? {
            0 => ReadMode::Bytes,
            1 => ReadMode::Lines,
            _ => return Err(RequestError::UnexpectedValue("m")),
        };
        Self::new(
            text_vector(&values[0], "p")?,
            mode,
            unsigned_cell(&values[2], "o")?,
            unsigned_cell(&values[3], "n")?,
        )
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(READ_COLUMNS)?,
            vec![
                text_vector_value(&self.paths),
                unsigned_value(match self.mode {
                    ReadMode::Bytes => 0,
                    ReadMode::Lines => 1,
                }),
                unsigned_value(self.offset),
                unsigned_value(self.length),
            ],
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        validate_paths(&self.paths)?;
        if self.length == 0 {
            return Err(RequestError::InvalidLimit("n"));
        }
        if self.mode == ReadMode::Lines && self.offset == 0 {
            return Err(RequestError::InvalidLimit("o"));
        }
        Ok(())
    }

    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    #[must_use]
    pub const fn mode(&self) -> ReadMode {
        self.mode
    }

    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListArgs {
    paths: Vec<String>,
    depth: u16,
    flags: u32,
}

impl ListArgs {
    pub fn new(paths: Vec<String>, depth: u16, flags: u32) -> Result<Self, RequestError> {
        let arguments = Self {
            paths,
            depth,
            flags,
        };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        expect_columns(record, LIST_COLUMNS)?;
        let values = record.values();
        Self::new(
            text_vector(&values[0], "p")?,
            narrow_u16(unsigned_cell(&values[1], "d")?, "d")?,
            narrow_u32(unsigned_cell(&values[2], "f")?, "f")?,
        )
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(LIST_COLUMNS)?,
            vec![
                text_vector_value(&self.paths),
                unsigned_value(u64::from(self.depth)),
                unsigned_value(u64::from(self.flags)),
            ],
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        validate_paths(&self.paths)?;
        if self.depth > 64 {
            return Err(RequestError::InvalidLimit("d"));
        }
        validate_flags(self.flags, LIST_FLAGS)?;
        if self.flags & LIST_FILES_ONLY != 0 && self.flags & LIST_DIRECTORIES_ONLY != 0 {
            return Err(RequestError::UnexpectedValue("f"));
        }
        Ok(())
    }

    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    #[must_use]
    pub const fn depth(&self) -> u16 {
        self.depth
    }

    #[must_use]
    pub const fn flags(&self) -> u32 {
        self.flags
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchArgs {
    query: String,
    paths: Vec<String>,
    flags: u32,
}

impl SearchArgs {
    pub fn new(
        query: impl Into<String>,
        paths: Vec<String>,
        flags: u32,
    ) -> Result<Self, RequestError> {
        let arguments = Self {
            query: query.into(),
            paths,
            flags,
        };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        expect_columns(record, SEARCH_COLUMNS)?;
        let values = record.values();
        Self::new(
            text_cell(&values[0], "q")?,
            text_vector(&values[1], "p")?,
            narrow_u32(unsigned_cell(&values[2], "f")?, "f")?,
        )
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(SEARCH_COLUMNS)?,
            vec![
                text_value(&self.query),
                text_vector_value(&self.paths),
                unsigned_value(u64::from(self.flags)),
            ],
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        validate_text(&self.query, "q", 1024 * 1024)?;
        validate_paths(&self.paths)?;
        validate_flags(self.flags, SEARCH_FLAGS)
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    #[must_use]
    pub const fn flags(&self) -> u32 {
        self.flags
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatchContent {
    Inline(String),
    Reference(u64),
}

impl PatchContent {
    fn decode(atom: &Atom) -> Result<Self, RequestError> {
        match atom {
            Atom::Text(value) => Ok(Self::Inline(value.clone())),
            Atom::Reference(reference) if *reference != 0 => Ok(Self::Reference(*reference)),
            Atom::Null | Atom::Reference(_) => Err(RequestError::InvalidText("v")),
        }
    }

    fn encode(&self) -> Atom {
        match self {
            Self::Inline(value) => Atom::text(value),
            Self::Reference(reference) => Atom::reference(*reference),
        }
    }

    fn validate(&self) -> Result<(), RequestError> {
        match self {
            Self::Inline(value) => validate_bounded_text(value, "v", 1024 * 1024),
            Self::Reference(0) => Err(RequestError::InvalidUnsigned("v")),
            Self::Reference(_) => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchEdit {
    file_index: u16,
    offset: u64,
    delete_length: u64,
    replacement: PatchContent,
}

impl PatchEdit {
    pub fn new(
        file_index: u16,
        offset: u64,
        delete_length: u64,
        replacement: PatchContent,
    ) -> Result<Self, RequestError> {
        let edit = Self {
            file_index,
            offset,
            delete_length,
            replacement,
        };
        edit.replacement.validate()?;
        offset
            .checked_add(delete_length)
            .ok_or(RequestError::IntegerRange("n"))?;
        Ok(edit)
    }

    #[must_use]
    pub const fn file_index(&self) -> u16 {
        self.file_index
    }

    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    #[must_use]
    pub const fn delete_length(&self) -> u64 {
        self.delete_length
    }

    #[must_use]
    pub const fn replacement(&self) -> &PatchContent {
        &self.replacement
    }
}

/// Canonical compare-and-swap byte edits over one or more existing files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchArgs {
    paths: Vec<String>,
    expected_digests: Vec<String>,
    edits: Vec<PatchEdit>,
    flags: u32,
}

impl PatchArgs {
    pub fn new(
        paths: Vec<String>,
        expected_digests: Vec<String>,
        edits: Vec<PatchEdit>,
        flags: u32,
    ) -> Result<Self, RequestError> {
        let arguments = Self {
            paths,
            expected_digests,
            edits,
            flags,
        };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        expect_columns(record, PATCH_COLUMNS)?;
        let values = record.values();
        let paths = text_vector(&values[0], "p")?;
        let expected_digests = text_vector(&values[1], "h")?;
        let file_indices = unsigned_vector(&values[2], "i")?;
        let offsets = unsigned_vector(&values[3], "o")?;
        let delete_lengths = unsigned_vector(&values[4], "n")?;
        let replacements = atom_vector(&values[5], "v")?;
        let edit_count = file_indices.len();
        if offsets.len() != edit_count
            || delete_lengths.len() != edit_count
            || replacements.len() != edit_count
        {
            return Err(RequestError::InvalidLimit("i"));
        }
        let edits = file_indices
            .into_iter()
            .zip(offsets)
            .zip(delete_lengths)
            .zip(replacements)
            .map(|(((file_index, offset), delete_length), replacement)| {
                PatchEdit::new(
                    narrow_u16(file_index, "i")?,
                    offset,
                    delete_length,
                    PatchContent::decode(&replacement)?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(
            paths,
            expected_digests,
            edits,
            narrow_u32(unsigned_cell(&values[6], "f")?, "f")?,
        )
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(PATCH_COLUMNS)?,
            vec![
                text_vector_value(&self.paths),
                text_vector_value(&self.expected_digests),
                Cell::Vector(
                    self.edits
                        .iter()
                        .map(|edit| Atom::text(edit.file_index.to_string()))
                        .collect(),
                ),
                Cell::Vector(
                    self.edits
                        .iter()
                        .map(|edit| Atom::text(edit.offset.to_string()))
                        .collect(),
                ),
                Cell::Vector(
                    self.edits
                        .iter()
                        .map(|edit| Atom::text(edit.delete_length.to_string()))
                        .collect(),
                ),
                Cell::Vector(
                    self.edits
                        .iter()
                        .map(|edit| edit.replacement.encode())
                        .collect(),
                ),
                unsigned_value(u64::from(self.flags)),
            ],
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        validate_paths(&self.paths)?;
        if !self
            .paths
            .windows(2)
            .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
        {
            return Err(RequestError::UnexpectedValue("p"));
        }
        if self.expected_digests.len() != self.paths.len()
            || self
                .expected_digests
                .iter()
                .any(|digest| !valid_digest(digest))
        {
            return Err(RequestError::InvalidText("h"));
        }
        if self.edits.is_empty() || self.edits.len() > MAX_ARGUMENT_ITEMS {
            return Err(RequestError::InvalidLimit("i"));
        }
        let mut seen = vec![false; self.paths.len()];
        let mut inline_bytes = 0_usize;
        let mut previous: Option<&PatchEdit> = None;
        for edit in &self.edits {
            edit.replacement.validate()?;
            let file_index = usize::from(edit.file_index);
            let Some(seen_file) = seen.get_mut(file_index) else {
                return Err(RequestError::UnexpectedValue("i"));
            };
            *seen_file = true;
            edit.offset
                .checked_add(edit.delete_length)
                .ok_or(RequestError::IntegerRange("n"))?;
            if let PatchContent::Inline(value) = &edit.replacement {
                inline_bytes = inline_bytes
                    .checked_add(value.len())
                    .ok_or(RequestError::InvalidLimit("v"))?;
            }
            if let Some(previous) = previous
                && (edit.file_index < previous.file_index
                    || (edit.file_index == previous.file_index
                        && (edit.offset <= previous.offset
                            || edit.offset
                                < previous.offset.saturating_add(previous.delete_length))))
            {
                return Err(RequestError::UnexpectedValue("o"));
            }
            previous = Some(edit);
        }
        if seen.iter().any(|seen| !seen) {
            return Err(RequestError::InvalidLimit("i"));
        }
        if inline_bytes > MAX_PATCH_INLINE_BYTES {
            return Err(RequestError::InvalidLimit("v"));
        }
        validate_flags(self.flags, PATCH_FLAGS)
    }

    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    #[must_use]
    pub fn expected_digests(&self) -> &[String] {
        &self.expected_digests
    }

    #[must_use]
    pub fn edits(&self) -> &[PatchEdit] {
        &self.edits
    }

    #[must_use]
    pub const fn flags(&self) -> u32 {
        self.flags
    }
}

/// File-only transaction actions. Directory and overwrite semantics require
/// separate capabilities and are intentionally absent from the core schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FsActionKind {
    Create = 0,
    Copy = 1,
    Move = 2,
    Remove = 3,
}

impl FsActionKind {
    fn decode(value: u64) -> Result<Self, RequestError> {
        match value {
            0 => Ok(Self::Create),
            1 => Ok(Self::Copy),
            2 => Ok(Self::Move),
            3 => Ok(Self::Remove),
            _ => Err(RequestError::UnexpectedValue("k")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsAction {
    id: u64,
    kind: FsActionKind,
    path: String,
    destination: Option<String>,
    expected_digest: Option<String>,
    content: Option<PatchContent>,
}

impl FsAction {
    pub fn new(
        id: u64,
        kind: FsActionKind,
        path: impl Into<String>,
        destination: Option<String>,
        expected_digest: Option<String>,
        content: Option<PatchContent>,
    ) -> Result<Self, RequestError> {
        let action = Self {
            id,
            kind,
            path: path.into(),
            destination,
            expected_digest,
            content,
        };
        action.validate()?;
        Ok(action)
    }

    fn decode(row: &[Cell]) -> Result<Self, RequestError> {
        let content = match &row[5] {
            Cell::Atom(Atom::Null) => None,
            Cell::Atom(value) => Some(PatchContent::decode(value)?),
            Cell::Vector(_) => return Err(RequestError::ExpectedScalar("v")),
        };
        Self::new(
            unsigned_cell(&row[0], "i")?,
            FsActionKind::decode(unsigned_cell(&row[1], "k")?)?,
            text_cell(&row[2], "p")?,
            optional_text_cell(&row[3], "q")?,
            optional_text_cell(&row[4], "h")?,
            content,
        )
    }

    fn encode(&self) -> Vec<Cell> {
        vec![
            unsigned_value(self.id),
            unsigned_value(u64::from(self.kind as u8)),
            text_value(&self.path),
            self.destination
                .as_deref()
                .map_or_else(|| Cell::Atom(Atom::Null), text_value),
            self.expected_digest
                .as_deref()
                .map_or_else(|| Cell::Atom(Atom::Null), text_value),
            self.content.as_ref().map_or_else(
                || Cell::Atom(Atom::Null),
                |content| Cell::Atom(content.encode()),
            ),
        ]
    }

    fn validate(&self) -> Result<(), RequestError> {
        if self.id == 0 {
            return Err(RequestError::InvalidUnsigned("i"));
        }
        validate_text(&self.path, "p", 4096)?;
        if let Some(destination) = &self.destination {
            validate_text(destination, "q", 4096)?;
            if destination == &self.path {
                return Err(RequestError::UnexpectedValue("q"));
            }
        }
        if let Some(digest) = &self.expected_digest
            && !valid_digest(digest)
        {
            return Err(RequestError::InvalidText("h"));
        }
        if let Some(content) = &self.content {
            content.validate()?;
        }
        let shape_is_valid = match self.kind {
            FsActionKind::Create => {
                self.destination.is_none()
                    && self.expected_digest.is_none()
                    && self.content.is_some()
            }
            FsActionKind::Copy | FsActionKind::Move => {
                self.destination.is_some()
                    && self.expected_digest.is_some()
                    && self.content.is_none()
            }
            FsActionKind::Remove => {
                self.destination.is_none()
                    && self.expected_digest.is_some()
                    && self.content.is_none()
            }
        };
        if shape_is_valid {
            Ok(())
        } else {
            Err(RequestError::UnexpectedValue("k"))
        }
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> FsActionKind {
        self.kind
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn destination(&self) -> Option<&str> {
        self.destination.as_deref()
    }

    #[must_use]
    pub fn expected_digest(&self) -> Option<&str> {
        self.expected_digest.as_deref()
    }

    #[must_use]
    pub const fn content(&self) -> Option<&PatchContent> {
        self.content.as_ref()
    }
}

/// A bounded, stable-order file transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsArgs {
    actions: Vec<FsAction>,
}

impl FsArgs {
    pub fn new(actions: Vec<FsAction>) -> Result<Self, RequestError> {
        let arguments = Self { actions };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(table: &Table) -> Result<Self, RequestError> {
        expect_table_columns(table, FS_COLUMNS)?;
        Self::new(
            table
                .rows()
                .iter()
                .map(|row| FsAction::decode(row))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Table(Table::new(
            keys(FS_COLUMNS)?,
            self.actions.iter().map(FsAction::encode).collect(),
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        if self.actions.is_empty() || self.actions.len() > MAX_FS_ACTIONS {
            return Err(RequestError::InvalidLimit("a"));
        }
        if !self.actions.windows(2).all(|pair| pair[0].id < pair[1].id) {
            return Err(RequestError::UnexpectedValue("i"));
        }
        let mut paths = HashSet::new();
        let mut inline_bytes = 0_usize;
        for action in &self.actions {
            action.validate()?;
            if !paths.insert(action.path.as_str())
                || action
                    .destination
                    .as_deref()
                    .is_some_and(|destination| !paths.insert(destination))
            {
                return Err(RequestError::UnexpectedValue("p"));
            }
            if let Some(PatchContent::Inline(content)) = &action.content {
                inline_bytes = inline_bytes
                    .checked_add(content.len())
                    .ok_or(RequestError::InvalidLimit("v"))?;
            }
        }
        if inline_bytes > MAX_PATCH_INLINE_BYTES {
            Err(RequestError::InvalidLimit("v"))
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn actions(&self) -> &[FsAction] {
        &self.actions
    }
}

/// One typed node in a batch dependency graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchNode {
    id: u64,
    dependencies: Vec<u64>,
    arguments: Box<Arguments>,
}

impl BatchNode {
    pub fn new(
        id: u64,
        dependencies: Vec<u64>,
        arguments: Arguments,
    ) -> Result<Self, RequestError> {
        let node = Self {
            id,
            dependencies,
            arguments: Box::new(arguments),
        };
        node.validate()?;
        Ok(node)
    }

    fn decode(row: &[Cell]) -> Result<Self, RequestError> {
        let id = unsigned_cell(&row[0], "i")?;
        let dependencies = unsigned_vector(&row[1], "d")?;
        let operation_text = text_cell(&row[2], "o")?;
        let operation = operation_text
            .as_bytes()
            .first()
            .copied()
            .filter(|_| operation_text.len() == 1)
            .and_then(Operation::from_id)
            .ok_or(RequestError::UnsupportedOperation)?;
        if matches!(operation, Operation::Batch | Operation::Cancel) {
            return Err(RequestError::UnsupportedOperation);
        }
        let payload = text_cell(&row[3], "a")?;
        validate_bounded_text(payload, "a", 1024 * 1024)?;
        let document = crate::ason::decode(payload).map_err(|_| RequestError::InvalidText("a"))?;
        if document.encode() != payload {
            return Err(RequestError::InvalidText("a"));
        }
        expect_fields(&document, &["a"])?;
        let arguments = Arguments::decode(
            operation,
            document.get("a").ok_or(RequestError::InvalidText("a"))?,
        )?;
        Self::new(id, dependencies, arguments)
    }

    fn encode(&self) -> Result<Vec<Cell>, BuildError> {
        let payload =
            Document::new(vec![Field::new(Key::new("a")?, self.arguments.encode()?)])?.encode();
        Ok(vec![
            unsigned_value(self.id),
            Cell::Vector(
                self.dependencies
                    .iter()
                    .map(|dependency| Atom::text(dependency.to_string()))
                    .collect(),
            ),
            text_value(&char::from(self.arguments.operation().id()).to_string()),
            text_value(&payload),
        ])
    }

    fn validate(&self) -> Result<(), RequestError> {
        if self.id == 0 {
            return Err(RequestError::InvalidUnsigned("i"));
        }
        if self.dependencies.len() > MAX_BATCH_EDGES
            || !self.dependencies.windows(2).all(|pair| pair[0] < pair[1])
            || self
                .dependencies
                .iter()
                .any(|dependency| *dependency == 0 || *dependency == self.id)
        {
            return Err(RequestError::UnexpectedValue("d"));
        }
        if matches!(
            self.arguments.operation(),
            Operation::Batch | Operation::Cancel
        ) {
            return Err(RequestError::UnsupportedOperation);
        }
        self.arguments.validate()
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub fn dependencies(&self) -> &[u64] {
        &self.dependencies
    }

    #[must_use]
    pub fn arguments(&self) -> &Arguments {
        &self.arguments
    }
}

/// A validated, acyclic batch graph in stable node-id order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchArgs {
    nodes: Vec<BatchNode>,
}

impl BatchArgs {
    pub fn new(nodes: Vec<BatchNode>) -> Result<Self, RequestError> {
        let arguments = Self { nodes };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(table: &Table) -> Result<Self, RequestError> {
        expect_table_columns(table, BATCH_COLUMNS)?;
        let nodes = table
            .rows()
            .iter()
            .map(|row| BatchNode::decode(row))
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(nodes)
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Table(Table::new(
            keys(BATCH_COLUMNS)?,
            self.nodes
                .iter()
                .map(BatchNode::encode)
                .collect::<Result<Vec<_>, _>>()?,
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        if self.nodes.is_empty() || self.nodes.len() > MAX_BATCH_NODES {
            return Err(RequestError::InvalidLimit("a"));
        }
        if !self.nodes.windows(2).all(|pair| pair[0].id < pair[1].id) {
            return Err(RequestError::UnexpectedValue("i"));
        }
        let positions = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect::<HashMap<_, _>>();
        let mut edge_count = 0_usize;
        let mut incoming = Vec::with_capacity(self.nodes.len());
        let mut dependents = vec![Vec::new(); self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            node.validate()?;
            edge_count = edge_count
                .checked_add(node.dependencies.len())
                .ok_or(RequestError::InvalidLimit("d"))?;
            if edge_count > MAX_BATCH_EDGES {
                return Err(RequestError::InvalidLimit("d"));
            }
            incoming.push(node.dependencies.len());
            for dependency in &node.dependencies {
                let dependency_index = positions
                    .get(dependency)
                    .copied()
                    .ok_or(RequestError::UnexpectedValue("d"))?;
                dependents[dependency_index].push(index);
            }
        }
        let mut ready = incoming
            .iter()
            .enumerate()
            .filter_map(|(index, incoming)| (*incoming == 0).then_some(index))
            .collect::<VecDeque<_>>();
        let mut visited = 0_usize;
        while let Some(index) = ready.pop_front() {
            visited += 1;
            for dependent in &dependents[index] {
                incoming[*dependent] -= 1;
                if incoming[*dependent] == 0 {
                    ready.push_back(*dependent);
                }
            }
        }
        if visited != self.nodes.len() {
            return Err(RequestError::UnexpectedValue("d"));
        }
        Ok(())
    }

    #[must_use]
    pub fn nodes(&self) -> &[BatchNode] {
        &self.nodes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotMode {
    Capture,
    Delta,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotArgs {
    paths: Vec<String>,
    depth: u16,
    mode: SnapshotMode,
    baseline: Option<u64>,
    flags: u32,
}

impl SnapshotArgs {
    pub fn new(
        paths: Vec<String>,
        depth: u16,
        mode: SnapshotMode,
        baseline: Option<u64>,
        flags: u32,
    ) -> Result<Self, RequestError> {
        let arguments = Self {
            paths,
            depth,
            mode,
            baseline,
            flags,
        };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        expect_columns(record, SNAPSHOT_COLUMNS)?;
        let values = record.values();
        let mode = match unsigned_cell(&values[2], "m")? {
            0 => SnapshotMode::Capture,
            1 => SnapshotMode::Delta,
            _ => return Err(RequestError::UnexpectedValue("m")),
        };
        Self::new(
            text_vector(&values[0], "p")?,
            narrow_u16(unsigned_cell(&values[1], "d")?, "d")?,
            mode,
            optional_reference_cell(&values[3], "r")?,
            narrow_u32(unsigned_cell(&values[4], "f")?, "f")?,
        )
    }

    fn encode(&self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(SNAPSHOT_COLUMNS)?,
            vec![
                text_vector_value(&self.paths),
                unsigned_value(u64::from(self.depth)),
                unsigned_value(match self.mode {
                    SnapshotMode::Capture => 0,
                    SnapshotMode::Delta => 1,
                }),
                self.baseline.map_or_else(null_value, |reference| {
                    Cell::Atom(Atom::reference(reference))
                }),
                unsigned_value(u64::from(self.flags)),
            ],
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        validate_paths(&self.paths)?;
        if !self
            .paths
            .windows(2)
            .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
        {
            return Err(RequestError::UnexpectedValue("p"));
        }
        if self.depth > 64 {
            return Err(RequestError::InvalidLimit("d"));
        }
        match (self.mode, self.baseline) {
            (SnapshotMode::Capture, None) | (SnapshotMode::Delta, Some(1..)) => {}
            (SnapshotMode::Capture, Some(_)) | (SnapshotMode::Delta, None | Some(0)) => {
                return Err(RequestError::UnexpectedValue("r"));
            }
        }
        validate_flags(self.flags, SNAPSHOT_FLAGS)
    }

    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    #[must_use]
    pub const fn depth(&self) -> u16 {
        self.depth
    }

    #[must_use]
    pub const fn mode(&self) -> SnapshotMode {
        self.mode
    }

    #[must_use]
    pub const fn baseline(&self) -> Option<u64> {
        self.baseline
    }

    #[must_use]
    pub const fn flags(&self) -> u32 {
        self.flags
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefFormula {
    Bytes {
        offset: u64,
        length: u64,
    },
    Lines {
        offset: u64,
        length: u64,
    },
    Search {
        offset: u64,
        length: u64,
        query: String,
        flags: u32,
    },
    Release,
    Project {
        table: String,
        offset: u64,
        length: u64,
        columns: Vec<String>,
    },
    Materialize {
        path: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefArgs {
    reference: u64,
    formula: RefFormula,
}

impl RefArgs {
    fn new(reference: u64, formula: RefFormula) -> Result<Self, RequestError> {
        let arguments = Self { reference, formula };
        arguments.validate()?;
        Ok(arguments)
    }

    pub fn bytes(reference: u64, offset: u64, length: u64) -> Result<Self, RequestError> {
        Self::new(reference, RefFormula::Bytes { offset, length })
    }

    pub fn lines(reference: u64, offset: u64, length: u64) -> Result<Self, RequestError> {
        Self::new(reference, RefFormula::Lines { offset, length })
    }

    pub fn search(
        reference: u64,
        offset: u64,
        length: u64,
        query: impl Into<String>,
        flags: u32,
    ) -> Result<Self, RequestError> {
        Self::new(
            reference,
            RefFormula::Search {
                offset,
                length,
                query: query.into(),
                flags,
            },
        )
    }

    pub fn release(reference: u64) -> Result<Self, RequestError> {
        Self::new(reference, RefFormula::Release)
    }

    pub fn project(
        reference: u64,
        table: impl Into<String>,
        offset: u64,
        length: u64,
        columns: Vec<String>,
    ) -> Result<Self, RequestError> {
        Self::new(
            reference,
            RefFormula::Project {
                table: table.into(),
                offset,
                length,
                columns,
            },
        )
    }

    pub fn materialize(reference: u64, path: impl Into<String>) -> Result<Self, RequestError> {
        Self::new(reference, RefFormula::Materialize { path: path.into() })
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        if record.columns().len() != 1 || record.values().len() != 1 {
            return Err(RequestError::Columns);
        }
        let Cell::Vector(values) = &record.values()[0] else {
            return Err(RequestError::ExpectedVector("a"));
        };
        let reference =
            reference_formula_atom(values.first().ok_or(RequestError::UnexpectedValue("a"))?)?;
        let formula = match record.columns()[0].as_str() {
            "b" => {
                expect_formula_width(values, 3)?;
                RefFormula::Bytes {
                    offset: unsigned_atom(&values[1], "o")?,
                    length: unsigned_atom(&values[2], "n")?,
                }
            }
            "l" => {
                expect_formula_width(values, 3)?;
                RefFormula::Lines {
                    offset: unsigned_atom(&values[1], "o")?,
                    length: unsigned_atom(&values[2], "n")?,
                }
            }
            "g" => {
                expect_formula_width(values, 5)?;
                RefFormula::Search {
                    offset: unsigned_atom(&values[1], "o")?,
                    length: unsigned_atom(&values[2], "n")?,
                    query: text_formula_atom(&values[3], "q")?.to_owned(),
                    flags: narrow_u32(unsigned_atom(&values[4], "f")?, "f")?,
                }
            }
            "d" => {
                expect_formula_width(values, 1)?;
                RefFormula::Release
            }
            "p" => {
                if values.len() < 5 {
                    return Err(RequestError::UnexpectedValue("a"));
                }
                RefFormula::Project {
                    table: text_formula_atom(&values[1], "t")?.to_owned(),
                    offset: unsigned_atom(&values[2], "o")?,
                    length: unsigned_atom(&values[3], "n")?,
                    columns: values[4..]
                        .iter()
                        .map(|value| text_formula_atom(value, "c").map(str::to_owned))
                        .collect::<Result<_, _>>()?,
                }
            }
            "w" => {
                expect_formula_width(values, 2)?;
                RefFormula::Materialize {
                    path: text_formula_atom(&values[1], "p")?.to_owned(),
                }
            }
            _ => return Err(RequestError::UnexpectedValue("a")),
        };
        Self::new(reference, formula)
    }

    fn encode(&self) -> Result<Value, BuildError> {
        let (operator, mut values) = match &self.formula {
            RefFormula::Bytes { offset, length } => (
                "b",
                vec![
                    Atom::text(offset.to_string()),
                    Atom::text(length.to_string()),
                ],
            ),
            RefFormula::Lines { offset, length } => (
                "l",
                vec![
                    Atom::text(offset.to_string()),
                    Atom::text(length.to_string()),
                ],
            ),
            RefFormula::Search {
                offset,
                length,
                query,
                flags,
            } => (
                "g",
                vec![
                    Atom::text(offset.to_string()),
                    Atom::text(length.to_string()),
                    Atom::text(query),
                    Atom::text(flags.to_string()),
                ],
            ),
            RefFormula::Release => ("d", vec![]),
            RefFormula::Project {
                table,
                offset,
                length,
                columns,
            } => {
                let mut values = Vec::with_capacity(columns.len() + 3);
                values.push(Atom::text(table));
                values.push(Atom::text(offset.to_string()));
                values.push(Atom::text(length.to_string()));
                values.extend(columns.iter().map(Atom::text));
                ("p", values)
            }
            RefFormula::Materialize { path } => ("w", vec![Atom::text(path)]),
        };
        values.insert(0, Atom::reference(self.reference));
        Ok(Value::Record(Record::new(
            vec![Key::new(operator)?],
            vec![Cell::Vector(values)],
        )?))
    }

    fn validate(&self) -> Result<(), RequestError> {
        if self.reference == 0 {
            return Err(RequestError::InvalidUnsigned("r"));
        }
        match &self.formula {
            RefFormula::Bytes { length, .. } => {
                if *length == 0 || *length > MAX_REF_BYTES {
                    return Err(RequestError::InvalidLimit("n"));
                }
            }
            RefFormula::Lines { offset, length } => {
                if *offset == 0 {
                    return Err(RequestError::InvalidLimit("o"));
                }
                if *length == 0 || *length > MAX_REF_LINES {
                    return Err(RequestError::InvalidLimit("n"));
                }
            }
            RefFormula::Search {
                length,
                query,
                flags,
                ..
            } => {
                if *length == 0 || *length > MAX_REF_BYTES {
                    return Err(RequestError::InvalidLimit("n"));
                }
                validate_text(query, "q", 1024 * 1024)?;
                validate_flags(*flags, REF_SEARCH_FLAGS)?;
            }
            RefFormula::Release => {}
            RefFormula::Project {
                table,
                length,
                columns,
                ..
            } => {
                if *length == 0 || *length > MAX_REF_LINES {
                    return Err(RequestError::InvalidLimit("n"));
                }
                validate_text(table, "t", 128)?;
                if Key::new(table).is_err() || columns.is_empty() || columns.len() > 128 {
                    return Err(RequestError::InvalidText("c"));
                }
                let mut unique = HashSet::new();
                for column in columns {
                    validate_text(column, "c", 128)?;
                    if Key::new(column).is_err() || !unique.insert(column.as_str()) {
                        return Err(RequestError::InvalidText("c"));
                    }
                }
            }
            RefFormula::Materialize { path } => {
                validate_text(path, "p", 4096)?;
            }
        }
        Ok(())
    }

    #[must_use]
    pub const fn reference(&self) -> u64 {
        self.reference
    }

    #[must_use]
    pub const fn formula(&self) -> &RefFormula {
        &self.formula
    }
}

fn expect_formula_width(values: &[Atom], expected: usize) -> Result<(), RequestError> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(RequestError::UnexpectedValue("a"))
    }
}

fn reference_formula_atom(value: &Atom) -> Result<u64, RequestError> {
    match value {
        Atom::Reference(reference) if *reference != 0 => Ok(*reference),
        Atom::Null | Atom::Reference(_) | Atom::Text(_) => Err(RequestError::InvalidUnsigned("r")),
    }
}

fn text_formula_atom<'a>(value: &'a Atom, field: &'static str) -> Result<&'a str, RequestError> {
    match value {
        Atom::Text(value) => Ok(value),
        Atom::Null | Atom::Reference(_) => Err(RequestError::InvalidText(field)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelArgs {
    target_id: u64,
}

impl CancelArgs {
    pub fn new(target_id: u64) -> Result<Self, RequestError> {
        let arguments = Self { target_id };
        arguments.validate()?;
        Ok(arguments)
    }

    fn decode(record: &Record) -> Result<Self, RequestError> {
        expect_columns(record, CANCEL_COLUMNS)?;
        Self::new(unsigned_cell(&record.values()[0], "i")?)
    }

    fn encode(self) -> Result<Value, BuildError> {
        Ok(Value::Record(Record::new(
            keys(CANCEL_COLUMNS)?,
            vec![unsigned_value(self.target_id)],
        )?))
    }

    fn validate(self) -> Result<(), RequestError> {
        if self.target_id == 0 {
            Err(RequestError::InvalidUnsigned("i"))
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub const fn target_id(self) -> u64 {
        self.target_id
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RequestError {
    #[error("unexpected top-level field order")]
    Fields,
    #[error("field {0} must be a scalar")]
    ExpectedScalar(&'static str),
    #[error("field {0} must be a record")]
    ExpectedRecord(&'static str),
    #[error("field {0} must be a table")]
    ExpectedTable(&'static str),
    #[error("field {0} must be a vector")]
    ExpectedVector(&'static str),
    #[error("record columns do not match the operation schema")]
    Columns,
    #[error("field {0} is not canonical unsigned decimal")]
    InvalidUnsigned(&'static str),
    #[error("field {0} exceeds its integer type")]
    IntegerRange(&'static str),
    #[error("field {0} has an unexpected value")]
    UnexpectedValue(&'static str),
    #[error("request limit {0} is invalid")]
    InvalidLimit(&'static str),
    #[error("request text field {0} is invalid")]
    InvalidText(&'static str),
    #[error("request contains an unsupported operation")]
    UnsupportedOperation,
}

fn expect_fields(document: &Document, expected: &[&str]) -> Result<(), RequestError> {
    if document.fields().len() == expected.len()
        && document
            .fields()
            .iter()
            .zip(expected)
            .all(|(field, expected)| field.key().as_str() == *expected)
    {
        Ok(())
    } else {
        Err(RequestError::Fields)
    }
}

fn expect_columns(record: &Record, expected: &[&str]) -> Result<(), RequestError> {
    if record.columns().len() == expected.len()
        && record
            .columns()
            .iter()
            .zip(expected)
            .all(|(column, expected)| column.as_str() == *expected)
    {
        Ok(())
    } else {
        Err(RequestError::Columns)
    }
}

fn expect_table_columns(table: &Table, expected: &[&str]) -> Result<(), RequestError> {
    if table.columns().len() == expected.len()
        && table
            .columns()
            .iter()
            .zip(expected)
            .all(|(column, expected)| column.as_str() == *expected)
    {
        Ok(())
    } else {
        Err(RequestError::Columns)
    }
}

fn unsigned(value: Option<&Value>, field: &'static str) -> Result<u64, RequestError> {
    match value {
        Some(Value::Scalar(atom)) => unsigned_atom(atom, field),
        _ => Err(RequestError::ExpectedScalar(field)),
    }
}

fn unsigned_cell(cell: &Cell, field: &'static str) -> Result<u64, RequestError> {
    match cell {
        Cell::Atom(atom) => unsigned_atom(atom, field),
        Cell::Vector(_) => Err(RequestError::ExpectedScalar(field)),
    }
}

fn optional_reference_cell(cell: &Cell, field: &'static str) -> Result<Option<u64>, RequestError> {
    match cell {
        Cell::Atom(Atom::Null) => Ok(None),
        Cell::Atom(Atom::Reference(reference)) if *reference != 0 => Ok(Some(*reference)),
        Cell::Atom(_) | Cell::Vector(_) => Err(RequestError::InvalidUnsigned(field)),
    }
}

fn unsigned_atom(atom: &Atom, field: &'static str) -> Result<u64, RequestError> {
    let Atom::Text(value) = atom else {
        return Err(RequestError::InvalidUnsigned(field));
    };
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(RequestError::InvalidUnsigned(field));
    }
    value
        .parse::<u64>()
        .map_err(|_| RequestError::InvalidUnsigned(field))
}

fn expect_unsigned(
    value: Option<&Value>,
    field: &'static str,
    expected: u64,
) -> Result<(), RequestError> {
    if unsigned(value, field)? == expected {
        Ok(())
    } else {
        Err(RequestError::UnexpectedValue(field))
    }
}

fn text<'a>(value: Option<&'a Value>, field: &'static str) -> Result<&'a str, RequestError> {
    match value {
        Some(Value::Scalar(Atom::Text(value))) => Ok(value),
        _ => Err(RequestError::ExpectedScalar(field)),
    }
}

fn text_cell<'a>(cell: &'a Cell, field: &'static str) -> Result<&'a str, RequestError> {
    match cell {
        Cell::Atom(Atom::Text(value)) => Ok(value),
        Cell::Atom(Atom::Null | Atom::Reference(_)) | Cell::Vector(_) => {
            Err(RequestError::ExpectedScalar(field))
        }
    }
}

fn optional_text_cell(cell: &Cell, field: &'static str) -> Result<Option<String>, RequestError> {
    match cell {
        Cell::Atom(Atom::Null) => Ok(None),
        Cell::Atom(Atom::Text(value)) => Ok(Some(value.clone())),
        Cell::Atom(Atom::Reference(_)) | Cell::Vector(_) => {
            Err(RequestError::ExpectedScalar(field))
        }
    }
}

fn text_vector(cell: &Cell, field: &'static str) -> Result<Vec<String>, RequestError> {
    let Cell::Vector(values) = cell else {
        return Err(RequestError::ExpectedVector(field));
    };
    values
        .iter()
        .map(|value| match value {
            Atom::Text(value) => Ok(value.clone()),
            Atom::Null | Atom::Reference(_) => Err(RequestError::InvalidText(field)),
        })
        .collect()
}

fn atom_vector(cell: &Cell, field: &'static str) -> Result<Vec<Atom>, RequestError> {
    let Cell::Vector(values) = cell else {
        return Err(RequestError::ExpectedVector(field));
    };
    Ok(values.clone())
}

fn unsigned_vector(cell: &Cell, field: &'static str) -> Result<Vec<u64>, RequestError> {
    let Cell::Vector(values) = cell else {
        return Err(RequestError::ExpectedVector(field));
    };
    values
        .iter()
        .map(|value| unsigned_atom(value, field))
        .collect()
}

fn record<'a>(value: Option<&'a Value>, field: &'static str) -> Result<&'a Record, RequestError> {
    match value {
        Some(Value::Record(record)) => Ok(record),
        _ => Err(RequestError::ExpectedRecord(field)),
    }
}

fn narrow_u16(value: u64, field: &'static str) -> Result<u16, RequestError> {
    u16::try_from(value).map_err(|_| RequestError::IntegerRange(field))
}

fn narrow_u32(value: u64, field: &'static str) -> Result<u32, RequestError> {
    u32::try_from(value).map_err(|_| RequestError::IntegerRange(field))
}

fn keys(values: &[&str]) -> Result<Vec<Key>, BuildError> {
    values.iter().map(|value| Key::new(*value)).collect()
}

fn scalar_field(key: &str, value: &str) -> Result<Field, BuildError> {
    Ok(Field::new(Key::new(key)?, Value::Scalar(Atom::text(value))))
}

fn unsigned_value(value: u64) -> Cell {
    Cell::Atom(Atom::text(value.to_string()))
}

fn text_value(value: &str) -> Cell {
    Cell::Atom(Atom::text(value))
}

fn null_value() -> Cell {
    Cell::Atom(Atom::Null)
}

fn text_vector_value(values: &[String]) -> Cell {
    Cell::Vector(values.iter().map(Atom::text).collect())
}

fn validate_text(value: &str, field: &'static str, max: usize) -> Result<(), RequestError> {
    if value.is_empty() {
        return Err(RequestError::InvalidText(field));
    }
    validate_bounded_text(value, field, max)
}

fn validate_bounded_text(value: &str, field: &'static str, max: usize) -> Result<(), RequestError> {
    if value.len() > max || value.contains('\0') {
        Err(RequestError::InvalidText(field))
    } else {
        Ok(())
    }
}

fn validate_text_items(
    values: &[String],
    field: &'static str,
    max_items: usize,
    max_item_bytes: usize,
) -> Result<(), RequestError> {
    if values.len() > max_items {
        return Err(RequestError::InvalidLimit(field));
    }
    for value in values {
        validate_bounded_text(value, field, max_item_bytes)?;
    }
    Ok(())
}

fn validate_paths(paths: &[String]) -> Result<(), RequestError> {
    if paths.is_empty() || paths.len() > 256 {
        return Err(RequestError::InvalidLimit("p"));
    }
    validate_text_items(paths, "p", 256, 4096)?;
    if paths.iter().any(String::is_empty) {
        return Err(RequestError::InvalidText("p"));
    }
    Ok(())
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_flags(flags: u32, allowed: u32) -> Result<(), RequestError> {
    if flags & !allowed == 0 {
        Ok(())
    } else {
        Err(RequestError::UnexpectedValue("f"))
    }
}

fn validate_environment(environment: &[String]) -> Result<(), RequestError> {
    validate_text_items(environment, "e", 256, 32 * 1024)?;
    let mut names = HashSet::new();
    for entry in environment {
        let name = if let Some(name) = entry.strip_prefix('-') {
            name
        } else {
            entry
                .split_once('=')
                .map(|(name, _)| name)
                .ok_or(RequestError::InvalidText("e"))?
        };
        if !valid_environment_name(name) || !names.insert(name) {
            return Err(RequestError::InvalidText("e"));
        }
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests;
