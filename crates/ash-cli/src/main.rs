#![forbid(unsafe_code)]

mod ason_command;
mod cli_error;
mod rpc;

use std::process::ExitCode;

use tokio::io::{AsyncWriteExt, stdout};

use cli_error::CliError;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error.emit().await;
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run() -> Result<(), CliError> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [command] if command == "--version" || command == "version" => version().await,
        [command] if command == "ason" => ason_command::run().await,
        [command] if command == "rpc" => rpc::run().await,
        _ => Err(CliError::Usage),
    }
}

async fn version() -> Result<(), CliError> {
    let output = format!("ash {}\n", env!("CARGO_PKG_VERSION"));
    let mut stdout = stdout();
    stdout.write_all(output.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}
