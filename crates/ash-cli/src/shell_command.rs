use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

use ash_shell::{ShellState, execute_source};
use tokio::io::{AsyncReadExt, AsyncWriteExt, stderr, stdin, stdout};

use crate::CliError;

const MAX_SHELL_SOURCE_BYTES: u64 = 1024 * 1024;

pub async fn run(arguments: &[OsString]) -> Result<ExitCode, CliError> {
    let source = source(arguments).await?;
    let source = source
        .to_str()
        .ok_or_else(|| CliError::human("shell source must be valid UTF-8", 2))?;
    let mut state = ShellState::from_process()
        .map_err(|error| CliError::human(format!("cannot initialize shell state: {error}"), 1))?;
    let execution = execute_source(source, &mut state);

    let mut stdout = stdout();
    stdout
        .write_all(execution.stdout())
        .await
        .map_err(|error| CliError::human(format!("cannot write shell output: {error}"), 1))?;
    stdout
        .flush()
        .await
        .map_err(|error| CliError::human(format!("cannot flush shell output: {error}"), 1))?;

    let rendered = execution.rendered_stderr();
    if !rendered.is_empty() {
        let mut stderr = stderr();
        stderr.write_all(&rendered).await.map_err(|error| {
            CliError::human(format!("cannot write shell diagnostic: {error}"), 1)
        })?;
        stderr.flush().await.map_err(|error| {
            CliError::human(format!("cannot flush shell diagnostic: {error}"), 1)
        })?;
    }
    Ok(exit_code(execution.status().code()))
}

async fn source(arguments: &[OsString]) -> Result<OsString, CliError> {
    match arguments {
        [] => read_stdin().await,
        [flag] if flag == "--no-profile" => read_stdin().await,
        [flag, source] if flag == "-c" => Ok(source.clone()),
        [profile, flag, source] if profile == "--no-profile" && flag == "-c" => Ok(source.clone()),
        [flag, source, profile] if flag == "-c" && profile == "--no-profile" => Ok(source.clone()),
        _ => Err(CliError::human(
            "usage: ash shell [--no-profile] [-c SOURCE]",
            2,
        )),
    }
}

async fn read_stdin() -> Result<OsString, CliError> {
    if std::io::stdin().is_terminal() {
        return Err(CliError::human(
            "interactive shell mode is not implemented yet; use `ash shell -c SOURCE`",
            2,
        ));
    }
    let mut bytes = Vec::new();
    stdin()
        .take(MAX_SHELL_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| CliError::human(format!("cannot read shell source: {error}"), 1))?;
    if bytes.len() as u64 > MAX_SHELL_SOURCE_BYTES {
        return Err(CliError::human(
            "shell source exceeds the 1 MiB input ceiling",
            2,
        ));
    }
    String::from_utf8(bytes)
        .map(OsString::from)
        .map_err(|_| CliError::human("shell source must be valid UTF-8", 2))
}

fn exit_code(code: i64) -> ExitCode {
    if code >= 0 && code <= u8::MAX as i64 {
        ExitCode::from(code as u8)
    } else {
        ExitCode::FAILURE
    }
}
