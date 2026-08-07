use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;

use crate::{
    CommandResolver, DiagnosticCode, ExecutionBackend, HostPlatform, NativeCommandLookup,
    PathCommandLookup, PortableCommand, ResolutionError, ResolvedCommand, ShellState, ShellStatus,
    ShellStatusKind, SourceSpan, StatefulBuiltin, parse,
};

/// Stable category for a human-shell execution diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExecutionDiagnosticCode {
    Parse(DiagnosticCode),
    InvalidArguments,
    Resolution,
    Filesystem,
    Unsupported,
}

/// One source-spanned diagnostic produced while executing a shell command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionDiagnostic {
    code: ExecutionDiagnosticCode,
    message: String,
    span: SourceSpan,
}

impl ExecutionDiagnostic {
    #[must_use]
    pub const fn code(&self) -> ExecutionDiagnosticCode {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "ash: {} at bytes {}..{}\n",
            self.message,
            self.span.start(),
            self.span.end()
        )
    }
}

/// Complete foreground output and final status for one submitted source string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellExecution {
    stdout: Vec<u8>,
    diagnostics: Vec<ExecutionDiagnostic>,
    status: ShellStatus,
}

impl ShellExecution {
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[ExecutionDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub const fn status(&self) -> &ShellStatus {
        &self.status
    }

    #[must_use]
    pub fn rendered_stderr(&self) -> Vec<u8> {
        let capacity = self
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.len() + 40)
            .sum();
        let mut output = Vec::with_capacity(capacity);
        for diagnostic in &self.diagnostics {
            output.extend_from_slice(diagnostic.render().as_bytes());
        }
        output
    }
}

/// Parses and executes the currently implemented foreground shell subset.
///
/// Commands run sequentially against one mutable `ShellState`. This first H1
/// slice implements `pwd`, `echo`, and `cd`; other resolved command categories
/// produce explicit diagnostics without invoking a host shell.
#[must_use]
pub fn execute_source(source: &str, state: &mut ShellState) -> ShellExecution {
    execute_source_with(source, state, &PathCommandLookup, HostPlatform::current())
}

/// Injectable form of [`execute_source`] for deterministic resolver tests.
#[must_use]
pub fn execute_source_with<L>(
    source: &str,
    state: &mut ShellState,
    lookup: &L,
    host: HostPlatform,
) -> ShellExecution
where
    L: NativeCommandLookup + ?Sized,
{
    let script = match parse(source) {
        Ok(script) => script,
        Err(error) => {
            let status = shell_status(2, ShellStatusKind::ParseError);
            state.set_last_status(status.clone());
            return ShellExecution {
                stdout: Vec::new(),
                diagnostics: vec![ExecutionDiagnostic {
                    code: ExecutionDiagnosticCode::Parse(error.code()),
                    message: error.message().to_owned(),
                    span: error.span(),
                }],
                status,
            };
        }
    };

    let mut stdout = Vec::new();
    let mut diagnostics = Vec::new();
    let mut final_status = ShellStatus::success();
    for command in script.commands() {
        let words: Vec<OsString> = command
            .words()
            .iter()
            .map(|word| OsString::from(word.literal()))
            .collect();
        let Some(name) = words.first().and_then(|word| word.to_str()) else {
            continue;
        };
        let name_span = command.words()[0].span();
        let resolved = {
            let resolver = CommandResolver::for_platform(
                state,
                |command: &str, cwd: &std::path::Path, environment: &crate::PlatformEnvironment| {
                    lookup.resolve(command, cwd, environment)
                },
                host,
            );
            resolver.resolve(name)
        };
        final_status = match resolved {
            Ok(resolved) => execute_command(
                state,
                resolved,
                &words[1..],
                command.span(),
                &mut stdout,
                &mut diagnostics,
            ),
            Err(error) => resolution_failure(error, name_span, &mut diagnostics),
        };
        state.set_last_status(final_status.clone());
    }
    if script.commands().is_empty() {
        state.set_last_status(final_status.clone());
    }
    ShellExecution {
        stdout,
        diagnostics,
        status: final_status,
    }
}

