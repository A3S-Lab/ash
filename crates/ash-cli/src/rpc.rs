use ash_protocol::ason::Limits;
use ash_protocol::frame::FrameCodec;
use ash_protocol::handshake::{HandshakeRequest, ServerHandshake};
use tokio::io::{AsyncWriteExt, stdin, stdout};

use crate::cli_error::CliError;

pub async fn run() -> Result<(), CliError> {
    let bootstrap_codec = FrameCodec::default();
    let limits = Limits {
        max_bytes: bootstrap_codec.max_payload(),
        ..Limits::default()
    };
    let mut stdin = stdin();
    let mut stdout = stdout();

    let document = bootstrap_codec
        .read_document(&mut stdin, &limits)
        .await?
        .ok_or(CliError::MissingHandshake)?;
    let request = HandshakeRequest::decode(&document)?;
    let response = ServerHandshake::default().negotiate(&request, 1)?;
    let response_document = response.encode()?;
    bootstrap_codec
        .write_document(&mut stdout, &response_document)
        .await?;
    stdout.flush().await?;

    let session_codec = FrameCodec::new(response.frame_bytes() as usize)?;
    let session_limits = Limits {
        max_bytes: session_codec.max_payload(),
        ..Limits::default()
    };
    match session_codec
        .read_document(&mut stdin, &session_limits)
        .await?
    {
        None => Ok(()),
        Some(_) => Err(CliError::UnsupportedMessage),
    }
}
