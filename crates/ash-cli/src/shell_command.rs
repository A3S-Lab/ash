use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ash_shell::{
    ExecutionBackend, InteractiveConfig, InteractiveEditor, InteractiveError, InteractiveEvent,
    ShellExecution, ShellState, ShellStatus, ShellStatusKind, execute_source,
};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, stderr, stdin, stdout};

use crate::CliError;

const MAX_SHELL_SOURCE_BYTES: u64 = 1024 * 1024;
const SHELL_USAGE: &str = "usage: ash shell [--no-profile | --profile FILE] [-c SOURCE | FILE]";

#[derive(Clone, Debug, Eq, PartialEq)]
enum ShellInput {
    Automatic,
    Inline(String),
    Script(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProfileMode {
    Configured,
    Disabled,
    Explicit(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ShellInvocation {
    input: ShellInput,
    profile: ProfileMode,
}

impl ShellInvocation {
    fn parse(arguments: &[OsString]) -> Result<Self, CliError> {
        let mut input = None;
        let mut profile = ProfileMode::Configured;
        let mut profile_selected = false;
        let mut options = true;
        let mut index = 0;

        while index < arguments.len() {
            let argument = &arguments[index];
            if options && argument == "--" {
                options = false;
                index += 1;
                continue;
            }
            if options && argument == "--no-profile" {
                if profile_selected {
                    return Err(usage());
                }
                profile = ProfileMode::Disabled;
                profile_selected = true;
                index += 1;
                continue;
            }
            if options && argument == "--profile" {
                if profile_selected || index + 1 == arguments.len() {
                    return Err(usage());
                }
                profile = ProfileMode::Explicit(PathBuf::from(&arguments[index + 1]));
                profile_selected = true;
                index += 2;
                continue;
            }
            if options && argument == "-c" {
                if input.is_some() || index + 1 == arguments.len() {
                    return Err(usage());
                }
                input = Some(ShellInput::Inline(inline_source(&arguments[index + 1])?));
                index += 2;
                continue;
            }
            if options && argument.to_string_lossy().starts_with('-') {
                return Err(usage());
            }
            if input.is_some() {
                return Err(usage());
            }
            input = Some(ShellInput::Script(PathBuf::from(argument)));
            index += 1;
        }

        Ok(Self {
            input: input.unwrap_or(ShellInput::Automatic),
            profile,
        })
    }
}

pub async fn run(arguments: &[OsString]) -> Result<ExitCode, CliError> {
    let invocation = ShellInvocation::parse(arguments)?;
    let mut state = ShellState::from_process()
        .map_err(|error| CliError::human(format!("cannot initialize shell state: {error}"), 1))?;
    let initial_cwd = state.cwd().to_owned();
    let interactive =
        matches!(&invocation.input, ShellInput::Automatic) && std::io::stdin().is_terminal();

    if let Some(path) = profile_path(&invocation.profile, &state, &initial_cwd) {
        let source = read_profile(&path).await?;
        let execution = execute_shell_source(&source, &mut state).await;
        write_execution(&execution).await?;
        if execution.exit_requested().is_some() {
            return Ok(exit_code(execution.status().code()));
        }
        if execution.status().kind() == ShellStatusKind::ParseError && !interactive {
            return Ok(exit_code(execution.status().code()));
        }
    }

    if interactive {
        return run_interactive(&mut state, &initial_cwd).await;
    }

    let source = match invocation.input {
        ShellInput::Automatic => read_stdin().await?,
        ShellInput::Inline(source) => source,
        ShellInput::Script(path) => {
            let path = resolve_from_initial_cwd(path, &initial_cwd);
            read_script(&path).await?
        }
    };
    let execution = execute_shell_source(&source, &mut state).await;
    write_execution(&execution).await?;
    Ok(exit_code(execution.status().code()))
}

async fn run_interactive(state: &mut ShellState, initial_cwd: &Path) -> Result<ExitCode, CliError> {
    let config = InteractiveConfig::from_environment(state.environment(), initial_cwd)
        .map_err(interactive_error)?;
    let mut editor = InteractiveEditor::new(config).map_err(interactive_error)?;
    write_editor_warnings(&mut editor).await?;

    loop {
        let event =
            tokio::task::block_in_place(|| editor.read_line()).map_err(interactive_error)?;
        write_editor_warnings(&mut editor).await?;
        match event {
            InteractiveEvent::Line(source) => {
                let execution = execute_shell_source(&source, state).await;
                write_execution(&execution).await?;
                if execution.exit_requested().is_some() {
                    return Ok(exit_code(execution.status().code()));
                }
            }
            InteractiveEvent::Interrupted => {
                state.set_last_status(ShellStatus::new(
                    130,
                    ShellStatusKind::Interrupted,
                    None,
                    ExecutionBackend::Native,
                ));
            }
            InteractiveEvent::EndOfFile => {
                return Ok(exit_code(state.last_status().code()));
            }
            _ => {}
        }
    }
}

async fn execute_shell_source(source: &str, state: &mut ShellState) -> ShellExecution {
    // Native execution owns sizeable capture buffers inside its future. Keep
    // that state on the heap so the REPL branch cannot exhaust the smaller
    // default main-thread stack used by Windows executables.
    Box::pin(execute_source(source, state)).await
}

async fn write_editor_warnings(editor: &mut InteractiveEditor) -> Result<(), CliError> {
    let warnings = editor.take_warnings();
    if warnings.is_empty() {
        return Ok(());
    }
    let rendered = warnings
        .into_iter()
        .map(|warning| format!("ash: warning: {warning}\n"))
        .collect::<String>();
    write_shell_stderr(rendered.as_bytes(), "cannot write shell warning").await
}

fn interactive_error(error: InteractiveError) -> CliError {
    let exit_code = match &error {
        InteractiveError::PromptNotUtf8 => 2,
        _ => 1,
    };
    CliError::human(error.to_string(), exit_code)
}

fn profile_path(mode: &ProfileMode, state: &ShellState, initial_cwd: &Path) -> Option<PathBuf> {
    let path = match mode {
        ProfileMode::Configured => state
            .environment()
            .get("ASH_PROFILE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        ProfileMode::Disabled => None,
        ProfileMode::Explicit(path) => Some(path.clone()),
    }?;
    Some(resolve_from_initial_cwd(path, initial_cwd))
}

fn resolve_from_initial_cwd(path: PathBuf, initial_cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        initial_cwd.join(path)
    }
}

fn inline_source(source: &OsStr) -> Result<String, CliError> {
    source
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| CliError::human("shell source must be valid UTF-8", 2))
}

async fn read_stdin() -> Result<String, CliError> {
    read_bounded(stdin(), "cannot read shell source", "shell source").await
}

async fn read_script(path: &Path) -> Result<String, CliError> {
    let file = File::open(path)
        .await
        .map_err(|error| CliError::human(format!("cannot open shell script: {error}"), 1))?;
    read_bounded(file, "cannot read shell script", "shell source").await
}

async fn read_profile(path: &Path) -> Result<String, CliError> {
    let file = File::open(path)
        .await
        .map_err(|error| CliError::human(format!("cannot open shell profile: {error}"), 1))?;
    read_bounded(file, "cannot read shell profile", "shell profile").await
}

async fn read_bounded<R>(
    reader: R,
    read_error: &str,
    source_description: &str,
) -> Result<String, CliError>
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
            format!("{source_description} exceeds the 1 MiB input ceiling"),
            2,
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| CliError::human(format!("{source_description} must be valid UTF-8"), 2))
}

async fn write_execution(execution: &ShellExecution) -> Result<(), CliError> {
    let mut process_stdout = stdout();
    process_stdout
        .write_all(execution.stdout())
        .await
        .map_err(|error| CliError::human(format!("cannot write shell output: {error}"), 1))?;
    process_stdout
        .flush()
        .await
        .map_err(|error| CliError::human(format!("cannot flush shell output: {error}"), 1))?;

    if !execution.stderr().is_empty() {
        write_shell_stderr(execution.stderr(), "cannot write shell diagnostic").await?;
    }
    Ok(())
}

async fn write_shell_stderr(bytes: &[u8], write_error: &str) -> Result<(), CliError> {
    let mut process_stderr = stderr();
    process_stderr
        .write_all(bytes)
        .await
        .map_err(|error| CliError::human(format!("{write_error}: {error}"), 1))?;
    process_stderr
        .flush()
        .await
        .map_err(|error| CliError::human(format!("cannot flush shell diagnostic: {error}"), 1))
}

fn usage() -> CliError {
    CliError::human(SHELL_USAGE, 2)
}

fn exit_code(code: i64) -> ExitCode {
    if code >= 0 && code <= u8::MAX as i64 {
        ExitCode::from(code as u8)
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{ProfileMode, SHELL_USAGE, ShellInput, ShellInvocation, profile_path};
    use crate::CliError;
    use ash_shell::ShellState;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn assert_usage(values: &[&str]) {
        let error = ShellInvocation::parse(&arguments(values)).expect_err("usage error");
        assert!(matches!(
            error,
            CliError::Human { ref message, exit_code: 2 } if message == SHELL_USAGE
        ));
    }

    #[test]
    fn invocation_accepts_profile_options_before_or_after_input() {
        let first = ShellInvocation::parse(&arguments(&[
            "--profile",
            "profile.ash",
            "-c",
            "echo ready",
        ]))
        .expect("invocation");
        assert_eq!(
            first,
            ShellInvocation {
                input: ShellInput::Inline("echo ready".to_owned()),
                profile: ProfileMode::Explicit(PathBuf::from("profile.ash")),
            }
        );

        let second = ShellInvocation::parse(&arguments(&["script.ash", "--no-profile"]))
            .expect("invocation");
        assert_eq!(
            second,
            ShellInvocation {
                input: ShellInput::Script(PathBuf::from("script.ash")),
                profile: ProfileMode::Disabled,
            }
        );
    }

    #[test]
    fn invocation_honors_option_termination_for_dash_paths() {
        let invocation =
            ShellInvocation::parse(&arguments(&["--", "-script.ash"])).expect("invocation");
        assert_eq!(
            invocation.input,
            ShellInput::Script(PathBuf::from("-script.ash"))
        );
    }

    #[test]
    fn invocation_rejects_conflicts_duplicates_and_missing_values() {
        for values in [
            &["--profile"][..],
            &["-c"][..],
            &["--profile", "one", "--profile", "two"][..],
            &["--profile", "one", "--no-profile"][..],
            &["--no-profile", "--no-profile"][..],
            &["-c", "one", "two"][..],
            &["one", "two"][..],
            &["--unknown"][..],
        ] {
            assert_usage(values);
        }
    }

    #[test]
    fn configured_profile_is_opt_in_and_anchored_to_initial_cwd() {
        let initial = if cfg!(windows) {
            Path::new(r"C:\initial")
        } else {
            Path::new("/initial")
        };
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\initial\config\profile.ash")
        } else {
            PathBuf::from("/initial/config/profile.ash")
        };
        let mut state = ShellState::new("/changed");
        assert_eq!(
            profile_path(&ProfileMode::Configured, &state, initial),
            None
        );

        state
            .environment_mut()
            .insert("ASH_PROFILE", "config/profile.ash");
        assert_eq!(
            profile_path(&ProfileMode::Configured, &state, initial),
            Some(expected)
        );
        assert_eq!(profile_path(&ProfileMode::Disabled, &state, initial), None);
    }
}
