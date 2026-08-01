#![forbid(unsafe_code)]

//! Stable ASH protocol identifiers, canonical ASON, and stream framing.

pub mod ason;
pub mod frame;
pub mod handshake;
mod operation;

pub use operation::{ALL_OPERATION_MASK, Operation};

/// The current ASH protocol major version.
pub const ASH_PROTOCOL_MAJOR: u16 = 1;

/// The current ASH protocol minor version.
pub const ASH_PROTOCOL_MINOR: u16 = 0;

/// The current ASON format major version.
pub const ASON_FORMAT_MAJOR: u16 = 1;

/// The current ASON format minor version.
pub const ASON_FORMAT_MINOR: u16 = 0;
