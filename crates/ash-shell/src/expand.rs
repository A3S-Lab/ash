use std::borrow::Cow;
use std::ffi::{OsStr, OsString};

use crate::{Parameter, QuoteMode, ShellState, SourceSpan, Word, WordPart};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExpandedWord {
    value: OsString,
    span: SourceSpan,
    pathname_segments: Vec<PathnameSegment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PathnameSegment {
    value: OsString,
    active: bool,
}

impl PathnameSegment {
    pub(crate) fn value(&self) -> &OsStr {
        &self.value
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ExpandedFieldBuilder {
    value: OsString,
    pathname_segments: Vec<PathnameSegment>,
    present: bool,
}

impl ExpandedFieldBuilder {
    fn append(&mut self, value: &OsStr, pathname_active: bool) {
        if value.is_empty() {
            return;
        }
        self.value.push(value);
        if let Some(segment) = self
            .pathname_segments
            .last_mut()
            .filter(|segment| segment.active == pathname_active)
        {
            segment.value.push(value);
        } else {
            self.pathname_segments.push(PathnameSegment {
                value: value.to_owned(),
                active: pathname_active,
            });
        }
        self.present = true;
    }

    const fn mark_present(&mut self) {
        self.present = true;
    }

    fn take(&mut self) -> Option<(OsString, Vec<PathnameSegment>)> {
        if !self.present {
            return None;
        }
        self.present = false;
        Some((
            std::mem::take(&mut self.value),
            std::mem::take(&mut self.pathname_segments),
        ))
    }
}

impl ExpandedWord {
    pub(crate) fn into_value(self) -> OsString {
        self.value
    }

    pub(crate) fn value(&self) -> &OsStr {
        &self.value
    }

    pub(crate) const fn span(&self) -> SourceSpan {
        self.span
    }

    pub(crate) fn pathname_segments(&self) -> &[PathnameSegment] {
        &self.pathname_segments
    }

    pub(crate) fn from_pathname(value: OsString, span: SourceSpan) -> Self {
        Self {
            value,
            span,
            pathname_segments: Vec::new(),
        }
    }
}

/// Expands syntax words without losing native environment-string values.
///
/// Only unquoted parameter and command-substitution values participate in the
/// current fixed field splitting contract. Literal source characters and
/// quoted values remain in the field in which they appeared.
#[cfg(test)]
fn expand_words(words: &[Word], state: &ShellState) -> Vec<ExpandedWord> {
    words
        .iter()
        .flat_map(|word| expand_word_with_substitutions(word, state, std::iter::empty()))
        .collect()
}

pub(crate) fn expand_word_with_substitutions(
    word: &Word,
    state: &ShellState,
    substitution_values: impl IntoIterator<Item = OsString>,
) -> Vec<ExpandedWord> {
    let mut fields = Vec::new();
    let mut current = ExpandedFieldBuilder::default();
    let mut substitution_values = substitution_values.into_iter();

    for part in word.parts() {
        match part {
            WordPart::Literal { value, quote, .. } => {
                current.append(OsStr::new(value), matches!(quote, QuoteMode::Unquoted));
                if value.is_empty() && *quote != QuoteMode::Unquoted {
                    current.mark_present();
                }
            }
            WordPart::EscapedLiteral { value, .. } => {
                current.append(OsStr::new(value), false);
            }
            WordPart::Parameter {
                parameter, quote, ..
            } => {
                let value = parameter_value(parameter, state);
                if *quote == QuoteMode::Unquoted {
                    append_split_value(value.as_ref(), &mut fields, &mut current);
                } else {
                    current.append(value.as_ref(), false);
                    current.mark_present();
                }
            }
            WordPart::CommandSubstitution { quote, .. } => {
                let value = substitution_values
                    .next()
                    .expect("every command substitution has one execution value");
                if *quote == QuoteMode::Unquoted {
                    append_split_value(&value, &mut fields, &mut current);
                } else {
                    current.append(&value, false);
                    current.mark_present();
                }
            }
        }
    }
    debug_assert!(
        substitution_values.next().is_none(),
        "command substitution values align with word parts"
    );

    finish_field(&mut fields, &mut current);
    fields
        .into_iter()
        .map(|(value, pathname_segments)| ExpandedWord {
            value,
            span: word.span(),
            pathname_segments,
        })
        .collect()
}

fn parameter_value<'a>(parameter: &Parameter, state: &'a ShellState) -> Cow<'a, OsStr> {
    match parameter {
        Parameter::Variable { name, .. } => state
            .variable(name)
            .or_else(|| state.environment().get(name))
            .map_or_else(|| Cow::Borrowed(OsStr::new("")), Cow::Borrowed),
        Parameter::LastStatus => Cow::Owned(OsString::from(state.last_status().code().to_string())),
    }
}

fn finish_field(
    fields: &mut Vec<(OsString, Vec<PathnameSegment>)>,
    current: &mut ExpandedFieldBuilder,
) {
    if let Some(field) = current.take() {
        fields.push(field);
    }
}

const fn is_field_separator(value: u32) -> bool {
    matches!(value, 0x20 | 0x09 | 0x0a)
}

#[cfg(unix)]
fn append_split_value(
    value: &OsStr,
    fields: &mut Vec<(OsString, Vec<PathnameSegment>)>,
    current: &mut ExpandedFieldBuilder,
) {
    use std::os::unix::ffi::OsStrExt as _;

    let bytes = value.as_bytes();
    let mut segment_start = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if is_field_separator(u32::from(byte)) {
            if segment_start < index {
                current.append(OsStr::from_bytes(&bytes[segment_start..index]), true);
            }
            finish_field(fields, current);
            segment_start = index + 1;
        }
    }
    if segment_start < bytes.len() {
        current.append(OsStr::from_bytes(&bytes[segment_start..]), true);
    }
}

