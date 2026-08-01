use std::collections::HashSet;

use super::error::{DecodeError, LimitKind};
use super::model::{Atom, Cell, Document, Field, Key, Record, Table, Value};

/// Resource ceilings applied while parsing one ASON document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_fields: usize,
    pub max_key_bytes: usize,
    pub max_columns: usize,
    pub max_rows: usize,
    pub max_vector_items: usize,
    pub max_scalar_bytes: usize,
    pub max_values: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_lines: 262_144,
            max_fields: 256,
            max_key_bytes: 128,
            max_columns: 128,
            max_rows: 100_000,
            max_vector_items: 4_096,
            max_scalar_bytes: 1024 * 1024,
            max_values: 1_000_000,
        }
    }
}

pub fn decode(input: &str, limits: &Limits) -> Result<Document, DecodeError> {
    Parser::new(limits).document(input)
}

struct Parser<'a> {
    limits: &'a Limits,
    values: usize,
}

impl<'a> Parser<'a> {
    const fn new(limits: &'a Limits) -> Self {
        Self { limits, values: 0 }
    }

    fn document(mut self, input: &str) -> Result<Document, DecodeError> {
        self.enforce(input.len(), self.limits.max_bytes, LimitKind::Bytes, 1)?;
        if let Some(offset) = input.as_bytes().iter().position(|byte| *byte == b'\r') {
            return Err(DecodeError::CarriageReturn { offset });
        }
        let Some(body) = input.strip_suffix('\n') else {
            return Err(DecodeError::MissingFinalLf);
        };
        if body.is_empty() {
            return Err(DecodeError::EmptyDocument);
        }

        let line_count = input.bytes().filter(|byte| *byte == b'\n').count();
        self.enforce(line_count, self.limits.max_lines, LimitKind::Lines, 1)?;

        let mut fields = Vec::new();
        let mut field_names = HashSet::new();
        let mut lines = body.split('\n').enumerate();
        while let Some((line_index, line)) = lines.next() {
            let line_number = line_index + 1;
            if line.is_empty() {
                return Err(DecodeError::BlankLine { line: line_number });
            }
            self.enforce(
                fields.len() + 1,
                self.limits.max_fields,
                LimitKind::Fields,
                line_number,
            )?;

            let (head, payload) = line.split_once(':').ok_or(DecodeError::Syntax {
                line: line_number,
                column: line.len() + 1,
                message: "top-level field is missing ':'",
            })?;

            let (key, value) = if head.contains('{') || head.contains('[') {
                if !payload.is_empty() {
                    return Err(DecodeError::Syntax {
                        line: line_number,
                        column: head.len() + 2,
                        message: "record and table headers cannot contain an inline value",
                    });
                }
                let header = self.header(head, line_number)?;
                let row_count = header.rows.unwrap_or(1);
                self.enforce(
                    row_count,
                    self.limits.max_rows,
                    LimitKind::Rows,
                    line_number,
                )?;
                let mut rows = Vec::with_capacity(row_count.min(1024));
                for consumed in 0..row_count {
                    let Some((row_index, row)) = lines.next() else {
                        return Err(DecodeError::MissingRows {
                            line: line_number,
                            declared: row_count,
                            available: consumed,
                        });
                    };
                    let row_number = row_index + 1;
                    if row.is_empty() {
                        return Err(DecodeError::BlankLine { line: row_number });
                    }
                    rows.push(self.row(row, header.columns.len(), row_number)?);
                }

                let value = if header.rows.is_some() {
                    Value::Table(Table::new(header.columns, rows).map_err(|_| {
                        DecodeError::Syntax {
                            line: line_number,
                            column: 1,
                            message: "invalid table model",
                        }
                    })?)
                } else {
                    let values = rows.pop().ok_or(DecodeError::MissingRows {
                        line: line_number,
                        declared: 1,
                        available: 0,
                    })?;
                    Value::Record(Record::new(header.columns, values).map_err(|_| {
                        DecodeError::Syntax {
                            line: line_number,
                            column: 1,
                            message: "invalid record model",
                        }
                    })?)
                };
                (header.key, value)
            } else {
                let key = self.key(head, line_number)?;
                let value = if payload.starts_with('[') {
                    Value::Vector(self.vector(payload, line_number, head.len() + 2)?)
                } else {
                    Value::Scalar(self.atom(payload, line_number, head.len() + 2)?)
                };
                (key, value)
            };

            if !field_names.insert(key.to_string()) {
                return Err(DecodeError::DuplicateField {
                    line: line_number,
                    key: key.to_string(),
                });
            }
            fields.push(Field::new(key, value));
        }

        Document::new(fields).map_err(|_| DecodeError::EmptyDocument)
    }

