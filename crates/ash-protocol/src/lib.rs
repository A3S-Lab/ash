#![forbid(unsafe_code)]

//! Stable ASH protocol identifiers, canonical ASON, and stream framing.

pub mod ason;
pub mod frame;
mod operation;

pub use operation::Operation;

/// The current ASH protocol major version.
pub const ASH_PROTOCOL_MAJOR: u16 = 1;

/// The current ASON format major version.
pub const ASON_FORMAT_MAJOR: u16 = 1;