fn execute_command(
    state: &mut ShellState,
    resolved: ResolvedCommand,
    arguments: &[OsString],
    span: SourceSpan,
    stdout: &mut Vec<u8>,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    match resolved {
        ResolvedCommand::StatefulBuiltin(StatefulBuiltin::Cd) => {
            execute_cd(state, arguments, span, diagnostics)
        }
        ResolvedCommand::Portable(PortableCommand::Pwd) => {
            execute_pwd(state, arguments, span, stdout, diagnostics)
        }
        ResolvedCommand::Portable(PortableCommand::Echo) => {
            execute_echo(arguments, span, stdout, diagnostics)
        }
        ResolvedCommand::StatefulBuiltin(command) => unsupported(
            format!("builtin `{}` is not implemented yet", command.name()),
            span,
            diagnostics,
            ShellStatusKind::Exited,
            2,
        ),
        ResolvedCommand::Portable(command) => unsupported(
            format!(
                "portable command `{}` is not implemented yet",
                command.name()
            ),
            span,
            diagnostics,
            ShellStatusKind::Exited,
            2,
        ),
        ResolvedCommand::Alias { name, .. } => unsupported(
            format!("alias execution for `{name}` is not implemented yet"),
            span,
            diagnostics,
            ShellStatusKind::ResolutionError,
            126,
        ),
        ResolvedCommand::Function { name } => unsupported(
            format!("function execution for `{name}` is not implemented yet"),
            span,
            diagnostics,
            ShellStatusKind::ResolutionError,
            126,
        ),
        ResolvedCommand::Native { executable, .. } => unsupported(
            format!(
                "native execution for `{}` is not implemented yet",
                display_os_string(executable.as_os_str())
            ),
            span,
            diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
        ResolvedCommand::Wsl { command, .. } => unsupported(
            format!("WSL execution for `{command}` is not implemented yet"),
            span,
            diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
    }
}

fn execute_pwd(
    state: &ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    stdout: &mut Vec<u8>,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    if !arguments.is_empty() {
        return invalid_arguments("pwd does not accept arguments", span, diagnostics);
    }
    push_os_string(stdout, state.cwd().as_os_str());
    stdout.push(b'\n');
    ShellStatus::success()
}

fn execute_echo(
    arguments: &[OsString],
    span: SourceSpan,
    stdout: &mut Vec<u8>,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let mut arguments = arguments;
    let mut newline = true;
    if arguments.first().is_some_and(|argument| argument == "-n") {
        newline = false;
        arguments = &arguments[1..];
    } else if arguments.first().is_some_and(|argument| argument == "--") {
        arguments = &arguments[1..];
    } else if arguments.first().is_some_and(|argument| {
        argument
            .to_str()
            .is_some_and(|argument| argument.starts_with('-') && argument != "-")
    }) {
        return invalid_arguments("echo supports only the `-n` option", span, diagnostics);
    }
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            stdout.push(b' ');
        }
        push_os_string(stdout, argument);
    }
    if newline {
        stdout.push(b'\n');
    }
    ShellStatus::success()
}

fn execute_cd(
    state: &mut ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    if arguments.len() > 1 {
        return invalid_arguments("cd accepts at most one path", span, diagnostics);
    }
    let target = if let Some(target) = arguments.first() {
        if target.is_empty() {
            return filesystem_failure(
                "cannot change directory: path is empty".to_owned(),
                span,
                diagnostics,
            );
        }
        if target == "-" {
            return invalid_arguments("cd - is not implemented yet", span, diagnostics);
        }
        PathBuf::from(target)
    } else {
        let home = state
            .environment()
            .get("HOME")
            .filter(|home| !home.is_empty())
            .or_else(|| {
                state
                    .environment()
                    .get("USERPROFILE")
                    .filter(|home| !home.is_empty())
            });
        let Some(home) = home else {
            return filesystem_failure(
                "cd requires HOME or USERPROFILE when no path is supplied".to_owned(),
                span,
                diagnostics,
            );
        };
        PathBuf::from(home)
    };
    let target = if target.is_absolute() {
        target
    } else {
        state.cwd().join(target)
    };
    let target = match fs::canonicalize(&target) {
        Ok(target) => target,
        Err(error) => {
            return filesystem_failure(
                format!("cannot change directory: {error}"),
                span,
                diagnostics,
            );
        }
    };
    if !target.is_dir() {
        return filesystem_failure(
            "cannot change directory: target is not a directory".to_owned(),
            span,
            diagnostics,
        );
    }
    state.set_cwd(target);
    ShellStatus::success()
}

