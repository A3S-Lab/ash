use super::error::{DecodeError, LimitKind};
use super::model::Atom;

pub(super) fn decode(
    source: &str,
    line: usize,
    column: usize,
    max_bytes: usize,
) -> Result<Atom, DecodeError> {
    if source.is_empty() {
        return Err(syntax(line, column, "empty scalar must be written as \"\""));
    }
    if source == "~" {
        return Ok(Atom::Null);
    }
    if let Some(digits) = source.strip_prefix('@')
        && !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        if digits.len() > 1 && digits.starts_with('0') {
            return Err(DecodeError::InvalidReference { line, column });
        }
        return digits
            .parse::<u64>()
            .map(Atom::Reference)
            .map_err(|_| DecodeError::InvalidReference { line, column });
    }
    if source.starts_with('"') {
        quoted(source, line, column, max_bytes).map(Atom::Text)
    } else {
        bare(source, line, column, max_bytes)
    }
}

fn bare(source: &str, line: usize, column: usize, max_bytes: usize) -> Result<Atom, DecodeError> {
    enforce_bytes(source.len(), max_bytes, line)?;
    if !source.bytes().all(is_bare_byte) {
        return Err(syntax(line, column, "invalid bare scalar character"));
    }
    Ok(Atom::Text(source.to_owned()))
}

pub(super) const fn is_bare_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'.' | b'/' | b'@' | b'+' | b'-' | b'#' | b'?' | b'|' | b'>'
        )
}

fn quoted(
    source: &str,
    line: usize,
    column: usize,
    max_bytes: usize,
) -> Result<String, DecodeError> {
    let mut output = String::with_capacity(source.len().min(max_bytes));
    let mut chars = source[1..].char_indices();
    while let Some((offset, character)) = chars.next() {
        let current_column = column + offset + 1;
        match character {
            '"' => {
                if offset + character.len_utf8() + 1 != source.len() {
                    return Err(syntax(
                        line,
                        current_column,
                        "characters follow the closing quote",
                    ));
                }
                return Ok(output);
            }
            '\\' => {
                let Some((escape_offset, escape)) = chars.next() else {
                    return Err(invalid_escape(line, current_column, "incomplete escape"));
                };
                match escape {
                    '\\' => output.push('\\'),
                    '"' => output.push('"'),
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    'u' => {
                        let escaped =
                            parse_unicode_escape(&mut chars, line, column + escape_offset + 2)?;
                        output.push(escaped);
                    }
                    _ => return Err(invalid_escape(line, current_column, "unknown escape")),
                }
            }
            character if character.is_control() => {
                return Err(syntax(
                    line,
                    current_column,
                    "control character must be escaped",
                ));
            }
            character => output.push(character),
        }
        enforce_bytes(output.len(), max_bytes, line)?;
    }
    Err(syntax(line, column, "unterminated quoted string"))
}

fn parse_unicode_escape(
    chars: &mut std::str::CharIndices<'_>,
    line: usize,
    column: usize,
) -> Result<char, DecodeError> {
    let Some((_, '{')) = chars.next() else {
        return Err(invalid_escape(
            line,
            column,
            "unicode escape must start with '{'",
        ));
    };
    let mut digits = String::with_capacity(6);
    loop {
        let Some((_, character)) = chars.next() else {
            return Err(invalid_escape(line, column, "unterminated unicode escape"));
        };
        if character == '}' {
            break;
        }
        if !character.is_ascii_hexdigit() || digits.len() == 6 {
            return Err(invalid_escape(
                line,
                column,
                "unicode escape requires one to six hexadecimal digits",
            ));
        }
        digits.push(character);
    }
    if digits.is_empty() {
        return Err(invalid_escape(
            line,
            column,
            "unicode escape cannot be empty",
        ));
    }
    let value = u32::from_str_radix(&digits, 16)
        .map_err(|_| invalid_escape(line, column, "invalid unicode scalar"))?;
    char::from_u32(value).ok_or_else(|| invalid_escape(line, column, "invalid unicode scalar"))
}

fn enforce_bytes(actual: usize, max: usize, line: usize) -> Result<(), DecodeError> {
    if actual > max {
        Err(DecodeError::Limit {
            kind: LimitKind::ScalarBytes,
            max,
            line,
        })
    } else {
        Ok(())
    }
}

const fn syntax(line: usize, column: usize, message: &'static str) -> DecodeError {
    DecodeError::Syntax {
        line,
        column,
        message,
    }
}

const fn invalid_escape(line: usize, column: usize, message: &'static str) -> DecodeError {
    DecodeError::InvalidEscape {
        line,
        column,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::decode;
    use crate::ason::{Atom, DecodeError, LimitKind};

    #[test]
    fn unicode_and_short_escapes_decode() {
        assert_eq!(
            decode("\"a\\n\\u{4e2d}\"", 1, 1, 32).expect("valid string"),
            Atom::text("a\n中")
        );
    }

    #[test]
    fn decoded_limit_applies_after_escape_expansion() {
        assert!(matches!(
            decode("\"\\u{4e2d}\"", 1, 1, 2),
            Err(DecodeError::Limit {
                kind: LimitKind::ScalarBytes,
                max: 2,
                ..
            })
        ));
    }
}
