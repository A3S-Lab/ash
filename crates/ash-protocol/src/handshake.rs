//! Typed ASH/1 session handshake.

use thiserror::Error;

use crate::ason::{Atom, BuildError, Cell, Document, Field, Key, Record, Value};
use crate::frame::{DEFAULT_MAX_FRAME_BYTES, HARD_MAX_FRAME_BYTES};
use crate::{
    ALL_OPERATION_MASK, ASH_PROTOCOL_MAJOR, ASH_PROTOCOL_MINOR, ASON_FORMAT_MAJOR,
    ASON_FORMAT_MINOR,
};

const MAX_WORKSPACE_BYTES: usize = 4096;
const MAX_NONCE_BYTES: usize = 128;
const MAX_PLATFORM_BYTES: usize = 64;
pub const MIN_SESSION_FRAME_BYTES: u32 = 256;

const REQUEST_COLUMNS: &[&str] = &[
    "ap", "al", "ah", "zp", "zl", "zh", "frm", "out", "ops", "cap", "root", "n",
];
const RESPONSE_COLUMNS: &[&str] = &[
    "ap", "av", "zp", "zv", "frm", "out", "ops", "cap", "os", "arch", "sid", "n",
];

/// Client-selected handshake preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandshakePreferences {
    pub ash_minor_low: u16,
    pub ash_minor_high: u16,
    pub ason_minor_low: u16,
    pub ason_minor_high: u16,
    pub max_frame_bytes: u32,
    pub output_bytes: u32,
    pub operation_mask: u64,
    pub capability_mask: u64,
}

impl Default for HandshakePreferences {
    fn default() -> Self {
        Self {
            ash_minor_low: ASH_PROTOCOL_MINOR,
            ash_minor_high: ASH_PROTOCOL_MINOR,
            ason_minor_low: ASON_FORMAT_MINOR,
            ason_minor_high: ASON_FORMAT_MINOR,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES as u32,
            output_bytes: 64 * 1024,
            operation_mask: 0,
            capability_mask: 0,
        }
    }
}

/// The first client message in a persistent ASH/1 session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandshakeRequest {
    request_id: u64,
    ash_major: u16,
    ason_major: u16,
    preferences: HandshakePreferences,
    workspace: String,
    nonce: String,
}

