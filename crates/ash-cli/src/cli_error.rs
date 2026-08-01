use std::io;
use std::string::FromUtf8Error;

use ash_protocol::ason::{BuildError, DecodeError};
use ash_protocol::frame::{FrameError, ProtocolReadError};
use ash_protocol::handshake::SchemaError;
use thiserror::Error;
use tokio::io::{AsyncWriteExt, stderr};

#[derive(Debug, Error)]
pub enum CliError {
    #[error("unsupported command shape")]
    Usage,
    #[error("stdin exceeds the hard input ceiling")]
    InputTooLarge,
    #[error(transparent)]
    InvalidUtf8(#[from] FromUtf8Error),
    #[error(transparent)]
    Ason(#[from] DecodeError),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Protocol(#[from] ProtocolReadError),
    #[error(transparent)]
    Handshake(#[from] SchemaError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error("RPC input ended before a handshake")]
    MissingHandshake,
    #[error("this protocol message is not implemented yet")]
    UnsupportedMessage,
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl CliError {
    pub const fn diagnostic_code(&self) -> u16 {
        match self {
            Self::Usage => 1,
            Self::InputTooLarge => 2,
            Self::InvalidUtf8(_) => 3,
            Self::Ason(_) => 4,
            Self::Protocol(ProtocolReadError::InvalidUtf8 { .. }) => 3,
            Self::Protocol(ProtocolReadError::Ason(_)) => 4,
            Self::Frame(FrameError::Io(_))
            | Self::Protocol(ProtocolReadError::Frame(FrameError::Io(_)))
            | Self::Io(_) => 9,
            Self::Frame(_)
            | Self::Protocol(ProtocolReadError::Frame(_) | ProtocolReadError::NonCanonical) => 5,
            Self::Handshake(_) => 6,
            Self::MissingHandshake => 7,
            Self::UnsupportedMessage => 8,
            Self::Build(_) => 10,
        }
    }

    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage => 2,
            Self::InputTooLarge | Self::InvalidUtf8(_) | Self::Ason(_) => 3,
            Self::Frame(_)
            | Self::Protocol(_)
            | Self::Handshake(_)
            | Self::MissingHandshake
            | Self::UnsupportedMessage => 4,
            Self::Io(_) | Self::Build(_) => 70,
        }
    }

    pub async fn emit(&self) {
        let diagnostic = format!("s:1\ne{{c}}:\n{}\n", self.diagnostic_code());
        let mut stderr = stderr();
        let _ = stderr.write_all(diagnostic.as_bytes()).await;
        let _ = stderr.flush().await;
    }
}
