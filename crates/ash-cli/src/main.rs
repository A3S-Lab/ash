#![forbid(unsafe_code)]

mod ason_command;
mod cli_error;
mod execution;
mod rpc;
mod run_command;

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
        [command] if command == "--build-info" => build_info().await,
        [command] if command == "ason" => ason_command::run().await,
        [command] if command == "run" => run_command::run().await,
        [command] if command == "rpc" => rpc::run().await,
        _ => Err(CliError::Usage),
    }
}

async fn build_info() -> Result<(), CliError> {
    let output = format!(
        "v:{}\nt:{}\np:1\na:1\n",
        env!("CARGO_PKG_VERSION"),
        build_target()
    );
    let mut stdout = stdout();
    stdout.write_all(output.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}

fn build_target() -> &'static str {
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
