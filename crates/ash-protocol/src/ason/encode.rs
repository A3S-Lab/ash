use std::fmt::Write as _;

use super::model::{Atom, Cell, Document, Field, Value};

#[must_use]
pub fn encode(document: &Document) -> String {
    let mut output = String::new();
    for field in document.fields() {
        write_field(&mut output, field);
    }
    output
}

fn write_field(output: &mut String, field: &Field) {
    output.push_str(field.key().as_str());
    match field.value() {
        Value::Scalar(atom) => {
            output.push(':');
            write_atom(output, atom);
            output.push('\n');
        }
        Value::Vector(values) => {
            output.push(':');
            write_vector(output, values);
            output.push('\n');
        }
        Value::Record(record) => {
            write_columns(output, record.columns());
            output.push_str(":\n");
            write_row(output, record.values());
        }
        Value::Table(table) => {
            let _ = write!(output, "[{}]", table.rows().len());
            write_columns(output, table.columns());
            output.push_str(":\n");
            for row in table.rows() {
                write_row(output, row);
            }
        }
    }
}

fn write_columns(output: &mut String, columns: &[super::model::Key]) {
    output.push('{');
    for (index, column) in columns.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str(column.as_str());
    }
    output.push('}');
}

fn write_row(output: &mut String, row: &[Cell]) {
    for (index, cell) in row.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        match cell {
            Cell::Atom(atom) => write_atom(output, atom),
            Cell::Vector(values) => write_vector(output, values),
        }
    }
    output.push('\n');
}

fn write_vector(output: &mut String, values: &[Atom]) {
    output.push('[');
    for (index, atom) in values.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        write_atom(output, atom);
    }
    output.push(']');
}

fn write_atom(output: &mut String, atom: &Atom) {
    match atom {
        Atom::Null => output.push('~'),
        Atom::Reference(id) => {
            output.push('@');
            let _ = write!(output, "{id}");
        }
        Atom::Text(value) if is_bare_text(value) => output.push_str(value),
        Atom::Text(value) => write_quoted(output, value),
    }
}

fn is_bare_text(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'@' | b'+' | b'-')
        })
        && !is_reference_syntax(value)
}

fn is_reference_syntax(value: &str) -> bool {
    value.strip_prefix('@').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn write_quoted(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(output, "\\u{{{:x}}}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::write_quoted;

    #[test]
    fn quoted_strings_use_short_canonical_escapes() {
        let mut output = String::new();
        write_quoted(&mut output, "a\n\t\u{7}\"\\中");
        assert_eq!(output, "\"a\\n\\t\\u{7}\\\"\\\\中\"");
    }
}