    fn header(&self, head: &str, line: usize) -> Result<Header, DecodeError> {
        if !head.ends_with('}') {
            return Err(self.syntax(line, 1, "record or table header must end with '}'"));
        }
        let open = head.find('{').ok_or_else(|| {
            self.syntax(
                line,
                1,
                "record or table header is missing an opening brace",
            )
        })?;
        let prefix = &head[..open];
        let column_source = &head[open + 1..head.len() - 1];
        let (key_source, rows) = if prefix.ends_with(']') {
            let bracket = prefix.rfind('[').ok_or_else(|| {
                self.syntax(line, 1, "table header is missing an opening bracket")
            })?;
            let count = &prefix[bracket + 1..prefix.len() - 1];
            (&prefix[..bracket], Some(parse_row_count(count, line)?))
        } else {
            if prefix.contains('[') || prefix.contains(']') {
                return Err(self.syntax(line, 1, "malformed table row count"));
            }
            (prefix, None)
        };

        let key = self.key(key_source, line)?;
        let columns = self.columns(column_source, line)?;
        Ok(Header { key, columns, rows })
    }

    fn columns(&self, source: &str, line: usize) -> Result<Vec<Key>, DecodeError> {
        if source.is_empty() {
            return Err(DecodeError::EmptyColumns { line });
        }
        let mut seen = HashSet::new();
        let mut columns = Vec::new();
        for value in source.split(',') {
            self.enforce(
                columns.len() + 1,
                self.limits.max_columns,
                LimitKind::Columns,
                line,
            )?;
            let key = self.key(value, line)?;
            if !seen.insert(key.to_string()) {
                return Err(DecodeError::DuplicateColumn {
                    line,
                    key: key.to_string(),
                });
            }
            columns.push(key);
        }
        Ok(columns)
    }

    fn key(&self, source: &str, line: usize) -> Result<Key, DecodeError> {
        self.enforce(
            source.len(),
            self.limits.max_key_bytes,
            LimitKind::KeyBytes,
            line,
        )?;
        Key::new(source).map_err(|_| DecodeError::InvalidKey {
            line,
            key: source.to_owned(),
        })
    }

    fn row(&mut self, source: &str, width: usize, line: usize) -> Result<Vec<Cell>, DecodeError> {
        let raw = split_items(source, true, line, ItemLimit::Row(width))?;
        if raw.len() != width {
            return Err(DecodeError::RowWidth {
                line,
                expected: width,
                actual: raw.len(),
            });
        }
        raw.into_iter()
            .map(|(column, value)| self.cell(value, line, column))
            .collect()
    }

    fn cell(&mut self, source: &str, line: usize, column: usize) -> Result<Cell, DecodeError> {
        if source.starts_with('[') {
            Ok(Cell::Vector(self.vector(source, line, column)?))
        } else {
            Ok(Cell::Atom(self.atom(source, line, column)?))
        }
    }

