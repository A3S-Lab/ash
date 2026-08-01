//! Canonical Agent Serialized Object Notation (ASON).

mod encode;
mod error;
mod model;
mod parse;
mod scalar;

use std::str::FromStr;

pub use error::{DecodeError, LimitKind};
pub use model::{Atom, BuildError, Cell, Document, Field, Key, Record, Table, Value};
pub use parse::Limits;

/// Parses an ASON document with the default hard resource ceilings.
pub fn decode(input: &str) -> Result<Document, DecodeError> {
    decode_with_limits(input, &Limits::default())
}

/// Parses an ASON document with caller-provided resource ceilings.
pub fn decode_with_limits(input: &str, limits: &Limits) -> Result<Document, DecodeError> {
    parse::decode(input, limits)
}

/// Parses and re-encodes a document into its unique syntax-level form.
pub fn canonicalize(input: &str, limits: &Limits) -> Result<String, DecodeError> {
    decode_with_limits(input, limits).map(|document| encode::encode(&document))
}

/// Returns whether the input is already the canonical encoding of its value.
pub fn is_canonical(input: &str, limits: &Limits) -> Result<bool, DecodeError> {
    canonicalize(input, limits).map(|canonical| canonical == input)
}

impl Document {
    /// Returns the canonical ASON text, including exactly one final LF.
    #[must_use]
    pub fn encode(&self) -> String {
        encode::encode(self)
    }
}

impl FromStr for Document {
    type Err = DecodeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        decode(input)
    }
}

impl std::fmt::Display for Document {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Atom, Cell, DecodeError, Document, Field, Key, LimitKind, Limits, Record, Table, Value,
        canonicalize, decode,
    };

    const SEARCH_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/search-request.ason");
    const SEARCH_RESULT: &str = include_str!("../../../../spec/fixtures/ason/search-result.ason");
    const VALUE_FORMS: &str = include_str!("../../../../spec/fixtures/ason/value-forms.ason");

    #[test]
    fn canonical_fixtures_round_trip_byte_for_byte() {
        for fixture in [SEARCH_REQUEST, SEARCH_RESULT, VALUE_FORMS] {
            let document = decode(fixture).expect("fixture must decode");
            assert_eq!(document.encode(), fixture);
            assert_eq!(
                decode(&document.encode()).expect("encoded output decodes"),
                document
            );
        }
    }

    #[test]
    fn search_request_preserves_schema_bound_values() {
        let document = decode(SEARCH_REQUEST).expect("request must decode");
        assert_eq!(document.get("o"), Some(&Value::Scalar(Atom::text("g"))));
        let Value::Record(arguments) = document.get("a").expect("arguments") else {
            panic!("arguments must be a record");
        };
        assert_eq!(arguments.columns()[0].as_str(), "q");
        assert_eq!(arguments.values().len(), 3);
    }

    #[test]
    fn safe_quoted_text_is_canonicalized_to_bare_text() {
        let input = "v:\"safe\"\n";
        assert_eq!(
            canonicalize(input, &Limits::default()).expect("valid input"),
            "v:safe\n"
        );
    }

    #[test]
    fn malformed_documents_are_rejected_before_model_creation() {
        for input in [
            "t:1",
            "t:1\r\n",
            "t:1\n\n",
            "t:1\nt:2\n",
            "d[2]{p,l}:\n1,2\n",
            "d{p,p}:\n1,2\n",
            "v:[a,[b]]\n",
            "v:\"unterminated\n",
            "r:@01\n",
        ] {
            assert!(
                decode(input).is_err(),
                "input unexpectedly decoded: {input:?}"
            );
        }
    }

    #[test]
    fn limits_are_enforced_during_parse() {
        let limits = Limits {
            max_values: 2,
            ..Limits::default()
        };
        let error = super::decode_with_limits("v:[a,b,c]\n", &limits)
            .expect_err("value ceiling must reject the vector");
        assert!(matches!(
            error,
            DecodeError::Limit {
                kind: LimitKind::Values,
                max: 2,
                ..
            }
        ));
    }

    #[test]
    fn constructed_values_round_trip_without_lexical_ambiguity() {
        let document = Document::new(vec![
            Field::new(
                Key::new("scalars").expect("key"),
                Value::Vector(vec![
                    Atom::text("@01"),
                    Atom::text("~"),
                    Atom::text(""),
                    Atom::text("with space"),
                    Atom::text("中"),
                    Atom::reference(7),
                    Atom::Null,
                ]),
            ),
            Field::new(
                Key::new("record").expect("key"),
                Value::Record(
                    Record::new(
                        vec![Key::new("a").expect("key"), Key::new("b").expect("key")],
                        vec![
                            Cell::Atom(Atom::text("a,b")),
                            Cell::Vector(vec![Atom::text("[x]"), Atom::text("line\n")]),
                        ],
                    )
                    .expect("record"),
                ),
            ),
            Field::new(
                Key::new("table").expect("key"),
                Value::Table(
                    Table::new(
                        vec![Key::new("x").expect("key")],
                        vec![vec![Cell::Atom(Atom::text("value"))]],
                    )
                    .expect("table"),
                ),
            ),
        ])
        .expect("document");

        let encoded = document.encode();
        assert_eq!(
            decode(&encoded).expect("decode constructed document"),
            document
        );
        assert!(encoded.contains("\"@01\""));
        assert!(encoded.contains("\"~\""));
    }

    #[test]
    fn arbitrary_valid_utf8_never_panics_the_parser() {
        const ALPHABET: &[char] = &[
            'a', '0', ':', ',', '[', ']', '{', '}', '"', '\\', '@', '~', '\n', '\r', '中', '\u{7}',
        ];
        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        for _ in 0..5_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let length = (state as usize) & 127;
            let mut input = String::new();
            for _ in 0..length {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                input.push(ALPHABET[(state as usize) % ALPHABET.len()]);
            }
            let _ = decode(&input);
        }
    }

    #[test]
    fn collection_limits_stop_wide_input_early() {
        let limits = Limits {
            max_columns: 2,
            max_vector_items: 2,
            ..Limits::default()
        };
        assert!(matches!(
            super::decode_with_limits("r{a,b,c,d,e}:\n1,2,3,4,5\n", &limits),
            Err(DecodeError::Limit {
                kind: LimitKind::Columns,
                max: 2,
                ..
            })
        ));
        assert!(matches!(
            super::decode_with_limits("v:[a,b,c,d,e]\n", &limits),
            Err(DecodeError::Limit {
                kind: LimitKind::VectorItems,
                max: 2,
                ..
            })
        ));
    }
}
