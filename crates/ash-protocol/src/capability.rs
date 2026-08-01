//! Stable capability identifiers and approval wire values.

use thiserror::Error;

use crate::ason::{Atom, BuildError, Document, Field, Key, Value};

const CHALLENGE_FIELDS: &[&str] = &["v", "s", "c", "e", "b", "p", "h", "n"];
pub const APPROVAL_CHALLENGE_VERSION: u8 = 1;
pub const APPROVAL_SIGNING_BYTES: usize = 105;
pub const APPROVAL_TOKEN_BYTES: usize = 16 + 32;
pub const APPROVAL_TOKEN_HEX_BYTES: usize = APPROVAL_TOKEN_BYTES * 2;

/// Stable capability bits negotiated by ASH/1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Capability {
    WorkspaceRead,
    WorkspaceWrite,
    HostProcess,
    RetainedResult,
}

impl Capability {
    #[must_use]
    pub const fn mask(self) -> u64 {
        match self {
            Self::WorkspaceRead => 1 << 0,
            Self::WorkspaceWrite => 1 << 1,
            Self::HostProcess => 1 << 2,
            Self::RetainedResult => 1 << 3,
        }
    }
}

pub const ALL_CAPABILITY_MASK: u64 = (1 << 4) - 1;

/// Immutable challenge retained as evidence for an approval-required result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApprovalChallenge {
    session_id: u64,
    capabilities: u64,
    expires_at_millis: u64,
    session_binding: [u8; 16],
    policy_digest: [u8; 16],
    action_digest: [u8; 32],
    nonce: [u8; 16],
}

impl ApprovalChallenge {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: u64,
        capabilities: u64,
        expires_at_millis: u64,
        session_binding: [u8; 16],
        policy_digest: [u8; 16],
        action_digest: [u8; 32],
        nonce: [u8; 16],
    ) -> Result<Self, ApprovalValueError> {
        let challenge = Self {
            session_id,
            capabilities,
            expires_at_millis,
            session_binding,
            policy_digest,
            action_digest,
            nonce,
        };
        challenge.validate()?;
        Ok(challenge)
    }

    pub fn decode(document: &Document) -> Result<Self, ApprovalValueError> {
        if document.fields().len() != CHALLENGE_FIELDS.len()
            || !document
                .fields()
                .iter()
                .zip(CHALLENGE_FIELDS)
                .all(|(field, expected)| field.key().as_str() == *expected)
        {
            return Err(ApprovalValueError::Fields);
        }
        if canonical_u64(document, "v")? != u64::from(APPROVAL_CHALLENGE_VERSION) {
            return Err(ApprovalValueError::Version);
        }
        Self::new(
            canonical_u64(document, "s")?,
            canonical_u64(document, "c")?,
            canonical_u64(document, "e")?,
            decode_hex_array(scalar(document, "b")?)?,
            decode_hex_array(scalar(document, "p")?)?,
            decode_hex_array(scalar(document, "h")?)?,
            decode_hex_array(scalar(document, "n")?)?,
        )
    }

    pub fn encode(&self) -> Result<Document, BuildError> {
        Document::new(vec![
            scalar_field("v", &APPROVAL_CHALLENGE_VERSION.to_string())?,
            scalar_field("s", &self.session_id.to_string())?,
            scalar_field("c", &self.capabilities.to_string())?,
            scalar_field("e", &self.expires_at_millis.to_string())?,
            scalar_field("b", &encode_hex(&self.session_binding))?,
            scalar_field("p", &encode_hex(&self.policy_digest))?,
            scalar_field("h", &encode_hex(&self.action_digest))?,
            scalar_field("n", &encode_hex(&self.nonce))?,
        ])
    }

    #[must_use]
    pub fn signing_bytes(self) -> [u8; APPROVAL_SIGNING_BYTES] {
        let mut bytes = [0_u8; APPROVAL_SIGNING_BYTES];
        let mut cursor = 0;
        put(&mut bytes, &mut cursor, &[APPROVAL_CHALLENGE_VERSION]);
        put(&mut bytes, &mut cursor, &self.session_id.to_be_bytes());
        put(&mut bytes, &mut cursor, &self.capabilities.to_be_bytes());
        put(
            &mut bytes,
            &mut cursor,
            &self.expires_at_millis.to_be_bytes(),
        );
        put(&mut bytes, &mut cursor, &self.session_binding);
        put(&mut bytes, &mut cursor, &self.policy_digest);
        put(&mut bytes, &mut cursor, &self.action_digest);
        put(&mut bytes, &mut cursor, &self.nonce);
        debug_assert_eq!(cursor, APPROVAL_SIGNING_BYTES);
        bytes
    }

    pub fn from_signing_bytes(
        bytes: &[u8; APPROVAL_SIGNING_BYTES],
    ) -> Result<Self, ApprovalValueError> {
        if bytes[0] != APPROVAL_CHALLENGE_VERSION {
            return Err(ApprovalValueError::Version);
        }
        Self::new(
            u64::from_be_bytes(
                bytes[1..9]
                    .try_into()
                    .map_err(|_| ApprovalValueError::Token)?,
            ),
            u64::from_be_bytes(
                bytes[9..17]
                    .try_into()
                    .map_err(|_| ApprovalValueError::Token)?,
            ),
            u64::from_be_bytes(
                bytes[17..25]
                    .try_into()
                    .map_err(|_| ApprovalValueError::Token)?,
            ),
            bytes[25..41]
                .try_into()
                .map_err(|_| ApprovalValueError::Token)?,
            bytes[41..57]
                .try_into()
                .map_err(|_| ApprovalValueError::Token)?,
            bytes[57..89]
                .try_into()
                .map_err(|_| ApprovalValueError::Token)?,
            bytes[89..105]
                .try_into()
                .map_err(|_| ApprovalValueError::Token)?,
        )
    }

    #[must_use]
    pub const fn session_id(self) -> u64 {
        self.session_id
    }

    #[must_use]
    pub const fn capabilities(self) -> u64 {
        self.capabilities
    }

    #[must_use]
    pub const fn expires_at_millis(self) -> u64 {
        self.expires_at_millis
    }

    #[must_use]
    pub const fn session_binding(self) -> [u8; 16] {
        self.session_binding
    }

    #[must_use]
    pub const fn policy_digest(self) -> [u8; 16] {
        self.policy_digest
    }

    #[must_use]
    pub const fn action_digest(self) -> [u8; 32] {
        self.action_digest
    }

    #[must_use]
    pub const fn nonce(self) -> [u8; 16] {
        self.nonce
    }

    fn validate(&self) -> Result<(), ApprovalValueError> {
        if self.session_id == 0 {
            return Err(ApprovalValueError::Unsigned("s"));
        }
        if self.capabilities == 0 || self.capabilities & !ALL_CAPABILITY_MASK != 0 {
            return Err(ApprovalValueError::Capability);
        }
        if self.expires_at_millis == 0 {
            return Err(ApprovalValueError::Unsigned("e"));
        }
        if self.session_binding == [0; 16]
            || self.policy_digest == [0; 16]
            || self.action_digest == [0; 32]
            || self.nonce == [0; 16]
        {
            return Err(ApprovalValueError::Digest);
        }
        Ok(())
    }
}