fn resolution_failure(
    error: ResolutionError,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let code = match error {
        ResolutionError::CommandNotFound { .. } | ResolutionError::EmptyCommand => 127,
        ResolutionError::BackendUnavailable { .. } => 126,
    };
    diagnostics.push(ExecutionDiagnostic {
        code: ExecutionDiagnosticCode::Resolution,
        message: error.to_string(),
        span,
    });
    shell_status(code, ShellStatusKind::ResolutionError)
}

fn invalid_arguments(
    message: &str,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    diagnostics.push(ExecutionDiagnostic {
        code: ExecutionDiagnosticCode::InvalidArguments,
        message: message.to_owned(),
        span,
    });
    shell_status(2, ShellStatusKind::Exited)
}

fn filesystem_failure(
    message: String,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    diagnostics.push(ExecutionDiagnostic {
        code: ExecutionDiagnosticCode::Filesystem,
        message,
        span,
    });
    shell_status(1, ShellStatusKind::Exited)
}

fn unsupported(
    message: String,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
    kind: ShellStatusKind,
    code: i64,
) -> ShellStatus {
    diagnostics.push(ExecutionDiagnostic {
        code: ExecutionDiagnosticCode::Unsupported,
        message,
        span,
    });
    shell_status(code, kind)
}

fn shell_status(code: i64, kind: ShellStatusKind) -> ShellStatus {
    ShellStatus::new(code, kind, None, ExecutionBackend::Native)
}

#[cfg(unix)]
fn display_os_string(value: &OsStr) -> String {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;

    if let Some(value) = value.to_str() {
        return value.to_owned();
    }
    let mut escaped = String::new();
    for byte in value.as_bytes() {
        let _ = write!(escaped, "\\x{byte:02x}");
    }
    escaped
}

#[cfg(windows)]
fn display_os_string(value: &OsStr) -> String {
    use std::fmt::Write;
    use std::os::windows::ffi::OsStrExt;

    if let Some(value) = value.to_str() {
        return value.to_owned();
    }
    let mut escaped = String::new();
    for unit in value.encode_wide() {
        let _ = write!(escaped, "\\u{{{unit:04x}}}");
    }
    escaped
}

