#![forbid(unsafe_code)]

//! Stable ASH protocol identifiers, canonical ASON, and stream framing.

pub mod ason;
mod capability;
pub mod frame;
pub mod handshake;
mod operation;
pub mod request;
pub mod response;

pub use capability::{
    ALL_CAPABILITY_MASK, APPROVAL_CHALLENGE_VERSION, APPROVAL_SIGNING_BYTES, APPROVAL_TOKEN_BYTES,
    APPROVAL_TOKEN_HEX_BYTES, ApprovalChallenge, ApprovalToken, ApprovalValueError, Capability,
};
pub use operation::{ALL_OPERATION_MASK, Operation};

/// The current ASH protocol major version.
pub const ASH_PROTOCOL_MAJOR: u16 = 1;

/// The current ASH protocol minor version.
pub const ASH_PROTOCOL_MINOR: u16 = 0;

/// The current ASON format major version.
pub const ASON_FORMAT_MAJOR: u16 = 1;

/// The current ASON format minor version.
pub const ASON_FORMAT_MINOR: u16 = 0;
