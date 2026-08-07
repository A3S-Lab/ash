use std::io;
use std::string::FromUtf8Error;

use ash_engine::EngineError;
use ash_ops::OperationError;
use ash_platform::PlatformError;
use ash_protocol::ason::{BuildError, DecodeError};
use ash_protocol::frame::{FrameError, ProtocolReadError};
use ash_protocol::handshake::SchemaError;
use ash_protocol::request::RequestError;
use ash_protocol::response::ResponseError;
use ash_update::UpdateError;
use thiserror::Error;
use tokio::io::{AsyncWriteExt, stderr};

#[derive(Debug, Error)]
pub enum CliError {
    #[error("unsupported command shape")]
    Usage,
    #[error("stdin exceeds the hard input ceiling")]
    InputTooLarge,
    #[error("{message}")]
    Human { message: String, exit_code: u8 },
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
    Request(#[from] RequestError),
    #[error(transparent)]
    Response(#[from] ResponseError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    Update(#[from] UpdateError),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error("RPC input ended before a handshake")]
    MissingHandshake,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
}

impl CliError {
    #[cfg(feature = "human-shell")]
    pub(crate) fn human(message: impl Into<String>, exit_code: u8) -> Self {
        Self::Human {
            message: message.into(),
            exit_code,
        }
    }

    pub const fn diagnostic_code(&self) -> u16 {
        match self {
            Self::Usage => 1,
            Self::InputTooLarge => 2,
            Self::Human { .. } => 12,
            Self::InvalidUtf8(_) => 3,
            Self::Ason(_) => 4,
            Self::Protocol(ProtocolReadError::InvalidUtf8 { .. }) => 3,
            Self::Protocol(ProtocolReadError::Ason(_)) => 4,
            Self::Frame(FrameError::Io(_))
            | Self::Protocol(ProtocolReadError::Frame(FrameError::Io(_)))
            | Self::Platform(PlatformError::Io(_))
            | Self::Io(_) => 9,
            Self::Frame(_)
            | Self::Protocol(ProtocolReadError::Frame(_) | ProtocolReadError::NonCanonical) => 5,
            Self::Handshake(_)
            | Self::Request(_)
            | Self::Platform(
                PlatformError::InvalidLogicalPath
                | PlatformError::WorkspaceEscape
                | PlatformError::InvalidWorkspace,
            ) => 6,
            Self::MissingHandshake => 7,
            Self::Update(_) | Self::Network(_) => 11,
            Self::Build(_)
            | Self::Response(_)
            | Self::Engine(_)
            | Self::Operation(_)
            | Self::Task(_)
            | Self::Platform(_) => 10,
        }
    }

    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage => 2,
            Self::InputTooLarge | Self::InvalidUtf8(_) | Self::Ason(_) => 3,
            Self::Human { exit_code, .. } => *exit_code,
            Self::Frame(_)
            | Self::Protocol(_)
            | Self::Handshake(_)
            | Self::Request(_)
            | Self::Platform(
                PlatformError::InvalidLogicalPath
                | PlatformError::WorkspaceEscape
                | PlatformError::InvalidWorkspace,
            )
            | Self::MissingHandshake => 4,
            Self::Network(_) => 69,
            Self::Io(_)
            | Self::Build(_)
            | Self::Response(_)
            | Self::Engine(_)
            | Self::Platform(_)
            | Self::Operation(_)
            | Self::Task(_)
            | Self::Update(_) => 70,
        }
    }

    pub async fn emit(&self) {
        if let Self::Human { message, .. } = self {
            let diagnostic = format!("ash: {message}\n");
            let mut stderr = stderr();
            let _ = stderr.write_all(diagnostic.as_bytes()).await;
            let _ = stderr.flush().await;
            return;
        }
        let diagnostic = format!("s:1\ne{{c}}:\n{}\n", self.diagnostic_code());
        let mut stderr = stderr();
        let _ = stderr.write_all(diagnostic.as_bytes()).await;
        let _ = stderr.flush().await;
    }
}