#[cfg(not(any(unix, windows)))]
fn display_os_string(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn push_os_string(output: &mut Vec<u8>, value: &OsStr) {
    use std::os::unix::ffi::OsStrExt;

    output.extend_from_slice(value.as_bytes());
}

#[cfg(windows)]
fn push_os_string(output: &mut Vec<u8>, value: &OsStr) {
    use std::fmt::Write;
    use std::os::windows::ffi::OsStrExt;

    if let Some(value) = value.to_str() {
        output.extend_from_slice(value.as_bytes());
        return;
    }
    let mut escaped = String::new();
    for unit in value.encode_wide() {
        let _ = write!(escaped, "\\u{{{unit:04x}}}");
    }
    output.extend_from_slice(escaped.as_bytes());
}

#[cfg(not(any(unix, windows)))]
fn push_os_string(output: &mut Vec<u8>, value: &OsStr) {
    output.extend_from_slice(value.to_string_lossy().as_bytes());
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{ExecutionDiagnosticCode, execute_source, execute_source_with};
    use crate::{HostPlatform, ShellState, ShellStatusKind, SourceSpan};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("ash-shell-execution-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pwd_echo_and_cd_share_state_across_sequential_commands() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        let mut state = ShellState::new(&directory.0);

        let execution = execute_source(
            "pwd; echo \"hello world\"; cd child; pwd; echo -n done",
            &mut state,
        );

        let child = fs::canonicalize(directory.0.join("child")).expect("canonical child");
        let expected = format!(
            "{}\nhello world\n{}\ndone",
            directory.0.display(),
            child.display()
        );
        assert_eq!(execution.stdout(), expected.as_bytes());
        assert!(execution.diagnostics().is_empty());
        assert_eq!(execution.status().code(), 0);
        assert_eq!(state.cwd(), child);
    }

    #[test]
    fn failed_resolution_is_typed_and_later_commands_still_run() {
        let directory = TestDirectory::new();
        let mut state = ShellState::new(&directory.0);
        let lookup = |_command: &str,
                      _cwd: &std::path::Path,
                      _environment: &crate::PlatformEnvironment| None;

        let execution = execute_source_with(
            "missing; echo recovered",
            &mut state,
            &lookup,
            HostPlatform::Linux,
        );

        assert_eq!(execution.stdout(), b"recovered\n");
        assert_eq!(execution.diagnostics().len(), 1);
        assert_eq!(
            execution.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Resolution
        );
        assert_eq!(execution.status().code(), 0);
        assert_eq!(state.last_status().code(), 0);
    }

    #[test]
    fn parse_argument_filesystem_and_unsupported_failures_are_distinct() {
        let mut state = ShellState::new(".");
        let parse = execute_source("echo 'unterminated", &mut state);
        assert!(matches!(
            parse.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Parse(_)
        ));
        assert_eq!(parse.status().kind(), ShellStatusKind::ParseError);
        assert_eq!(parse.status().code(), 2);

        let builtin = execute_source("pwd extra", &mut state);
        assert_eq!(
            builtin.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(builtin.status().kind(), ShellStatusKind::Exited);
        assert_eq!(builtin.status().code(), 2);

        let filesystem = execute_source("cd ''", &mut state);
        assert_eq!(
            filesystem.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(filesystem.diagnostics()[0].span(), SourceSpan::new(0, 5));
        assert_eq!(filesystem.status().kind(), ShellStatusKind::Exited);
        assert_eq!(filesystem.status().code(), 1);

        state.environment_mut().insert("HOME", "");
        let empty_home = execute_source("cd", &mut state);
        assert_eq!(
            empty_home.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(empty_home.status().code(), 1);

        let unsupported = execute_source("ls", &mut state);
        assert_eq!(
            unsupported.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Unsupported
        );
        assert_eq!(unsupported.diagnostics()[0].span(), SourceSpan::new(0, 2));
        assert_eq!(unsupported.status().kind(), ShellStatusKind::Exited);
        assert_eq!(unsupported.status().code(), 2);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn pwd_preserves_or_reversibly_escapes_native_path_units() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};

            let path = PathBuf::from(std::ffi::OsString::from_vec(b"native-\xff-path".to_vec()));
            let mut state = ShellState::new(&path);
            let execution = execute_source("pwd", &mut state);

            assert_eq!(execution.stdout(), b"native-\xff-path\n");
            assert_eq!(state.cwd().as_os_str().as_bytes(), b"native-\xff-path");
        }

        #[cfg(windows)]
        {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};

            let units = [
                b'n' as u16,
                b'a' as u16,
                b't' as u16,
                b'i' as u16,
                b'v' as u16,
                b'e' as u16,
                b'-' as u16,
                0xd800,
                b'-' as u16,
                b'p' as u16,
                b'a' as u16,
                b't' as u16,
                b'h' as u16,
            ];
            let path = PathBuf::from(std::ffi::OsString::from_wide(&units));
            let mut state = ShellState::new(&path);
            let execution = execute_source("pwd", &mut state);

            assert_eq!(
                execution.stdout(),
                b"\\u{006e}\\u{0061}\\u{0074}\\u{0069}\\u{0076}\\u{0065}\\u{002d}\\u{d800}\\u{002d}\\u{0070}\\u{0061}\\u{0074}\\u{0068}\n"
            );
            assert_eq!(
                state.cwd().as_os_str().encode_wide().collect::<Vec<_>>(),
                units.to_vec()
            );
        }
    }
}