    fn vector(
        &mut self,
        source: &str,
        line: usize,
        column: usize,
    ) -> Result<Vec<Atom>, DecodeError> {
        if !source.starts_with('[') || !source.ends_with(']') {
            return Err(self.syntax(line, column, "vector must be enclosed by '[' and ']'"));
        }
        let inner = &source[1..source.len() - 1];
        if inner.is_empty() {
            return Ok(Vec::new());
        }
        let raw = split_items(
            inner,
            false,
            line,
            ItemLimit::Vector(self.limits.max_vector_items),
        )?;
        raw.into_iter()
            .map(|(offset, value)| self.atom(value, line, column + offset))
            .collect()
    }

    fn atom(&mut self, source: &str, line: usize, column: usize) -> Result<Atom, DecodeError> {
        self.bump_value(line)?;
        super::scalar::decode(source, line, column, self.limits.max_scalar_bytes)
    }

    fn bump_value(&mut self, line: usize) -> Result<(), DecodeError> {
        self.enforce(
            self.values + 1,
            self.limits.max_values,
            LimitKind::Values,
            line,
        )?;
        self.values += 1;
        Ok(())
    }

    fn enforce(
        &self,
        actual: usize,
        max: usize,
        kind: LimitKind,
        line: usize,
    ) -> Result<(), DecodeError> {
        if actual > max {
            Err(DecodeError::Limit { kind, max, line })
        } else {
            Ok(())
        }
    }

    const fn syntax(&self, line: usize, column: usize, message: &'static str) -> DecodeError {
        DecodeError::Syntax {
            line,
            column,
            message,
        }
    }
}

struct Header {
    key: Key,
    columns: Vec<Key>,
    rows: Option<usize>,
}

fn parse_row_count(source: &str, line: usize) -> Result<usize, DecodeError> {
    if source.is_empty()
        || !source.bytes().all(|byte| byte.is_ascii_digit())
        || (source.len() > 1 && source.starts_with('0'))
    {
        return Err(DecodeError::InvalidRowCount { line });
    }
    source
        .parse::<usize>()
        .map_err(|_| DecodeError::InvalidRowCount { line })
}

fn split_items(
    source: &str,
    allow_vector: bool,
    line: usize,
    limit: ItemLimit,
) -> Result<Vec<(usize, &str)>, DecodeError> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut vector_depth = 0_u8;
    for (offset, character) in source.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '[' if allow_vector && vector_depth == 0 => vector_depth = 1,
            '[' => {
                return Err(DecodeError::Syntax {
                    line,
                    column: offset + 1,
                    message: "nested vectors are not allowed",
                });
            }
            ']' if allow_vector && vector_depth == 1 => vector_depth = 0,
            ']' => {
                return Err(DecodeError::Syntax {
                    line,
                    column: offset + 1,
                    message: "unmatched vector bracket",
                });
            }
            ',' if vector_depth == 0 => {
                push_item(&mut items, (start + 1, &source[start..offset]), limit, line)?;
                start = offset + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return Err(DecodeError::Syntax {
            line,
            column: source.len(),
            message: "unterminated quoted string",
        });
    }
    if vector_depth != 0 {
        return Err(DecodeError::Syntax {
            line,
            column: source.len(),
            message: "unterminated vector",
        });
    }
    push_item(&mut items, (start + 1, &source[start..]), limit, line)?;
    Ok(items)
}

#[derive(Clone, Copy)]
enum ItemLimit {
    Row(usize),
    Vector(usize),
}

fn push_item<'a>(
    items: &mut Vec<(usize, &'a str)>,
    item: (usize, &'a str),
    limit: ItemLimit,
    line: usize,
) -> Result<(), DecodeError> {
    let next = items.len() + 1;
    match limit {
        ItemLimit::Row(expected) if next > expected => {
            return Err(DecodeError::RowWidth {
                line,
                expected,
                actual: next,
            });
        }
        ItemLimit::Vector(max) if next > max => {
            return Err(DecodeError::Limit {
                kind: LimitKind::VectorItems,
                max,
                line,
            });
        }
        ItemLimit::Row(_) | ItemLimit::Vector(_) => {}
    }
    items.push(item);
    Ok(())
}
