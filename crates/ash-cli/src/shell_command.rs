use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use ash_shell::{ShellState, execute_source};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, stderr, stdin, stdout};

use crate::CliError;

const MAX_SHELL_SOURCE_BYTES: u64 = 1024 * 1024;

pub async fn run(arguments: &[OsString]) -> Result<ExitCode, CliError> {
    let source = source(arguments).await?;
    let mut state = ShellState::from_process()
        .map_err(|error| CliError::human(format!("cannot initialize shell state: {error}"), 1))?;
    let execution = execute_source(&source, &mut state);

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

async fn source(arguments: &[OsString]) -> Result<String, CliError> {
    match arguments {
        [] => read_stdin().await,
        [flag] if flag == "--no-profile" => read_stdin().await,
        [flag, source] if flag == "-c" => inline_source(source),
        [profile, flag, source] if profile == "--no-profile" && flag == "-c" => {
            inline_source(source)
        }
        [flag, source, profile] if flag == "-c" && profile == "--no-profile" => {
            inline_source(source)
        }
        [separator, path] if separator == "--" => read_script(path).await,
        [profile, separator, path] if profile == "--no-profile" && separator == "--" => {
            read_script(path).await
        }
        [path] if is_script_path(path) => read_script(path).await,
        [profile, path] if profile == "--no-profile" && is_script_path(path) => {
            read_script(path).await
        }
        [path, profile] if profile == "--no-profile" && is_script_path(path) => {
            read_script(path).await
        }
        _ => Err(CliError::human(
            "usage: ash shell [--no-profile] [-c SOURCE | FILE]",
            2,
        )),
    }
}

fn inline_source(source: &OsStr) -> Result<String, CliError> {
    source
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| CliError::human("shell source must be valid UTF-8", 2))
}

fn is_script_path(argument: &OsStr) -> bool {
    !argument.to_string_lossy().starts_with('-')
}

async fn read_stdin() -> Result<String, CliError> {
    if std::io::stdin().is_terminal() {
        return Err(CliError::human(
            "interactive shell mode is not implemented yet; use `ash shell -c SOURCE` or `ash shell FILE`",
            2,
        ));
    }
    read_bounded(stdin(), "cannot read shell source").await
}

async fn read_script(path: &OsStr) -> Result<String, CliError> {
    let file = File::open(Path::new(path))
        .await
        .map_err(|error| CliError::human(format!("cannot open shell script: {error}"), 1))?;
    read_bounded(file, "cannot read shell script").await
}

async fn read_bounded<R>(reader: R, read_error: &str) -> Result<String, CliError>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    reader
        .take(MAX_SHELL_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| CliError::human(format!("{read_error}: {error}"), 1))?;
    if bytes.len() as u64 > MAX_SHELL_SOURCE_BYTES {
        return Err(CliError::human(
            "shell source exceeds the 1 MiB input ceiling",
            2,
        ));
    }
    String::from_utf8(bytes).map_err(|_| CliError::human("shell source must be valid UTF-8", 2))
}

fn exit_code(code: i64) -> ExitCode {
    if code >= 0 && code <= u8::MAX as i64 {
        ExitCode::from(code as u8)
    } else {
        ExitCode::FAILURE
    }
}
