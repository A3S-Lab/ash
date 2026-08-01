use std::collections::HashSet;

use thiserror::Error;

/// An immutable, ordered ASON document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    fields: Vec<Field>,
}

impl Document {
    pub fn new(fields: Vec<Field>) -> Result<Self, BuildError> {
        if fields.is_empty() {
            return Err(BuildError::EmptyDocument);
        }
        let mut names = HashSet::with_capacity(fields.len());
        for field in &fields {
            if !names.insert(field.key.as_str()) {
                return Err(BuildError::DuplicateField(field.key.to_string()));
            }
        }
        Ok(Self { fields })
    }

    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find(|field| field.key.as_str() == key)
            .map(Field::value)
    }
}

/// A named top-level ASON value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    key: Key,
    value: Value,
}

impl Field {
    #[must_use]
    pub const fn new(key: Key, value: Value) -> Self {
        Self { key, value }
    }

    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }
}

/// A syntax-safe ASON field or column identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Key(String);

impl Key {
    pub fn new(value: impl Into<String>) -> Result<Self, BuildError> {
        let value = value.into();
        if !is_valid_key(&value) {
            return Err(BuildError::InvalidKey(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for Key {
    type Error = BuildError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A top-level ASON value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Scalar(Atom),
    Vector(Vec<Atom>),
    Record(Record),
    Table(Table),
}

/// A homogeneous one-row record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    columns: Vec<Key>,
    values: Vec<Cell>,
}

impl Record {
    pub fn new(columns: Vec<Key>, values: Vec<Cell>) -> Result<Self, BuildError> {
        validate_columns(&columns)?;
        if columns.len() != values.len() {
            return Err(BuildError::Width {
                expected: columns.len(),
                actual: values.len(),
            });
        }
        Ok(Self { columns, values })
    }

    #[must_use]
    pub fn columns(&self) -> &[Key] {
        &self.columns
    }

    #[must_use]
    pub fn values(&self) -> &[Cell] {
        &self.values
    }
}

/// A homogeneous multi-row ASON table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Table {
    columns: Vec<Key>,
    rows: Vec<Vec<Cell>>,
}

impl Table {
    pub fn new(columns: Vec<Key>, rows: Vec<Vec<Cell>>) -> Result<Self, BuildError> {
        validate_columns(&columns)?;
        for row in &rows {
            if row.len() != columns.len() {
                return Err(BuildError::Width {
                    expected: columns.len(),
                    actual: row.len(),
                });
            }
        }
        Ok(Self { columns, rows })
    }

    #[must_use]
    pub fn columns(&self) -> &[Key] {
        &self.columns
    }

    #[must_use]
    pub fn rows(&self) -> &[Vec<Cell>] {
        &self.rows
    }
}

/// A record or table cell. Vectors cannot contain other vectors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Cell {
    Atom(Atom),
    Vector(Vec<Atom>),
}

/// An ASON scalar before operation-schema type conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Atom {
    Null,
    Reference(u64),
    Text(String),
}

impl Atom {
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    #[must_use]
    pub const fn reference(id: u64) -> Self {
        Self::Reference(id)
    }
}

impl From<&str> for Atom {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl From<String> for Atom {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BuildError {
    #[error("an ASON document must contain at least one field")]
    EmptyDocument,
    #[error("invalid ASON key: {0}")]
    InvalidKey(String),
    #[error("duplicate top-level field: {0}")]
    DuplicateField(String),
    #[error("a record or table must contain at least one column")]
    EmptyColumns,
    #[error("duplicate record or table column: {0}")]
    DuplicateColumn(String),
    #[error("row width mismatch: expected {expected}, got {actual}")]
    Width { expected: usize, actual: usize },
}

pub(crate) fn is_valid_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn validate_columns(columns: &[Key]) -> Result<(), BuildError> {
    if columns.is_empty() {
        return Err(BuildError::EmptyColumns);
    }
    let mut names = HashSet::with_capacity(columns.len());
    for column in columns {
        if !names.insert(column.as_str()) {
            return Err(BuildError::DuplicateColumn(column.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Atom, BuildError, Document, Field, Key, Record, Value};

    #[test]
    fn model_rejects_duplicate_fields_and_columns() {
        let key = Key::new("x").expect("valid key");
        let field = Field::new(key.clone(), Value::Scalar(Atom::Null));
        assert!(matches!(
            Document::new(vec![field.clone(), field]),
            Err(BuildError::DuplicateField(_))
        ));

        assert!(matches!(
            Record::new(vec![key.clone(), key], vec![]),
            Err(BuildError::DuplicateColumn(_))
        ));
    }

    #[test]
    fn key_grammar_is_delimiter_safe() {
        for valid in ["t", "field_2", "error.code", "path-id"] {
            assert!(Key::new(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "2field", "a,b", "a:b", "a/b", "é"] {
            assert!(Key::new(invalid).is_err(), "{invalid}");
        }
    }
}