/// Opaque fixed-size permit attached to a retried request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalToken([u8; APPROVAL_TOKEN_BYTES]);

impl ApprovalToken {
    pub fn parse(value: &str) -> Result<Self, ApprovalValueError> {
        Ok(Self(decode_hex_array(value)?))
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPROVAL_TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPROVAL_TOKEN_BYTES] {
        &self.0
    }

    #[must_use]
    pub fn encode(&self) -> String {
        encode_hex(&self.0)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ApprovalValueError {
    #[error("approval value fields are missing or out of canonical order")]
    Fields,
    #[error("approval challenge uses an unsupported version")]
    Version,
    #[error("approval field {0} must be a canonical positive unsigned integer")]
    Unsigned(&'static str),
    #[error("approval capability mask is empty or contains unknown bits")]
    Capability,
    #[error("approval digest, binding, or nonce is invalid")]
    Digest,
    #[error("approval token is not canonical lowercase hexadecimal")]
    Token,
}

fn scalar<'a>(document: &'a Document, key: &'static str) -> Result<&'a str, ApprovalValueError> {
    match document.get(key) {
        Some(Value::Scalar(Atom::Text(value))) => Ok(value),
        _ => Err(ApprovalValueError::Fields),
    }
}

fn canonical_u64(document: &Document, key: &'static str) -> Result<u64, ApprovalValueError> {
    let text = scalar(document, key)?;
    let value = text
        .parse::<u64>()
        .map_err(|_| ApprovalValueError::Unsigned(key))?;
    if value.to_string() == text {
        Ok(value)
    } else {
        Err(ApprovalValueError::Unsigned(key))
    }
}

fn scalar_field(key: &str, value: &str) -> Result<Field, BuildError> {
    Ok(Field::new(Key::new(key)?, Value::Scalar(Atom::text(value))))
}

fn put<const N: usize>(output: &mut [u8; N], cursor: &mut usize, value: &[u8]) {
    let end = *cursor + value.len();
    output[*cursor..end].copy_from_slice(value);
    *cursor = end;
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_hex_array<const N: usize>(value: &str) -> Result<[u8; N], ApprovalValueError> {
    if value.len() != N * 2 {
        return Err(ApprovalValueError::Token);
    }
    let mut bytes = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes[index] = high
            .checked_mul(16)
            .and_then(|high| high.checked_add(low))
            .ok_or(ApprovalValueError::Token)?;
    }
    Ok(bytes)
}

const fn hex_nibble(byte: u8) -> Result<u8, ApprovalValueError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ApprovalValueError::Token),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ALL_CAPABILITY_MASK, APPROVAL_TOKEN_BYTES, ApprovalChallenge, ApprovalToken, Capability,
    };
    use crate::ason::decode;

    const APPROVAL_CHALLENGE: &str =
        include_str!("../../../spec/fixtures/ason/approval-challenge.ason");

    #[test]
    fn capability_bits_and_approval_values_are_stable() {
        assert_eq!(Capability::WorkspaceRead.mask(), 1);
        assert_eq!(Capability::WorkspaceWrite.mask(), 2);
        assert_eq!(Capability::HostProcess.mask(), 4);
        assert_eq!(Capability::RetainedResult.mask(), 8);
        assert_eq!(ALL_CAPABILITY_MASK, 15);

        let challenge = ApprovalChallenge::new(7, 2, 99, [1; 16], [2; 16], [3; 32], [4; 16])
            .expect("challenge");
        let encoded = challenge.encode().expect("encode").encode();
        assert_eq!(encoded, APPROVAL_CHALLENGE);
        let decoded =
            ApprovalChallenge::decode(&decode(&encoded).expect("ASON")).expect("decode challenge");
        assert_eq!(decoded, challenge);
        assert_eq!(
            ApprovalChallenge::from_signing_bytes(&challenge.signing_bytes()).expect("binary"),
            challenge
        );

        let token = ApprovalToken::from_bytes([0xab; APPROVAL_TOKEN_BYTES]);
        assert_eq!(ApprovalToken::parse(&token.encode()).expect("token"), token);
        assert!(ApprovalToken::parse(&token.encode().to_uppercase()).is_err());
    }
}
