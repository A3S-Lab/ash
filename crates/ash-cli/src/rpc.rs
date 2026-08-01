use ash_engine::Parallelism;
use ash_ops::PortableOperations;
use ash_protocol::ason::Limits;
use ash_protocol::frame::FrameCodec;
use ash_protocol::handshake::{HandshakeRequest, ServerHandshake};
use ash_protocol::request::Request;
use tokio::io::{AsyncWriteExt, stdin, stdout};

use crate::cli_error::CliError;
use crate::execution::{ExecutionSession, invalid_request};

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
    let parallelism = Parallelism::detected();
    let response = ServerHandshake {
        operation_mask: PortableOperations::operation_mask(),
        ..ServerHandshake::default()
    }
    .negotiate(&request, 1)?;
    let execution = ExecutionSession::open(
        response.session_id(),
        request.workspace(),
        u64::from(response.output_bytes()),
        parallelism,
    )?;
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
    while let Some(document) = session_codec
        .read_document(&mut stdin, &session_limits)
        .await?
    {
        let response = match Request::decode(&document) {
            Ok(request) => execution.execute(&request).await?,
            Err(error) => match Request::id_hint(&document) {
                Some(request_id) => invalid_request(request_id)?,
                None => return Err(error.into()),
            },
        };
        session_codec
            .write_document(&mut stdout, &response.encode()?)
            .await?;
        stdout.flush().await?;
    }
    execution.close()
}