#[cfg(windows)]
fn append_split_value(
    value: &OsStr,
    fields: &mut Vec<(OsString, Vec<PathnameSegment>)>,
    current: &mut ExpandedFieldBuilder,
) {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    let units: Vec<u16> = value.encode_wide().collect();
    let mut segment_start = 0;
    for (index, unit) in units.iter().copied().enumerate() {
        if is_field_separator(u32::from(unit)) {
            if segment_start < index {
                current.append(&OsString::from_wide(&units[segment_start..index]), true);
            }
            finish_field(fields, current);
            segment_start = index + 1;
        }
    }
    if segment_start < units.len() {
        current.append(&OsString::from_wide(&units[segment_start..]), true);
    }
}

#[cfg(not(any(unix, windows)))]
fn append_split_value(
    value: &OsStr,
    fields: &mut Vec<(OsString, Vec<PathnameSegment>)>,
    current: &mut ExpandedFieldBuilder,
) {
    let value = value.to_string_lossy();
    let mut segment_start = 0;
    for (index, character) in value.char_indices() {
        if is_field_separator(u32::from(character)) {
            if segment_start < index {
                current.append(OsStr::new(&value[segment_start..index]), true);
            }
            finish_field(fields, current);
            segment_start = index + character.len_utf8();
        }
    }
    if segment_start < value.len() {
        current.append(OsStr::new(&value[segment_start..]), true);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::expand_words;
    use crate::{ExecutionBackend, ShellState, ShellStatus, ShellStatusKind, parse};

    fn expanded(source: &str, state: &ShellState) -> Vec<OsString> {
        let script = parse(source).expect("parse expansion fixture");
        expand_words(script.commands()[0].words(), state)
            .into_iter()
            .map(super::ExpandedWord::into_value)
            .collect()
    }

    #[test]
    fn expands_parameters_with_quote_aware_field_boundaries() {
        let mut state = ShellState::new(".");
        state
            .set_variable("VALUE", " alpha  beta ")
            .expect("variable");
        state.set_last_status(ShellStatus::new(
            23,
            ShellStatusKind::Exited,
            None,
            ExecutionBackend::Native,
        ));

        let words = expanded(
            "tool pre${VALUE}post \"$VALUE\" '$VALUE' \\$VALUE $MISSING \"$MISSING\" $?",
            &state,
        );
        assert_eq!(
            words,
            [
                OsString::from("tool"),
                OsString::from("pre"),
                OsString::from("alpha"),
                OsString::from("beta"),
                OsString::from("post"),
                OsString::from(" alpha  beta "),
                OsString::from("$VALUE"),
                OsString::from("$VALUE"),
                OsString::new(),
                OsString::from("23"),
            ]
        );
    }

    #[test]
    fn shell_variables_precede_host_aware_environment_fallback() {
        let mut state = ShellState::new(".");
        state.environment_mut().insert("TOKEN", "environment");
        assert_eq!(
            expanded("echo $TOKEN", &state),
            [OsString::from("echo"), OsString::from("environment")]
        );

        state.set_variable("TOKEN", "variable").expect("variable");
        assert_eq!(
            expanded("echo $TOKEN", &state),
            [OsString::from("echo"), OsString::from("variable")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_field_splitting_preserves_non_utf8_native_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStringExt as _;

        let mut state = ShellState::new(".");
        state
            .set_variable("TOKEN", OsString::from_vec(vec![b'a', 0xff, b' ', b'b']))
            .expect("native variable");
        let words = expanded("tool $TOKEN \"$TOKEN\"", &state);
        assert_eq!(
            words,
            [
                OsString::from("tool"),
                OsString::from_vec(vec![b'a', 0xff]),
                OsString::from("b"),
                OsString::from_vec(vec![b'a', 0xff, b' ', b'b']),
            ]
        );
        assert_ne!(words[1].as_os_str(), OsStr::new("a"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_field_splitting_preserves_unpaired_native_units() {
        use std::os::windows::ffi::OsStringExt as _;

        let mut state = ShellState::new(".");
        state
            .set_variable(
                "TOKEN",
                OsString::from_wide(&[u16::from(b'a'), 0xd800, 0x20, u16::from(b'b')]),
            )
            .expect("native variable");
        let words = expanded("tool $TOKEN \"$TOKEN\"", &state);
        assert_eq!(words.len(), 4);
        assert_eq!(words[1], OsString::from_wide(&[u16::from(b'a'), 0xd800]));
        assert_eq!(words[2], OsString::from("b"));
        assert_eq!(
            words[3],
            OsString::from_wide(&[u16::from(b'a'), 0xd800, 0x20, u16::from(b'b')])
        );
    }
}
