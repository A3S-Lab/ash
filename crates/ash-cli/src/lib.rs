#![forbid(unsafe_code)]

//! Embeddable entrypoints for the ash machine shell.

mod ason_command;
mod cli_error;
mod execution;
mod rpc;
mod run_command;
mod self_command;

use std::process::ExitCode;

use ash_engine::Parallelism;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, stdout};

pub use cli_error::CliError;
pub use execution::ExecutionSession;

/// Runs the command selected by the process argument vector.
pub async fn run_cli() -> Result<(), CliError> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [command] if command == "--version" || command == "version" => version().await,
        [command] if command == "--build-info" => build_info().await,
        [command] if command == "ason" => ason_command::run().await,
        [command] if command == "run" => run_command::run().await,
        [command] if command == "rpc" => rpc::run().await,
        [command, tail @ ..] if command == "self" => self_command::run(tail).await,
        _ => Err(CliError::Usage),
    }
}

/// Serves one framed ASH/1 session over caller-owned asynchronous streams.
///
/// The standalone `ash rpc` command uses the same function with stdio. An
/// embedding harness can supply pipes, sockets, or an in-memory transport while
/// preserving the production handshake, admission, ordering, and cancellation
/// path.
pub async fn serve_rpc<R, W>(reader: R, writer: W, parallelism: Parallelism) -> Result<(), CliError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    rpc::serve(reader, writer, parallelism).await
}

async fn build_info() -> Result<(), CliError> {
    let output = format!(
        "v:{}\nt:{}\np:1\na:1\nk:{}\nc:{}\n",
        env!("CARGO_PKG_VERSION"),
        build_target(),
        self_command::trust_fingerprint(),
        build_commit(),
    );
    let mut stdout = stdout();
    stdout.write_all(output.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

pub(crate) fn build_commit() -> &'static str {
    option_env!("ASH_BUILD_COMMIT")
        .filter(|value| {
            value.len() == 40
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .unwrap_or("~")
}

pub(crate) fn build_target() -> &'static str {
    if cfg!(all(
        target_arch = "x86_64",
        target_os = "linux",
        target_env = "musl"
    )) {
        "x86_64-unknown-linux-musl"
    } else if cfg!(all(
        target_arch = "aarch64",
        target_os = "linux",
        target_env = "musl"
    )) {
        "aarch64-unknown-linux-musl"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_arch = "aarch64", target_os = "windows")) {
        "aarch64-pc-windows-msvc"
    } else {
        "unsupported"
    }
}

async fn version() -> Result<(), CliError> {
    let output = format!("ash {}\n", env!("CARGO_PKG_VERSION"));
    let mut stdout = stdout();
    stdout.write_all(output.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

/// Converts the CLI result into the stable process exit contract.
pub async fn entrypoint() -> ExitCode {
    match run_cli().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error.emit().await;
            ExitCode::from(error.exit_code())
        }
    }
}