impl HandshakeRequest {
    pub fn new(
        request_id: u64,
        workspace: impl Into<String>,
        nonce: impl Into<String>,
        preferences: HandshakePreferences,
    ) -> Result<Self, SchemaError> {
        let request = Self {
            request_id,
            ash_major: ASH_PROTOCOL_MAJOR,
            ason_major: ASON_FORMAT_MAJOR,
            preferences,
            workspace: workspace.into(),
            nonce: nonce.into(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn decode(document: &Document) -> Result<Self, SchemaError> {
        expect_fields(document, &["t", "i", "a"])?;
        expect_unsigned(document.get("t"), "t", 0)?;
        let request_id = unsigned(document.get("i"), "i")?;
        let arguments = record(document.get("a"), "a")?;
        expect_columns(arguments, REQUEST_COLUMNS)?;
        let values = arguments.values();

        let request = Self {
            request_id,
            ash_major: narrow(unsigned_cell(&values[0], "ap")?, "ap")?,
            preferences: HandshakePreferences {
                ash_minor_low: narrow(unsigned_cell(&values[1], "al")?, "al")?,
                ash_minor_high: narrow(unsigned_cell(&values[2], "ah")?, "ah")?,
                ason_minor_low: narrow(unsigned_cell(&values[4], "zl")?, "zl")?,
                ason_minor_high: narrow(unsigned_cell(&values[5], "zh")?, "zh")?,
                max_frame_bytes: narrow_u32(unsigned_cell(&values[6], "frm")?, "frm")?,
                output_bytes: narrow_u32(unsigned_cell(&values[7], "out")?, "out")?,
                operation_mask: unsigned_cell(&values[8], "ops")?,
                capability_mask: unsigned_cell(&values[9], "cap")?,
            },
            ason_major: narrow(unsigned_cell(&values[3], "zp")?, "zp")?,
            workspace: text_cell(&values[10], "root")?.to_owned(),
            nonce: text_cell(&values[11], "n")?.to_owned(),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn encode(&self) -> Result<Document, BuildError> {
        let p = self.preferences;
        let values = vec![
            unsigned_value(u64::from(self.ash_major)),
            unsigned_value(u64::from(p.ash_minor_low)),
            unsigned_value(u64::from(p.ash_minor_high)),
            unsigned_value(u64::from(self.ason_major)),
            unsigned_value(u64::from(p.ason_minor_low)),
            unsigned_value(u64::from(p.ason_minor_high)),
            unsigned_value(u64::from(p.max_frame_bytes)),
            unsigned_value(u64::from(p.output_bytes)),
            unsigned_value(p.operation_mask),
            unsigned_value(p.capability_mask),
            text_value(&self.workspace),
            text_value(&self.nonce),
        ];
        Document::new(vec![
            scalar_field("t", "0")?,
            scalar_field("i", &self.request_id.to_string())?,
            Field::new(
                Key::new("a")?,
                Value::Record(Record::new(keys(REQUEST_COLUMNS)?, values)?),
            ),
        ])
    }

    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    #[must_use]
    pub const fn preferences(&self) -> HandshakePreferences {
        self.preferences
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    fn validate(&self) -> Result<(), SchemaError> {
        let p = self.preferences;
        if p.ash_minor_low > p.ash_minor_high || p.ason_minor_low > p.ason_minor_high {
            return Err(SchemaError::InvalidRange);
        }
        if p.max_frame_bytes < MIN_SESSION_FRAME_BYTES
            || usize::try_from(p.max_frame_bytes).unwrap_or(usize::MAX) > HARD_MAX_FRAME_BYTES
        {
            return Err(SchemaError::InvalidLimit("frm"));
        }
        if p.output_bytes == 0 {
            return Err(SchemaError::InvalidLimit("out"));
        }
        if self.workspace.is_empty() || self.workspace.len() > MAX_WORKSPACE_BYTES {
            return Err(SchemaError::InvalidText("root"));
        }
        if self.nonce.is_empty() || self.nonce.len() > MAX_NONCE_BYTES {
            return Err(SchemaError::InvalidText("n"));
        }
        Ok(())
    }
}

/// Server capabilities used to negotiate one local session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerHandshake {
    pub max_frame_bytes: u32,
    pub max_output_bytes: u32,
    pub operation_mask: u64,
    pub capability_mask: u64,
    pub os: String,
    pub arch: String,
}

impl Default for ServerHandshake {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES as u32,
            max_output_bytes: 64 * 1024,
            operation_mask: 0,
            capability_mask: 0,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

impl ServerHandshake {
    pub fn negotiate(
        &self,
        request: &HandshakeRequest,
        session_id: u64,
    ) -> Result<HandshakeResponse, SchemaError> {
        let preferences = request.preferences;
        if request.ash_major != ASH_PROTOCOL_MAJOR
            || !(preferences.ash_minor_low..=preferences.ash_minor_high)
                .contains(&ASH_PROTOCOL_MINOR)
        {
            return Err(SchemaError::UnsupportedVersion("ash"));
        }
        if request.ason_major != ASON_FORMAT_MAJOR
            || !(preferences.ason_minor_low..=preferences.ason_minor_high)
                .contains(&ASON_FORMAT_MINOR)
        {
            return Err(SchemaError::UnsupportedVersion("ason"));
        }
        if self.max_frame_bytes < MIN_SESSION_FRAME_BYTES
            || usize::try_from(self.max_frame_bytes).unwrap_or(usize::MAX) > HARD_MAX_FRAME_BYTES
            || self.max_output_bytes == 0
        {
            return Err(SchemaError::InvalidServerConfiguration);
        }
        if !valid_platform_name(&self.os) || !valid_platform_name(&self.arch) || session_id == 0 {
            return Err(SchemaError::InvalidServerConfiguration);
        }

        let frame_bytes = preferences.max_frame_bytes.min(self.max_frame_bytes);
        Ok(HandshakeResponse {
            request_id: request.request_id,
            ash_minor: ASH_PROTOCOL_MINOR,
            ason_minor: ASON_FORMAT_MINOR,
            frame_bytes,
            output_bytes: preferences
                .output_bytes
                .min(self.max_output_bytes)
                .min(frame_bytes),
            operation_mask: preferences.operation_mask & self.operation_mask & ALL_OPERATION_MASK,
            capability_mask: preferences.capability_mask & self.capability_mask,
            os: self.os.clone(),
            arch: self.arch.clone(),
            session_id,
            nonce: request.nonce.clone(),
        })
    }
}

/// The successful server handshake result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandshakeResponse {
    request_id: u64,
    ash_minor: u16,
    ason_minor: u16,
    frame_bytes: u32,
    output_bytes: u32,
    operation_mask: u64,
    capability_mask: u64,
    os: String,
    arch: String,
    session_id: u64,
    nonce: String,
}

impl HandshakeResponse {
    pub fn decode(document: &Document) -> Result<Self, SchemaError> {
        expect_fields(document, &["t", "i", "s", "d"])?;
        expect_unsigned(document.get("t"), "t", 0)?;
        expect_unsigned(document.get("s"), "s", 0)?;
        let request_id = unsigned(document.get("i"), "i")?;
        let data = record(document.get("d"), "d")?;
        expect_columns(data, RESPONSE_COLUMNS)?;
        let values = data.values();
        expect_unsigned_cell(&values[0], "ap", u64::from(ASH_PROTOCOL_MAJOR))?;
        expect_unsigned_cell(&values[2], "zp", u64::from(ASON_FORMAT_MAJOR))?;
        let response = Self {
            request_id,
            ash_minor: narrow(unsigned_cell(&values[1], "av")?, "av")?,
            ason_minor: narrow(unsigned_cell(&values[3], "zv")?, "zv")?,
            frame_bytes: narrow_u32(unsigned_cell(&values[4], "frm")?, "frm")?,
            output_bytes: narrow_u32(unsigned_cell(&values[5], "out")?, "out")?,
            operation_mask: unsigned_cell(&values[6], "ops")?,
            capability_mask: unsigned_cell(&values[7], "cap")?,
            os: text_cell(&values[8], "os")?.to_owned(),
            arch: text_cell(&values[9], "arch")?.to_owned(),
            session_id: unsigned_cell(&values[10], "sid")?,
            nonce: text_cell(&values[11], "n")?.to_owned(),
        };
        response.validate()?;
        Ok(response)
    }

    pub fn encode(&self) -> Result<Document, BuildError> {
        let values = vec![
            unsigned_value(u64::from(ASH_PROTOCOL_MAJOR)),
            unsigned_value(u64::from(self.ash_minor)),
            unsigned_value(u64::from(ASON_FORMAT_MAJOR)),
            unsigned_value(u64::from(self.ason_minor)),
            unsigned_value(u64::from(self.frame_bytes)),
            unsigned_value(u64::from(self.output_bytes)),
            unsigned_value(self.operation_mask),
            unsigned_value(self.capability_mask),
            text_value(&self.os),
            text_value(&self.arch),
            unsigned_value(self.session_id),
            text_value(&self.nonce),
        ];
        Document::new(vec![
            scalar_field("t", "0")?,
            scalar_field("i", &self.request_id.to_string())?,
            scalar_field("s", "0")?,
            Field::new(
                Key::new("d")?,
                Value::Record(Record::new(keys(RESPONSE_COLUMNS)?, values)?),
            ),
        ])
    }

    #[must_use]
    pub const fn frame_bytes(&self) -> u32 {
        self.frame_bytes
    }

    #[must_use]
    pub const fn operation_mask(&self) -> u64 {
        self.operation_mask
    }

    #[must_use]
    pub const fn output_bytes(&self) -> u32 {
        self.output_bytes
    }

    #[must_use]
    pub const fn session_id(&self) -> u64 {
        self.session_id
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    fn validate(&self) -> Result<(), SchemaError> {
        if self.ash_minor != ASH_PROTOCOL_MINOR {
            return Err(SchemaError::UnsupportedVersion("ash"));
        }
        if self.ason_minor != ASON_FORMAT_MINOR {
            return Err(SchemaError::UnsupportedVersion("ason"));
        }
        if self.frame_bytes < MIN_SESSION_FRAME_BYTES
            || usize::try_from(self.frame_bytes).unwrap_or(usize::MAX) > HARD_MAX_FRAME_BYTES
        {
            return Err(SchemaError::InvalidLimit("frm"));
        }
        if self.output_bytes == 0 {
            return Err(SchemaError::InvalidLimit("out"));
        }
        if self.operation_mask & !ALL_OPERATION_MASK != 0 {
            return Err(SchemaError::UnexpectedValue("ops"));
        }
        if !valid_platform_name(&self.os) {
            return Err(SchemaError::InvalidText("os"));
        }
        if !valid_platform_name(&self.arch) {
            return Err(SchemaError::InvalidText("arch"));
        }
        if self.session_id == 0 {
            return Err(SchemaError::UnexpectedValue("sid"));
        }
        if self.nonce.is_empty() || self.nonce.len() > MAX_NONCE_BYTES {
            return Err(SchemaError::InvalidText("n"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SchemaError {
    #[error("unexpected top-level field order")]
    Fields,
    #[error("field {0} must be a scalar")]
    ExpectedScalar(&'static str),
    #[error("field {0} must be a record")]
    ExpectedRecord(&'static str),
    #[error("record columns do not match the handshake schema")]
    Columns,
    #[error("field {0} is not canonical unsigned decimal")]
    InvalidUnsigned(&'static str),
    #[error("field {0} has an unexpected value")]
    UnexpectedValue(&'static str),
    #[error("field {0} exceeds its integer type")]
    IntegerRange(&'static str),
    #[error("handshake version range is invalid")]
    InvalidRange,
    #[error("handshake limit {0} is invalid")]
    InvalidLimit(&'static str),
    #[error("handshake text field {0} is invalid")]
    InvalidText(&'static str),
    #[error("unsupported {0} protocol version")]
    UnsupportedVersion(&'static str),
    #[error("server handshake configuration is invalid")]
    InvalidServerConfiguration,
}

fn expect_fields(document: &Document, expected: &[&str]) -> Result<(), SchemaError> {
    if document.fields().len() == expected.len()
        && document
            .fields()
            .iter()
            .zip(expected)
            .all(|(field, expected)| field.key().as_str() == *expected)
    {
        Ok(())
    } else {
        Err(SchemaError::Fields)
    }
}

fn expect_columns(record: &Record, expected: &[&str]) -> Result<(), SchemaError> {
    if record.columns().len() == expected.len()
        && record
            .columns()
            .iter()
            .zip(expected)
            .all(|(column, expected)| column.as_str() == *expected)
    {
        Ok(())
    } else {
        Err(SchemaError::Columns)
    }
}

fn unsigned(value: Option<&Value>, field: &'static str) -> Result<u64, SchemaError> {
    match value {
        Some(Value::Scalar(atom)) => unsigned_atom(atom, field),
        _ => Err(SchemaError::ExpectedScalar(field)),
    }
}

fn unsigned_cell(cell: &Cell, field: &'static str) -> Result<u64, SchemaError> {
    match cell {
        Cell::Atom(atom) => unsigned_atom(atom, field),
        Cell::Vector(_) => Err(SchemaError::ExpectedScalar(field)),
    }
}

fn unsigned_atom(atom: &Atom, field: &'static str) -> Result<u64, SchemaError> {
    let Atom::Text(value) = atom else {
        return Err(SchemaError::InvalidUnsigned(field));
    };
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(SchemaError::InvalidUnsigned(field));
    }
    value
        .parse::<u64>()
        .map_err(|_| SchemaError::InvalidUnsigned(field))
}

fn expect_unsigned(
    value: Option<&Value>,
    field: &'static str,
    expected: u64,
) -> Result<(), SchemaError> {
    if unsigned(value, field)? == expected {
        Ok(())
    } else {
        Err(SchemaError::UnexpectedValue(field))
    }
}

fn expect_unsigned_cell(
    value: &Cell,
    field: &'static str,
    expected: u64,
) -> Result<(), SchemaError> {
    if unsigned_cell(value, field)? == expected {
        Ok(())
    } else {
        Err(SchemaError::UnexpectedValue(field))
    }
}

fn text_cell<'a>(cell: &'a Cell, field: &'static str) -> Result<&'a str, SchemaError> {
    match cell {
        Cell::Atom(Atom::Text(value)) => Ok(value),
        Cell::Atom(Atom::Null | Atom::Reference(_)) | Cell::Vector(_) => {
            Err(SchemaError::ExpectedScalar(field))
        }
    }
}

fn record<'a>(value: Option<&'a Value>, field: &'static str) -> Result<&'a Record, SchemaError> {
    match value {
        Some(Value::Record(record)) => Ok(record),
        _ => Err(SchemaError::ExpectedRecord(field)),
    }
}

fn narrow(value: u64, field: &'static str) -> Result<u16, SchemaError> {
    u16::try_from(value).map_err(|_| SchemaError::IntegerRange(field))
}

fn narrow_u32(value: u64, field: &'static str) -> Result<u32, SchemaError> {
    u32::try_from(value).map_err(|_| SchemaError::IntegerRange(field))
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

fn valid_platform_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_PLATFORM_BYTES
}

#[cfg(test)]
mod tests;
