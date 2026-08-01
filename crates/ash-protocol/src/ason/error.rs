use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    Bytes,
    Lines,
    Fields,
    KeyBytes,
    Columns,
    Rows,
    VectorItems,
    ScalarBytes,
    Values,
}

impl std::fmt::Display for LimitKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Bytes => "document bytes",
            Self::Lines => "document lines",
            Self::Fields => "top-level fields",
            Self::KeyBytes => "key bytes",
            Self::Columns => "columns",
            Self::Rows => "table rows",
            Self::VectorItems => "vector items",
            Self::ScalarBytes => "decoded scalar bytes",
            Self::Values => "total scalar values",
        };
        formatter.write_str(name)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DecodeError {
    #[error("ASON input exceeds the {kind} limit of {max} at line {line}")]
    Limit {
        kind: LimitKind,
        max: usize,
        line: usize,
    },
    #[error("canonical ASON must end with exactly one LF")]
    MissingFinalLf,
    #[error("ASON contains a CR byte at offset {offset}; only LF is allowed")]
    CarriageReturn { offset: usize },
    #[error("an ASON document must contain at least one field")]
    EmptyDocument,
    #[error("blank ASON line at line {line}")]
    BlankLine { line: usize },
    #[error("invalid ASON syntax at line {line}, column {column}: {message}")]
    Syntax {
        line: usize,
        column: usize,
        message: &'static str,
    },
    #[error("invalid ASON key at line {line}: {key}")]
    InvalidKey { line: usize, key: String },
    #[error("duplicate top-level field at line {line}: {key}")]
    DuplicateField { line: usize, key: String },
    #[error("duplicate column at line {line}: {key}")]
    DuplicateColumn { line: usize, key: String },
    #[error("record or table at line {line} must declare at least one column")]
    EmptyColumns { line: usize },
    #[error("table row count at line {line} is not canonical unsigned decimal")]
    InvalidRowCount { line: usize },
    #[error("table at line {line} declares {declared} rows but only {available} remain")]
    MissingRows {
        line: usize,
        declared: usize,
        available: usize,
    },
    #[error("row width mismatch at line {line}: expected {expected}, got {actual}")]
    RowWidth {
        line: usize,
        expected: usize,
        actual: usize,
    },
    #[error("invalid result reference at line {line}, column {column}")]
    InvalidReference { line: usize, column: usize },
    #[error("invalid string escape at line {line}, column {column}: {message}")]
    InvalidEscape {
        line: usize,
        column: usize,
        message: &'static str,
    },
}
