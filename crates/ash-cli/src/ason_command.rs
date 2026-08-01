use ash_protocol::ason::{Limits, canonicalize};
use ash_protocol::frame::HARD_MAX_FRAME_BYTES;
use tokio::io::{AsyncReadExt, AsyncWriteExt, stdin, stdout};

use crate::cli_error::CliError;

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
    let canonical = canonicalize(&text, &Limits::default())?;
    let mut stdout = stdout();
    stdout.write_all(canonical.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}
