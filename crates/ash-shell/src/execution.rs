use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;

use ash_ops::{
    ListQuery, MAX_READ_FILE_BYTES, NativeFileSystem, ReadQuery, SemanticEntryKind,
    SemanticFileSystem, SemanticListFilter, SemanticReadMode, SemanticServices,
};

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
/// slice implements `pwd`, `echo`, `cd`, and bounded portable `ls` and `cat`
/// commands; other resolved command categories produce explicit diagnostics
/// without invoking a host shell.
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
        ResolvedCommand::Portable(PortableCommand::List) => {
            execute_ls(state, arguments, span, stdout, diagnostics)
        }
        ResolvedCommand::Portable(PortableCommand::Cat) => {
            execute_cat(state, arguments, span, stdout, diagnostics)
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

#[derive(Default)]
struct ListCommandOptions {
    include_hidden: bool,
    directory: bool,
    path: Option<OsString>,
}

fn execute_ls(
    state: &ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    stdout: &mut Vec<u8>,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let options = match parse_ls_options(arguments) {
        Ok(options) => options,
        Err(message) => return invalid_arguments(&message, span, diagnostics),
    };
    let target = options.path.unwrap_or_else(|| OsString::from("."));
    if target.is_empty() {
        return filesystem_failure("cannot list: path is empty".to_owned(), span, diagnostics);
    }

    let filesystem = match NativeFileSystem::new(state.cwd()) {
        Ok(filesystem) => filesystem,
        Err(error) => {
            return filesystem_failure(
                format!("cannot list from the current directory: {error}"),
                span,
                diagnostics,
            );
        }
    };
    let path = PathBuf::from(&target);
    let resolved = match filesystem.resolve_existing(&path) {
        Ok(resolved) => resolved,
        Err(error) => {
            return list_filesystem_failure(&target, error, span, diagnostics);
        }
    };
    let root = filesystem.semantic_path(&resolved);
    let services = SemanticServices::new(filesystem);
    let result = match services.list_serial(&ListQuery::new(
        vec![path],
        if options.directory { 0 } else { 1 },
        options.include_hidden,
        SemanticListFilter::All,
    )) {
        Ok(result) => result,
        Err(error) => {
            return list_filesystem_failure(&target, error, span, diagnostics);
        }
    };
    let target_is_directory = result.entries.iter().any(|entry| {
        entry.path.stable_sort_key() == root.stable_sort_key()
            && entry.kind == SemanticEntryKind::Directory
    });
    if options.directory || !target_is_directory {
        push_os_string(stdout, &target);
        stdout.push(b'\n');
        return ShellStatus::success();
    }

    for entry in result
        .entries
        .iter()
        .filter(|entry| entry.path.stable_sort_key() != root.stable_sort_key())
    {
        let name = entry
            .path
            .as_path()
            .file_name()
            .unwrap_or_else(|| entry.path.as_path().as_os_str());
        push_os_string(stdout, name);
        stdout.push(b'\n');
    }
    ShellStatus::success()
}

fn parse_ls_options(arguments: &[OsString]) -> Result<ListCommandOptions, String> {
    let mut options = ListCommandOptions::default();
    let mut options_ended = false;
    for argument in arguments {
        if !options_ended {
            if argument == "--" {
                options_ended = true;
                continue;
            }
            match argument.to_str() {
                Some("--all") => {
                    options.include_hidden = true;
                    continue;
                }
                Some("--directory") => {
                    options.directory = true;
                    continue;
                }
                Some(value) if value.starts_with('-') && value != "-" => {
                    if value.starts_with("--")
                        || value.as_bytes()[1..]
                            .iter()
                            .any(|option| !matches!(option, b'a' | b'd' | b'1'))
                    {
                        return Err(format!(
                            "ls does not support option `{}`",
                            display_os_string(argument)
                        ));
                    }
                    options.include_hidden |= value.as_bytes()[1..].contains(&b'a');
                    options.directory |= value.as_bytes()[1..].contains(&b'd');
                    continue;
                }
                _ => {}
            }
        }
        if options.path.replace(argument.clone()).is_some() {
            return Err("ls accepts at most one path".to_owned());
        }
    }
    Ok(options)
}

fn list_filesystem_failure(
    target: &OsStr,
    error: impl std::fmt::Display,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    filesystem_failure(
        format!("cannot list `{}`: {error}", display_os_string(target)),
        span,
        diagnostics,
    )
}

fn execute_cat(
    state: &ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    stdout: &mut Vec<u8>,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let target = match parse_cat_path(arguments) {
        Ok(target) => target,
        Err(message) => return invalid_arguments(&message, span, diagnostics),
    };
    if target.is_empty() {
        return filesystem_failure("cannot read: path is empty".to_owned(), span, diagnostics);
    }

    let filesystem = match NativeFileSystem::new(state.cwd()) {
        Ok(filesystem) => filesystem,
        Err(error) => {
            return filesystem_failure(
                format!("cannot read from the current directory: {error}"),
                span,
                diagnostics,
            );
        }
    };
    let remaining = MAX_READ_FILE_BYTES.saturating_sub(stdout.len() as u64);
    let services = SemanticServices::new(filesystem);
    let result = match services.read_serial_limited(
        &ReadQuery::new(
            vec![PathBuf::from(&target)],
            SemanticReadMode::Bytes,
            0,
            u64::MAX,
        ),
        remaining,
    ) {
        Ok(result) => result,
        Err(error) => {
            return read_filesystem_failure(&target, error, span, diagnostics);
        }
    };
    let Some(read) = result.reads.into_iter().next() else {
        return filesystem_failure(
            "cannot read: semantic read returned no result".to_owned(),
            span,
            diagnostics,
        );
    };
    stdout.extend_from_slice(&read.bytes);
    ShellStatus::success()
}

fn parse_cat_path(arguments: &[OsString]) -> Result<OsString, String> {
    if arguments.is_empty() || arguments == ["--"] {
        return Err("cat requires exactly one path".to_owned());
    }
    if arguments.first().is_some_and(|argument| argument == "--") {
        if arguments.len() != 2 {
            return Err("cat accepts exactly one path".to_owned());
        }
        return Ok(arguments[1].clone());
    }
    if arguments.len() != 1 {
        return Err("cat accepts exactly one path".to_owned());
    }
    let target = &arguments[0];
    if target == "-" {
        return Err("cat standard-input operand `-` is not implemented yet".to_owned());
    }
    if target
        .to_str()
        .is_some_and(|target| target.starts_with('-'))
    {
        return Err(format!(
            "cat does not support option `{}`",
            display_os_string(target)
        ));
    }
    Ok(target.clone())
}

fn read_filesystem_failure(
    target: &OsStr,
    error: impl std::fmt::Display,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    filesystem_failure(
        format!("cannot read `{}`: {error}", display_os_string(target)),
        span,
        diagnostics,
    )
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

        let unsupported = execute_source("grep", &mut state);
        assert_eq!(
            unsupported.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Unsupported
        );
        assert_eq!(unsupported.diagnostics()[0].span(), SourceSpan::new(0, 4));
        assert_eq!(unsupported.status().kind(), ShellStatusKind::Exited);
        assert_eq!(unsupported.status().code(), 2);
    }

    #[test]
    fn ls_lists_direct_children_in_stable_order_and_observes_cd() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        fs::write(directory.0.join("b.txt"), b"b").expect("write b");
        fs::write(directory.0.join("a.txt"), b"a").expect("write a");
        fs::write(directory.0.join(".hidden"), b"hidden").expect("write hidden");
        fs::write(directory.0.join("child/nested.txt"), b"nested").expect("write nested");
        let mut state = ShellState::new(&directory.0);

        let visible = execute_source("ls -1", &mut state);
        assert_eq!(visible.stdout(), b"a.txt\nb.txt\nchild\n");
        assert!(visible.diagnostics().is_empty());
        assert_eq!(visible.status().code(), 0);

        let all = execute_source("ls --all", &mut state);
        assert_eq!(all.stdout(), b".hidden\na.txt\nb.txt\nchild\n");

        let nested = execute_source("cd child; ls", &mut state);
        assert_eq!(nested.stdout(), b"nested.txt\n");
        assert!(nested.diagnostics().is_empty());
    }

    #[test]
    fn ls_supports_directory_end_of_options_and_clear_failures() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        fs::write(directory.0.join("sample.txt"), b"sample").expect("write sample");
        fs::write(directory.0.join("-dash"), b"dash").expect("write dash");
        let mut state = ShellState::new(&directory.0);

        let file = execute_source("ls sample.txt", &mut state);
        assert_eq!(file.stdout(), b"sample.txt\n");
        assert!(file.diagnostics().is_empty());

        let directory_only = execute_source("ls -ad1 child", &mut state);
        assert_eq!(directory_only.stdout(), b"child\n");
        let dash = execute_source("ls -- -dash", &mut state);
        assert_eq!(dash.stdout(), b"-dash\n");

        let option = execute_source("ls -l", &mut state);
        assert_eq!(
            option.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(
            option.diagnostics()[0].message(),
            "ls does not support option `-l`"
        );
        assert_eq!(option.status().code(), 2);

        let paths = execute_source("ls child sample.txt", &mut state);
        assert_eq!(
            paths.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(
            paths.diagnostics()[0].message(),
            "ls accepts at most one path"
        );

        let missing = execute_source("ls missing", &mut state);
        assert_eq!(
            missing.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(missing.status().code(), 1);

        let empty = execute_source("ls ''", &mut state);
        assert_eq!(
            empty.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(
            empty.diagnostics()[0].message(),
            "cannot list: path is empty"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn ls_preserves_and_stably_sorts_native_names() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("a"), b"a").expect("write a");
        fs::write(directory.0.join("雪"), b"unicode").expect("write unicode");
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;

            fs::write(
                directory.0.join(std::ffi::OsString::from_vec(vec![0x80])),
                b"middle",
            )
            .expect("write middle");
            fs::write(
                directory.0.join(std::ffi::OsString::from_vec(vec![0xff])),
                b"last",
            )
            .expect("write last");
        }
        let mut state = ShellState::new(&directory.0);

        let execution = execute_source("ls", &mut state);

        #[cfg(target_os = "linux")]
        assert_eq!(execution.stdout(), b"a\n\x80\n\xe9\x9b\xaa\n\xff\n");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(execution.stdout(), "a\n雪\n".as_bytes());
        assert!(execution.diagnostics().is_empty());
    }

    #[test]
    fn cat_emits_exact_binary_bytes_and_observes_cd() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        fs::write(directory.0.join("雪.bin"), b"root").expect("write unicode path");
        fs::write(directory.0.join("child/payload.bin"), [0x00, 0xff, b'\n'])
            .expect("write binary payload");
        let mut state = ShellState::new(&directory.0);

        let execution = execute_source("cat '雪.bin'; cd child; cat payload.bin", &mut state);

        assert_eq!(execution.stdout(), b"root\x00\xff\n");
        assert!(execution.diagnostics().is_empty());
        assert_eq!(execution.status().code(), 0);
        assert_eq!(
            state.cwd(),
            fs::canonicalize(directory.0.join("child")).expect("canonical child")
        );
    }

    #[test]
    fn cat_supports_end_of_options_and_clear_bounded_failures() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("-"), b"hyphen").expect("write hyphen");
        fs::create_dir(directory.0.join("child")).expect("create child");
        let oversized =
            fs::File::create(directory.0.join("oversized.bin")).expect("create oversized file");
        oversized
            .set_len(super::MAX_READ_FILE_BYTES + 1)
            .expect("set oversized length");
        let mut state = ShellState::new(&directory.0);

        let dash = execute_source("cat -- -", &mut state);
        assert_eq!(dash.stdout(), b"hyphen");
        assert!(dash.diagnostics().is_empty());

        let option = execute_source("cat -n", &mut state);
        assert_eq!(
            option.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(
            option.diagnostics()[0].message(),
            "cat does not support option `-n`"
        );
        assert_eq!(option.status().code(), 2);

        let stdin = execute_source("cat -", &mut state);
        assert_eq!(
            stdin.diagnostics()[0].message(),
            "cat standard-input operand `-` is not implemented yet"
        );
        assert_eq!(stdin.status().code(), 2);

        let missing_argument = execute_source("cat", &mut state);
        assert_eq!(
            missing_argument.diagnostics()[0].message(),
            "cat requires exactly one path"
        );
        let paths = execute_source("cat one two", &mut state);
        assert_eq!(
            paths.diagnostics()[0].message(),
            "cat accepts exactly one path"
        );

        for source in ["cat missing", "cat child", "cat ''", "cat oversized.bin"] {
            let failure = execute_source(source, &mut state);
            assert!(failure.stdout().is_empty(), "source={source}");
            assert_eq!(
                failure.diagnostics()[0].code(),
                ExecutionDiagnosticCode::Filesystem,
                "source={source}"
            );
            assert_eq!(failure.status().code(), 1, "source={source}");
        }
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
