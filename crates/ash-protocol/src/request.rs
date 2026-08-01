//! Typed ASH/1 operation requests and resource budgets.

use std::collections::HashSet;

use thiserror::Error;

use crate::Operation;
use crate::ason::{Atom, BuildError, Cell, Document, Field, Key, Record, Value};

const ENVELOPE_FIELDS: &[&str] = &["t", "i", "o", "a", "u"];
const BUDGET_COLUMNS: &[&str] = &["tok", "rec", "ms"];
const EXEC_COLUMNS: &[&str] = &["x", "v", "c", "e", "in", "f"];
const READ_COLUMNS: &[&str] = &["p", "m", "o", "n"];
const LIST_COLUMNS: &[&str] = &["p", "d", "f"];
const SEARCH_COLUMNS: &[&str] = &["q", "p", "f"];
const CANCEL_COLUMNS: &[&str] = &["i"];

pub const MAX_REQUEST_TOKENS: u32 = 1_048_576;
pub const MAX_REQUEST_RECORDS: u32 = 1_000_000;
pub const MAX_REQUEST_MILLIS: u64 = 86_400_000;
pub const MAX_ARGUMENT_ITEMS: usize = 1024;

pub const EXEC_CLEAR_ENVIRONMENT: u32 = 1 << 0;
pub const LIST_INCLUDE_HIDDEN: u32 = 1 << 0;
pub const LIST_FILES_ONLY: u32 = 1 << 1;
pub const LIST_DIRECTORIES_ONLY: u32 = 1 << 2;
pub const SEARCH_REGEX: u32 = 1 << 0;
pub const SEARCH_CASE_INSENSITIVE: u32 = 1 << 1;
pub const SEARCH_INCLUDE_HIDDEN: u32 = 1 << 2;

const EXEC_FLAGS: u32 = EXEC_CLEAR_ENVIRONMENT;
const LIST_FLAGS: u32 = LIST_INCLUDE_HIDDEN | LIST_FILES_ONLY | LIST_DIRECTORIES_ONLY;
const SEARCH_FLAGS: u32 = SEARCH_REGEX | SEARCH_CASE_INSENSITIVE | SEARCH_INCLUDE_HIDDEN;

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
        budget.validate()?;
        Ok(Self {
            id,
            arguments,
            budget,
        })
    }

    pub fn decode(document: &Document) -> Result<Self, RequestError> {
        expect_fields(document, ENVELOPE_FIELDS)?;
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
        let arguments = Arguments::decode(operation, record(document.get("a"), "a")?)?;
        let budget_record = record(document.get("u"), "u")?;
        expect_columns(budget_record, BUDGET_COLUMNS)?;
        let budget_values = budget_record.values();
        let budget = Budget::new(
            narrow_u32(unsigned_cell(&budget_values[0], "tok")?, "tok")?,
            narrow_u32(unsigned_cell(&budget_values[1], "rec")?, "rec")?,
            unsigned_cell(&budget_values[2], "ms")?,
        )?;
        Self::new(id, arguments, budget)
    }

    /// Extracts a usable request identifier before full schema validation.
    #[must_use]
    pub fn id_hint(document: &Document) -> Option<u64> {
        unsigned(document.get("i"), "i").ok().filter(|id| *id != 0)
    }

    pub fn encode(&self) -> Result<Document, BuildError> {
        let operation = char::from(self.operation().id()).to_string();
        let budget = self.budget;
        Document::new(vec![
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Arguments {
    Exec(ExecArgs),
    Read(ReadArgs),
    List(ListArgs),
    Search(SearchArgs),
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
            Self::Cancel(_) => Operation::Cancel,
        }
    }

    fn decode(operation: Operation, record: &Record) -> Result<Self, RequestError> {
        match operation {
            Operation::Exec => ExecArgs::decode(record).map(Self::Exec),
            Operation::Read => ReadArgs::decode(record).map(Self::Read),
            Operation::List => ListArgs::decode(record).map(Self::List),
            Operation::Search => SearchArgs::decode(record).map(Self::Search),
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
            Self::Cancel(arguments) => arguments.encode(),
        }
    }

    fn validate(&self) -> Result<(), RequestError> {
        match self {
            Self::Exec(arguments) => arguments.validate(),
            Self::Read(arguments) => arguments.validate(),
            Self::List(arguments) => arguments.validate(),
            Self::Search(arguments) => arguments.validate(),
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
