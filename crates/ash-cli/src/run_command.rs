use ash_engine::Parallelism;
use ash_protocol::ason::{Limits, decode_with_limits};
use ash_protocol::frame::HARD_MAX_FRAME_BYTES;
use ash_protocol::request::Request;
use tokio::io::{AsyncReadExt, AsyncWriteExt, stdin, stdout};

use crate::cli_error::CliError;
use crate::execution::{ExecutionSession, invalid_request};

pub async fn run() -> Result<(), CliError> {
    let mut input = Vec::new();
    stdin()
        .take((HARD_MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .await?;
    if input.len() > HARD_MAX_FRAME_BYTES {
        return Err(CliError::InputTooLarge);
    }
    let text = String::from_utf8(input)?;
    let document = decode_with_limits(&text, &Limits::default())?;
    if document.encode() != text {
        return Err(CliError::Protocol(
            ash_protocol::frame::ProtocolReadError::NonCanonical,
        ));
    }
    let response = match Request::decode(&document) {
        Ok(request) => {
            let parallelism = Parallelism::detected();
            let execution = ExecutionSession::open(
                1,
                ".",
                1024 * 1024,
                parallelism,
                ExecutionSession::capability_mask(),
            )?;
            execution.execute(&request).await?
        }
        Err(error) => match Request::id_hint(&document) {
            Some(request_id) => invalid_request(request_id)?,
            None => return Err(error.into()),
        },
    };
    let encoded = response.encode()?.encode();
    let mut stdout = stdout();
    stdout.write_all(encoded.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}
