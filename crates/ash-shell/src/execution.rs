use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ash_ops::{
    ListQuery, MAX_READ_FILE_BYTES, MAX_SEARCH_FILE_BYTES, NativeFileSystem, ReadQuery,
    SearchQuery, SemanticEntryKind, SemanticError, SemanticFileSystem, SemanticListFilter,
    SemanticReadMode, SemanticSearchPattern, SemanticServices,
};
use ash_platform::{
    ClosedProcessPipeEnd, EnvironmentChange, NativeProcessFile, NativeProcessFileMode,
    NativeProcessGraph, NativeProcessSpec, ParentProcessFile, ParentProcessFileId,
    ParentProcessPipeEnd, PlatformError, ProcessCaptureId, ProcessExit, ProcessFileId,
    ProcessGraphFile, ProcessPipeId, ProcessStdio, spawn_native, spawn_native_graph_with_parent_io,
};
use futures::future::{join_all, try_join_all};
use regex::{Regex, RegexBuilder};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::expand::expand_words;
use crate::state::validate_identifier;
use crate::{
    CommandResolver, DiagnosticCode, ExecutionBackend, HostPlatform, NativeCommandLookup,
    PathCommandLookup, PortableCommand, Redirection, RedirectionDescriptor, RedirectionFileMode,
    RedirectionTarget, ResolutionError, ResolvedCommand, ShellState, ShellStatus, ShellStatusKind,
    SimpleCommand, SourceSpan, StatefulBuiltin, parse,
};

/// Maximum stages accepted by one foreground human-shell pipeline.
pub const MAX_NATIVE_PIPELINE_STAGES: usize = 32;

const STDOUT_CAPTURE: ProcessCaptureId = ProcessCaptureId::new(1);
const STDERR_CAPTURE: ProcessCaptureId = ProcessCaptureId::new(2);

/// Stable category for a human-shell execution diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExecutionDiagnosticCode {
    Parse(DiagnosticCode),
    InvalidArguments,
    Resolution,
    Process,
    Filesystem,
    Redirection,
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
    stderr: Vec<u8>,
    diagnostics: Vec<ExecutionDiagnostic>,
    status: ShellStatus,
    exit_requested: Option<i64>,
}

impl ShellExecution {
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[ExecutionDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub const fn status(&self) -> &ShellStatus {
        &self.status
    }

    /// Returns the requested process status when a valid `exit` stopped this source.
    #[must_use]
    pub const fn exit_requested(&self) -> Option<i64> {
        self.exit_requested
    }

    #[must_use]
    pub fn rendered_stderr(&self) -> Vec<u8> {
        self.stderr.clone()
    }
}

#[derive(Default)]
struct ExecutionOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    diagnostics: Vec<ExecutionDiagnostic>,
}

/// Parses and executes the currently implemented foreground shell subset.
///
/// Commands and pipelines run sequentially against one mutable `ShellState`.
/// The current human-shell slice performs quote-aware native-string parameter expansion
/// before resolution, implements stateful environment updates and the portable
/// command set, provides bounded direct-argv native execution, and connects
/// two-to-32-stage native, portable, and stateful-builtin foreground pipelines
/// with direct operating-system pipes or explicitly retained asynchronous ends.
/// Stateful stages execute against isolated state clones. Pipeline status
/// defaults to the final stage and can select the rightmost failure through
/// persistent `pipefail`; final stdout and every stage's stderr share the
/// synchronous capture ceiling. Native and portable commands plus implemented
/// stateful builtins accept
/// source-ordered `<`, `>`, `>>`, `2>`, `2>>`, `2>&1`, and `1>&2`
/// redirections resolved against the persistent shell working directory;
/// child and parent-task files open in one global order, stateful simple-command
/// files open before parent mutation, and replacing internal pipeline endpoints
/// preserves EOF and broken-pipe behavior. Unresolved backends produce explicit
/// diagnostics without invoking a host shell.
#[must_use]
pub async fn execute_source(source: &str, state: &mut ShellState) -> ShellExecution {
    execute_source_with(source, state, &PathCommandLookup, HostPlatform::current()).await
}

/// Injectable form of [`execute_source`] for deterministic resolver tests.
#[must_use]
pub async fn execute_source_with<L>(
    source: &str,
    state: &mut ShellState,
    lookup: &L,
    host: HostPlatform,
) -> ShellExecution
where
    L: NativeCommandLookup + ?Sized,
{
    execute_source_with_runner(source, state, lookup, host, &DirectNativeCommandRunner).await
}

async fn execute_source_with_runner<L, R>(
    source: &str,
    state: &mut ShellState,
    lookup: &L,
    host: HostPlatform,
    runner: &R,
) -> ShellExecution
where
    L: NativeCommandLookup + ?Sized,
    R: NativeCommandRunner + ?Sized,
{
    let script = match parse(source) {
        Ok(script) => script,
        Err(error) => {
            let status = shell_status(2, ShellStatusKind::ParseError);
            state.set_last_status(status.clone());
            let diagnostic = ExecutionDiagnostic {
                code: ExecutionDiagnosticCode::Parse(error.code()),
                message: error.message().to_owned(),
                span: error.span(),
            };
            return ShellExecution {
                stdout: Vec::new(),
                stderr: diagnostic.render().into_bytes(),
                diagnostics: vec![diagnostic],
                status,
                exit_requested: None,
            };
        }
    };

    let mut output = ExecutionOutput::default();
    let mut final_status = state.last_status().clone();
    let mut exit_requested = None;
    for pipeline in script.pipelines() {
        let commands = &script.commands()[pipeline.command_range()];
        let diagnostic_start = output.diagnostics.len();
        final_status = if commands.len() == 1 {
            execute_simple_command(
                &commands[0],
                state,
                lookup,
                host,
                &mut output,
                &mut exit_requested,
                runner,
            )
            .await
        } else {
            execute_pipeline(
                commands,
                pipeline.span(),
                state,
                lookup,
                host,
                &mut output,
                runner,
            )
            .await
        };
        for diagnostic in &output.diagnostics[diagnostic_start..] {
            output
                .stderr
                .extend_from_slice(diagnostic.render().as_bytes());
        }
        state.set_last_status(final_status.clone());
        if exit_requested.is_some() {
            break;
        }
    }
    ShellExecution {
        stdout: output.stdout,
        stderr: output.stderr,
        diagnostics: output.diagnostics,
        status: final_status,
        exit_requested,
    }
}

async fn execute_simple_command<L, R>(
    command: &SimpleCommand,
    state: &mut ShellState,
    lookup: &L,
    host: HostPlatform,
    output: &mut ExecutionOutput,
    exit_requested: &mut Option<i64>,
    runner: &R,
) -> ShellStatus
where
    L: NativeCommandLookup + ?Sized,
    R: NativeCommandRunner + ?Sized,
{
    let expanded = expand_words(command.words(), state);
    let name_span = expanded.first().map(|word| word.span());
    let words: Vec<OsString> = expanded
        .into_iter()
        .map(crate::expand::ExpandedWord::into_value)
        .collect();
    if words.is_empty() {
        return ShellStatus::success();
    }
    let Some(name) = words[0].to_str() else {
        return invalid_command_name(
            name_span.expect("a non-empty expansion retains a source span"),
            &mut output.diagnostics,
        );
    };
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
    match resolved {
        Ok(resolved) => {
            execute_command(
                state,
                resolved,
                &words[1..],
                command,
                output,
                exit_requested,
                runner,
            )
            .await
        }
        Err(error) => resolution_failure(
            error,
            name_span.expect("a non-empty expansion retains a source span"),
            &mut output.diagnostics,
        ),
    }
}

async fn execute_command<R>(
    state: &mut ShellState,
    resolved: ResolvedCommand,
    arguments: &[OsString],
    command: &SimpleCommand,
    output: &mut ExecutionOutput,
    exit_requested: &mut Option<i64>,
    runner: &R,
) -> ShellStatus
where
    R: NativeCommandRunner + ?Sized,
{
    let span = command.span();
    let redirections = command.redirections();
    if !redirections.is_empty()
        && !matches!(
            &resolved,
            ResolvedCommand::Native { .. } | ResolvedCommand::Portable(_)
        )
        && !matches!(
            &resolved,
            ResolvedCommand::StatefulBuiltin(command)
                if stateful_builtin_is_implemented(*command)
        )
    {
        return unsupported(
            "redirections currently require a native, portable, or implemented stateful command"
                .to_owned(),
            span,
            &mut output.diagnostics,
            ShellStatusKind::Exited,
            2,
        );
    }
    match resolved {
        ResolvedCommand::StatefulBuiltin(command) if stateful_builtin_is_implemented(command) => {
            let (status, requested) = if redirections.is_empty() {
                execute_stateful_builtin(state, command, arguments, span, &mut output.diagnostics)
            } else {
                execute_stateful_redirected(
                    state,
                    command,
                    arguments,
                    redirections,
                    span,
                    &mut output.diagnostics,
                )
            };
            *exit_requested = requested;
            status
        }
        ResolvedCommand::Portable(command) if !redirections.is_empty() => {
            execute_portable_redirected(
                state,
                command,
                arguments,
                redirections,
                span,
                output,
                runner,
            )
            .await
        }
        ResolvedCommand::Portable(PortableCommand::Pwd) => execute_pwd(
            state,
            arguments,
            span,
            &mut output.stdout,
            &mut output.diagnostics,
        ),
        ResolvedCommand::Portable(PortableCommand::Echo) => {
            execute_echo(arguments, span, &mut output.stdout, &mut output.diagnostics)
        }
        ResolvedCommand::Portable(PortableCommand::List) => execute_ls(
            state,
            arguments,
            span,
            &mut output.stdout,
            &mut output.diagnostics,
        ),
        ResolvedCommand::Portable(PortableCommand::Cat) => execute_cat(
            state,
            arguments,
            span,
            &mut output.stdout,
            &mut output.diagnostics,
        ),
        ResolvedCommand::Portable(PortableCommand::Grep) => execute_grep(
            state,
            arguments,
            span,
            &mut output.stdout,
            &mut output.diagnostics,
        ),
        ResolvedCommand::StatefulBuiltin(command) => unsupported(
            format!("builtin `{}` is not implemented yet", command.name()),
            span,
            &mut output.diagnostics,
            ShellStatusKind::Exited,
            2,
        ),
        ResolvedCommand::Alias { name, .. } => unsupported(
            format!("alias execution for `{name}` is not implemented yet"),
            span,
            &mut output.diagnostics,
            ShellStatusKind::ResolutionError,
            126,
        ),
        ResolvedCommand::Function { name } => unsupported(
            format!("function execution for `{name}` is not implemented yet"),
            span,
            &mut output.diagnostics,
            ShellStatusKind::ResolutionError,
            126,
        ),
        ResolvedCommand::Native { executable, .. } => {
            execute_native(
                state,
                executable,
                arguments,
                redirections,
                span,
                output,
                runner,
            )
            .await
        }
        ResolvedCommand::Wsl { command, .. } => unsupported(
            format!("WSL execution for `{command}` is not implemented yet"),
            span,
            &mut output.diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeInvocation {
    executable: PathBuf,
    arguments: Vec<OsString>,
    cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
    capture_limit: u64,
    files: Vec<NativeProcessFile>,
    stdin: ProcessStdio,
    stdout: ProcessStdio,
    stderr: ProcessStdio,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeStdio {
    stdin: ProcessStdio,
    stdout: ProcessStdio,
    stderr: ProcessStdio,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PipelineStageInvocation {
    Native(NativeInvocation),
    Portable(PortablePipelineInvocation),
    Stateful(StatefulPipelineInvocation),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParentTaskStdio {
    Null,
    Pipe(ProcessPipeId),
    File(ParentProcessFileId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipelineCaptureStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParentPipelineCapture {
    stage_index: usize,
    pipe: ProcessPipeId,
    stream: PipelineCaptureStream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PortablePipelineInvocation {
    command: PortableCommand,
    arguments: Vec<OsString>,
    state: ShellState,
    span: SourceSpan,
    files: Vec<ParentProcessFile>,
    stdin: ParentTaskStdio,
    stdout: ParentTaskStdio,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StatefulPipelineInvocation {
    command: StatefulBuiltin,
    arguments: Vec<OsString>,
    state: ShellState,
    span: SourceSpan,
    files: Vec<ParentProcessFile>,
    stdout: ParentTaskStdio,
}

enum InProcessPipelineInvocation {
    Portable(PortablePipelineInvocation),
    Stateful(StatefulPipelineInvocation),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PipelineInvocation {
    stages: Vec<PipelineStageInvocation>,
    closed_pipe_ends: Vec<ClosedProcessPipeEnd>,
    parent_pipe_ends: Vec<ParentProcessPipeEnd>,
    parent_captures: Vec<ParentPipelineCapture>,
    capture_limit: u64,
}

#[derive(Debug, Eq, PartialEq)]
struct NativeCommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: ProcessExit,
}

#[derive(Debug, Eq, PartialEq)]
struct PipelineOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exits: Vec<ProcessExit>,
    diagnostics: Vec<ExecutionDiagnostic>,
}

#[derive(Debug)]
enum NativeCommandError {
    Platform(PlatformError),
    Redirection(PlatformError),
    Capture(io::Error),
    MissingStream(&'static str),
    CaptureLimit { max: u64 },
}

impl std::fmt::Display for NativeCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Platform(error) => error.fmt(formatter),
            Self::Redirection(error) => error.fmt(formatter),
            Self::Capture(error) => write!(formatter, "cannot capture process output: {error}"),
            Self::MissingStream(stream) => {
                write!(
                    formatter,
                    "the process did not expose its captured {stream}"
                )
            }
            Self::CaptureLimit { max } => {
                write!(
                    formatter,
                    "process output exceeds the {max}-byte capture allowance"
                )
            }
        }
    }
}

trait NativeCommandRunner {
    async fn run(
        &self,
        invocation: NativeInvocation,
    ) -> Result<NativeCommandOutput, NativeCommandError>;

    async fn run_pipeline(
        &self,
        invocation: PipelineInvocation,
    ) -> Result<PipelineOutput, NativeCommandError>;
}

struct DirectNativeCommandRunner;

impl NativeCommandRunner for DirectNativeCommandRunner {
    async fn run(
        &self,
        invocation: NativeInvocation,
    ) -> Result<NativeCommandOutput, NativeCommandError> {
        let capture_limit = invocation.capture_limit;
        let captures_stdout = invocation_uses_capture(&invocation, STDOUT_CAPTURE);
        let captures_stderr = invocation_uses_capture(&invocation, STDERR_CAPTURE);
        let mut process = spawn_native(&NativeProcessSpec {
            executable: invocation.executable.into_os_string(),
            argv: invocation.arguments,
            cwd: invocation.cwd,
            environment: invocation
                .environment
                .into_iter()
                .map(|(name, value)| EnvironmentChange::Set(name, value))
                .collect(),
            clear_environment: true,
            files: invocation.files,
            stdin: invocation.stdin,
            stdout: invocation.stdout,
            stderr: invocation.stderr,
        })
        .map_err(classify_native_spawn_error)?;
        let stdout = process.take_capture(STDOUT_CAPTURE);
        if captures_stdout && stdout.is_none() {
            let _ = process.terminate().await;
            let _ = process.wait().await;
            return Err(NativeCommandError::MissingStream("stdout"));
        }
        let stderr = process.take_capture(STDERR_CAPTURE);
        if captures_stderr && stderr.is_none() {
            let _ = process.terminate().await;
            let _ = process.wait().await;
            return Err(NativeCommandError::MissingStream("stderr"));
        }
        let captured = Arc::new(AtomicU64::new(0));
        let result = tokio::try_join!(
            capture_optional_process_stream(stdout, Arc::clone(&captured), capture_limit),
            capture_optional_process_stream(stderr, captured, capture_limit),
            async { process.wait().await.map_err(NativeCommandError::Platform) },
        );
        if result.is_err() {
            let _ = process.terminate().await;
            let _ = process.wait().await;
        }
        let (stdout, stderr, exit) = result?;
        Ok(NativeCommandOutput {
            stdout,
            stderr,
            exit,
        })
    }

    async fn run_pipeline(
        &self,
        invocation: PipelineInvocation,
    ) -> Result<PipelineOutput, NativeCommandError> {
        run_pipeline(invocation).await
    }
}

async fn run_pipeline(
    invocation: PipelineInvocation,
) -> Result<PipelineOutput, NativeCommandError> {
    let PipelineInvocation {
        stages,
        closed_pipe_ends,
        parent_pipe_ends,
        parent_captures,
        capture_limit,
    } = invocation;
    let stage_count = stages.len();
    let mut specs = Vec::with_capacity(stage_count);
    let mut native_stage_indices = Vec::with_capacity(stage_count);
    let mut parent_files = Vec::new();
    let mut file_order = Vec::new();
    for (stage_index, stage) in stages.iter().enumerate() {
        match stage {
            PipelineStageInvocation::Native(stage) => {
                let process_index = specs.len();
                native_stage_indices.push(stage_index);
                file_order.extend(
                    stage
                        .files
                        .iter()
                        .map(|endpoint| ProcessGraphFile::Process {
                            process_index,
                            file: endpoint.id,
                        }),
                );
                specs.push(NativeProcessSpec {
                    executable: stage.executable.clone().into_os_string(),
                    argv: stage.arguments.clone(),
                    cwd: stage.cwd.clone(),
                    environment: stage
                        .environment
                        .iter()
                        .map(|(name, value)| EnvironmentChange::Set(name.clone(), value.clone()))
                        .collect(),
                    clear_environment: true,
                    files: stage.files.clone(),
                    stdin: stage.stdin,
                    stdout: stage.stdout,
                    stderr: stage.stderr,
                });
            }
            PipelineStageInvocation::Portable(stage) => {
                file_order.extend(
                    stage
                        .files
                        .iter()
                        .map(|endpoint| ProcessGraphFile::Parent(endpoint.id)),
                );
                parent_files.extend(stage.files.iter().cloned());
            }
            PipelineStageInvocation::Stateful(stage) => {
                file_order.extend(
                    stage
                        .files
                        .iter()
                        .map(|endpoint| ProcessGraphFile::Parent(endpoint.id)),
                );
                parent_files.extend(stage.files.iter().cloned());
            }
        }
    }

    let mut graph = spawn_native_graph_with_parent_io(
        &specs,
        &closed_pipe_ends,
        &parent_pipe_ends,
        &parent_files,
        &file_order,
    )
    .map_err(classify_native_spawn_error)?;
    let mut stdout_streams: Vec<Option<tokio::fs::File>> =
        std::iter::repeat_with(|| None).take(stage_count).collect();
    let mut stderr_streams: Vec<Option<tokio::fs::File>> =
        std::iter::repeat_with(|| None).take(stage_count).collect();
    for capture in parent_captures {
        let reader = match graph.take_pipe_reader(capture.pipe) {
            Some(reader) => reader,
            None => {
                return Err(cleanup_graph_setup_error(
                    &mut graph,
                    NativeCommandError::MissingStream("in-process stage capture"),
                )
                .await);
            }
        };
        match capture.stream {
            PipelineCaptureStream::Stdout => stdout_streams[capture.stage_index] = Some(reader),
            PipelineCaptureStream::Stderr => stderr_streams[capture.stage_index] = Some(reader),
        }
    }

    let mut in_process_tasks = Vec::new();
    for (stage_index, stage) in stages.into_iter().enumerate() {
        let (invocation, input, output) = match stage {
            PipelineStageInvocation::Native(_) => continue,
            PipelineStageInvocation::Portable(invocation) => {
                let input = match take_parent_task_reader(&mut graph, invocation.stdin) {
                    Ok(input) => input,
                    Err(error) => {
                        return Err(cleanup_graph_setup_error(&mut graph, error).await);
                    }
                };
                let output = match take_parent_task_writer(&mut graph, invocation.stdout) {
                    Ok(output) => output,
                    Err(error) => {
                        return Err(cleanup_graph_setup_error(&mut graph, error).await);
                    }
                };
                (
                    InProcessPipelineInvocation::Portable(invocation),
                    Some(input),
                    output,
                )
            }
            PipelineStageInvocation::Stateful(invocation) => {
                let output = match take_parent_task_writer(&mut graph, invocation.stdout) {
                    Ok(output) => output,
                    Err(error) => {
                        return Err(cleanup_graph_setup_error(&mut graph, error).await);
                    }
                };
                (
                    InProcessPipelineInvocation::Stateful(invocation),
                    None,
                    output,
                )
            }
        };
        in_process_tasks.push(execute_in_process_pipeline_stage(
            stage_index,
            invocation,
            input,
            output,
        ));
    }

    let mut job = graph.into_job();
    for ((process_index, spec), &stage_index) in specs.iter().enumerate().zip(&native_stage_indices)
    {
        let captures_stdout = spec_uses_capture(spec, STDOUT_CAPTURE);
        let stdout = job.take_capture(process_index, STDOUT_CAPTURE);
        if captures_stdout && stdout.is_none() {
            let _ = job.terminate_and_reap().await;
            return Err(NativeCommandError::MissingStream("stage stdout"));
        }
        stdout_streams[stage_index] = stdout;
        let captures_stderr = spec_uses_capture(spec, STDERR_CAPTURE);
        let stderr = job.take_capture(process_index, STDERR_CAPTURE);
        if captures_stderr && stderr.is_none() {
            let _ = job.terminate_and_reap().await;
            return Err(NativeCommandError::MissingStream("stage stderr"));
        }
        stderr_streams[stage_index] = stderr;
    }

    let captured = Arc::new(AtomicU64::new(0));
    let stdout_capture = try_join_all(stdout_streams.into_iter().map(|stdout| {
        capture_optional_process_stream(stdout, Arc::clone(&captured), capture_limit)
    }));
    let stderr_capture = try_join_all(stderr_streams.into_iter().map(|stderr| {
        capture_optional_process_stream(stderr, Arc::clone(&captured), capture_limit)
    }));
    let in_process_all = async { Ok::<_, NativeCommandError>(join_all(in_process_tasks).await) };
    let (stdout, stderr, in_process) =
        match tokio::try_join!(stdout_capture, stderr_capture, in_process_all) {
            Ok(output) => output,
            Err(error) => {
                let _ = job.terminate_and_reap().await;
                return Err(error);
            }
        };
    let native_exits = job.wait().await.map_err(NativeCommandError::Platform)?;
    let mut exits = std::iter::repeat_with(|| None)
        .take(stage_count)
        .collect::<Vec<_>>();
    for (&stage_index, exit) in native_stage_indices.iter().zip(native_exits) {
        exits[stage_index] = Some(exit);
    }
    let mut diagnostics = Vec::new();
    for output in in_process {
        exits[output.stage_index] = Some(output.exit);
        diagnostics.extend(output.diagnostics);
    }
    let exits = exits
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(NativeCommandError::MissingStream("pipeline stage status"))?;
    Ok(PipelineOutput {
        stdout: stdout.into_iter().flatten().collect(),
        stderr: stderr.into_iter().flatten().collect(),
        exits,
        diagnostics,
    })
}

type PortablePipelineReader = Box<dyn AsyncRead + Send + Unpin>;
type PortablePipelineWriter = Box<dyn AsyncWrite + Send + Unpin>;

fn take_parent_task_reader(
    graph: &mut ash_platform::NativeProcessGraph,
    endpoint: ParentTaskStdio,
) -> Result<PortablePipelineReader, NativeCommandError> {
    match endpoint {
        ParentTaskStdio::Null => Ok(Box::new(tokio::io::empty())),
        ParentTaskStdio::Pipe(id) => graph
            .take_pipe_reader(id)
            .map(|reader| Box::new(reader) as PortablePipelineReader)
            .ok_or(NativeCommandError::MissingStream("portable stage stdin")),
        ParentTaskStdio::File(id) => graph
            .take_parent_file(id)
            .map(|reader| Box::new(reader) as PortablePipelineReader)
            .ok_or(NativeCommandError::MissingStream(
                "portable stage input file",
            )),
    }
}

fn take_parent_task_writer(
    graph: &mut ash_platform::NativeProcessGraph,
    endpoint: ParentTaskStdio,
) -> Result<PortablePipelineWriter, NativeCommandError> {
    match endpoint {
        ParentTaskStdio::Null => Ok(Box::new(tokio::io::sink())),
        ParentTaskStdio::Pipe(id) => graph
            .take_pipe_writer(id)
            .map(|writer| Box::new(writer) as PortablePipelineWriter)
            .ok_or(NativeCommandError::MissingStream("portable stage stdout")),
        ParentTaskStdio::File(id) => graph
            .take_parent_file(id)
            .map(|writer| Box::new(writer) as PortablePipelineWriter)
            .ok_or(NativeCommandError::MissingStream(
                "portable stage output file",
            )),
    }
}

struct PipelineStageOutput {
    stage_index: usize,
    exit: ProcessExit,
    diagnostics: Vec<ExecutionDiagnostic>,
}

async fn execute_in_process_pipeline_stage(
    stage_index: usize,
    invocation: InProcessPipelineInvocation,
    input: Option<PortablePipelineReader>,
    output: PortablePipelineWriter,
) -> PipelineStageOutput {
    match invocation {
        InProcessPipelineInvocation::Portable(invocation) => {
            execute_portable_pipeline_stage(
                stage_index,
                invocation,
                input.expect("portable task input is prepared"),
                output,
            )
            .await
        }
        InProcessPipelineInvocation::Stateful(invocation) => {
            debug_assert!(input.is_none());
            execute_stateful_pipeline_stage(stage_index, invocation, output)
        }
    }
}

fn execute_stateful_pipeline_stage(
    stage_index: usize,
    invocation: StatefulPipelineInvocation,
    output: PortablePipelineWriter,
) -> PipelineStageOutput {
    let mut state = invocation.state;
    let mut diagnostics = Vec::new();
    let (status, _) = execute_stateful_builtin(
        &mut state,
        invocation.command,
        &invocation.arguments,
        invocation.span,
        &mut diagnostics,
    );
    drop(output);
    pipeline_stage_output(stage_index, status, diagnostics)
}

async fn execute_portable_pipeline_stage(
    stage_index: usize,
    invocation: PortablePipelineInvocation,
    input: PortablePipelineReader,
    output: PortablePipelineWriter,
) -> PipelineStageOutput {
    match invocation.command {
        PortableCommand::Cat => {
            execute_portable_pipeline_cat(stage_index, invocation, input, output).await
        }
        PortableCommand::Grep => {
            execute_portable_pipeline_grep(stage_index, invocation, input, output).await
        }
        command => {
            drop(input);
            let mut stdout = Vec::new();
            let mut diagnostics = Vec::new();
            let mut status = match command {
                PortableCommand::Pwd => execute_pwd(
                    &invocation.state,
                    &invocation.arguments,
                    invocation.span,
                    &mut stdout,
                    &mut diagnostics,
                ),
                PortableCommand::Echo => execute_echo(
                    &invocation.arguments,
                    invocation.span,
                    &mut stdout,
                    &mut diagnostics,
                ),
                PortableCommand::List => execute_ls(
                    &invocation.state,
                    &invocation.arguments,
                    invocation.span,
                    &mut stdout,
                    &mut diagnostics,
                ),
                PortableCommand::Cat | PortableCommand::Grep => unreachable!(),
            };
            if let Err(error) = write_portable_pipeline_output(output, &stdout).await
                && status.code() == 0
            {
                status = portable_pipeline_write_failure(error, invocation.span, &mut diagnostics);
            }
            pipeline_stage_output(stage_index, status, diagnostics)
        }
    }
}

async fn execute_portable_pipeline_cat(
    stage_index: usize,
    invocation: PortablePipelineInvocation,
    input: PortablePipelineReader,
    mut output: PortablePipelineWriter,
) -> PipelineStageOutput {
    let target = parse_cat_path_with_stdin(&invocation.arguments, true)
        .expect("portable pipeline cat arguments were preflighted");
    let mut diagnostics = Vec::new();
    let mut reader: PortablePipelineReader = if target == "-" {
        input
    } else {
        drop(input);
        let filesystem = match NativeFileSystem::new(invocation.state.cwd()) {
            Ok(filesystem) => filesystem,
            Err(error) => {
                let status = filesystem_failure(
                    format!("cannot read from the current directory: {error}"),
                    invocation.span,
                    &mut diagnostics,
                );
                drop(output);
                return pipeline_stage_output(stage_index, status, diagnostics);
            }
        };
        let path = PathBuf::from(&target);
        let resolved = match filesystem.resolve_existing(&path) {
            Ok(resolved) => resolved,
            Err(error) => {
                let status =
                    read_filesystem_failure(&target, error, invocation.span, &mut diagnostics);
                drop(output);
                return pipeline_stage_output(stage_index, status, diagnostics);
            }
        };
        match tokio::fs::File::open(&resolved).await {
            Ok(file) => Box::new(file),
            Err(error) => {
                let status =
                    read_filesystem_failure(&target, error, invocation.span, &mut diagnostics);
                drop(output);
                return pipeline_stage_output(stage_index, status, diagnostics);
            }
        }
    };

    let status = match copy_portable_pipeline_stream(&mut *reader, &mut output).await {
        Ok(()) => ShellStatus::success(),
        Err(PortableStreamError::Limit) => read_filesystem_failure(
            &target,
            format!("input exceeds the {MAX_READ_FILE_BYTES}-byte portable pipeline ceiling"),
            invocation.span,
            &mut diagnostics,
        ),
        Err(PortableStreamError::Read(error)) => {
            read_filesystem_failure(&target, error, invocation.span, &mut diagnostics)
        }
        Err(PortableStreamError::Write(error)) => {
            portable_pipeline_write_failure(error, invocation.span, &mut diagnostics)
        }
    };
    pipeline_stage_output(stage_index, status, diagnostics)
}

async fn execute_portable_pipeline_grep(
    stage_index: usize,
    invocation: PortablePipelineInvocation,
    input: PortablePipelineReader,
    mut output: PortablePipelineWriter,
) -> PipelineStageOutput {
    let options = parse_grep_options_with_stdin(&invocation.arguments, true)
        .expect("portable pipeline grep arguments were preflighted");
    let matcher =
        PipelineGrepMatcher::new(&options).expect("portable pipeline grep pattern was preflighted");
    let mut diagnostics = Vec::new();
    let mut reader: PortablePipelineReader = if options.path == "-" {
        input
    } else {
        drop(input);
        let filesystem = match NativeFileSystem::new(invocation.state.cwd()) {
            Ok(filesystem) => filesystem,
            Err(error) => {
                let status = filesystem_failure(
                    format!("cannot search from the current directory: {error}"),
                    invocation.span,
                    &mut diagnostics,
                );
                drop(output);
                return pipeline_stage_output(stage_index, status, diagnostics);
            }
        };
        let path = PathBuf::from(&options.path);
        let resolved = match filesystem.resolve_existing(&path) {
            Ok(resolved) => resolved,
            Err(error) => {
                let status = search_filesystem_failure(
                    &options.path,
                    error,
                    invocation.span,
                    &mut diagnostics,
                );
                drop(output);
                return pipeline_stage_output(stage_index, status, diagnostics);
            }
        };
        match tokio::fs::File::open(&resolved).await {
            Ok(file) => Box::new(file),
            Err(error) => {
                let status = search_filesystem_failure(
                    &options.path,
                    error,
                    invocation.span,
                    &mut diagnostics,
                );
                drop(output);
                return pipeline_stage_output(stage_index, status, diagnostics);
            }
        }
    };

    let status = match grep_portable_pipeline_stream(&mut *reader, &mut output, &options, &matcher)
        .await
    {
        Ok(true) => ShellStatus::success(),
        Ok(false) => shell_status(1, ShellStatusKind::Exited),
        Err(PortableGrepStreamError::InputLimit) => search_filesystem_failure(
            &options.path,
            format!("input exceeds the {MAX_SEARCH_FILE_BYTES}-byte portable grep ceiling"),
            invocation.span,
            &mut diagnostics,
        ),
        Err(PortableGrepStreamError::OutputLimit) => search_filesystem_failure(
            &options.path,
            format!(
                "output exceeds the {MAX_READ_FILE_BYTES}-byte synchronous shell capture ceiling"
            ),
            invocation.span,
            &mut diagnostics,
        ),
        Err(PortableGrepStreamError::InvalidUtf8) => search_filesystem_failure(
            &options.path,
            "input is not valid UTF-8",
            invocation.span,
            &mut diagnostics,
        ),
        Err(PortableGrepStreamError::Read(error)) => {
            search_filesystem_failure(&options.path, error, invocation.span, &mut diagnostics)
        }
        Err(PortableGrepStreamError::Write(error)) => {
            portable_pipeline_write_failure(error, invocation.span, &mut diagnostics)
        }
    };
    pipeline_stage_output(stage_index, status, diagnostics)
}

enum PortableStreamError {
    Read(io::Error),
    Write(io::Error),
    Limit,
}

async fn copy_portable_pipeline_stream(
    reader: &mut (dyn AsyncRead + Send + Unpin),
    writer: &mut (dyn AsyncWrite + Send + Unpin),
) -> Result<(), PortableStreamError> {
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let remaining = MAX_READ_FILE_BYTES.saturating_sub(copied);
        let requested = if remaining == 0 {
            1
        } else {
            usize::try_from(remaining)
                .unwrap_or(buffer.len())
                .min(buffer.len())
        };
        let read = reader
            .read(&mut buffer[..requested])
            .await
            .map_err(PortableStreamError::Read)?;
        if read == 0 {
            writer
                .shutdown()
                .await
                .map_err(PortableStreamError::Write)?;
            return Ok(());
        }
        if remaining == 0 {
            return Err(PortableStreamError::Limit);
        }
        writer
            .write_all(&buffer[..read])
            .await
            .map_err(PortableStreamError::Write)?;
        copied = copied.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
}

enum PipelineGrepMatcher {
    Literal(String),
    Regex(Regex),
}

impl PipelineGrepMatcher {
    fn new(options: &GrepCommandOptions) -> Result<Self, regex::Error> {
        if options.pattern == SemanticSearchPattern::Regex || options.case_insensitive {
            let pattern = if options.pattern == SemanticSearchPattern::Regex {
                options.query.clone()
            } else {
                regex::escape(&options.query)
            };
            RegexBuilder::new(&pattern)
                .case_insensitive(options.case_insensitive)
                .size_limit(16 * 1024 * 1024)
                .build()
                .map(Self::Regex)
        } else {
            Ok(Self::Literal(options.query.clone()))
        }
    }

    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal(query) => line.contains(query),
            Self::Regex(regex) => regex.is_match(line),
        }
    }
}

enum PortableGrepStreamError {
    Read(io::Error),
    Write(io::Error),
    InputLimit,
    OutputLimit,
    InvalidUtf8,
}

async fn grep_portable_pipeline_stream(
    reader: &mut (dyn AsyncRead + Send + Unpin),
    writer: &mut (dyn AsyncWrite + Send + Unpin),
    options: &GrepCommandOptions,
    matcher: &PipelineGrepMatcher,
) -> Result<bool, PortableGrepStreamError> {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut line_number = 0_u64;
    let mut input_bytes = 0_u64;
    let mut output_bytes = 0_u64;
    let mut matched_any = false;
    loop {
        line.clear();
        let mut reached_eof = false;
        loop {
            let available = reader
                .fill_buf()
                .await
                .map_err(PortableGrepStreamError::Read)?;
            if available.is_empty() {
                reached_eof = true;
                break;
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            let updated = input_bytes
                .checked_add(u64::try_from(take).unwrap_or(u64::MAX))
                .ok_or(PortableGrepStreamError::InputLimit)?;
            if updated > MAX_SEARCH_FILE_BYTES {
                return Err(PortableGrepStreamError::InputLimit);
            }
            let ends_line = available[take - 1] == b'\n';
            line.extend_from_slice(&available[..take]);
            input_bytes = updated;
            reader.consume(take);
            if ends_line {
                break;
            }
        }
        if line.is_empty() && reached_eof {
            writer
                .shutdown()
                .await
                .map_err(PortableGrepStreamError::Write)?;
            return Ok(matched_any);
        }
        line_number = line_number.saturating_add(1);
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let text = std::str::from_utf8(&line).map_err(|_| PortableGrepStreamError::InvalidUtf8)?;
        if !matcher.is_match(text) {
            continue;
        }
        matched_any = true;
        let prefix = options.line_number.then(|| format!("{line_number}:"));
        let rendered_len = prefix
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(line.len())
            .saturating_add(1);
        output_bytes = output_bytes
            .checked_add(u64::try_from(rendered_len).unwrap_or(u64::MAX))
            .ok_or(PortableGrepStreamError::OutputLimit)?;
        if output_bytes > MAX_READ_FILE_BYTES {
            return Err(PortableGrepStreamError::OutputLimit);
        }
        if let Some(prefix) = prefix {
            writer
                .write_all(prefix.as_bytes())
                .await
                .map_err(PortableGrepStreamError::Write)?;
        }
        writer
            .write_all(&line)
            .await
            .map_err(PortableGrepStreamError::Write)?;
        writer
            .write_all(b"\n")
            .await
            .map_err(PortableGrepStreamError::Write)?;
    }
}

async fn write_portable_pipeline_output(
    mut output: PortablePipelineWriter,
    bytes: &[u8],
) -> io::Result<()> {
    output.write_all(bytes).await?;
    output.shutdown().await
}

fn portable_pipeline_write_failure(
    error: io::Error,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    if error.kind() == io::ErrorKind::BrokenPipe {
        shell_status(1, ShellStatusKind::Exited)
    } else {
        process_failure(
            format!("cannot write portable pipeline output: {error}"),
            span,
            diagnostics,
            ShellStatusKind::Exited,
            1,
        )
    }
}

fn pipeline_stage_output(
    stage_index: usize,
    status: ShellStatus,
    diagnostics: Vec<ExecutionDiagnostic>,
) -> PipelineStageOutput {
    PipelineStageOutput {
        stage_index,
        exit: ProcessExit {
            success: status.code() == 0,
            code: Some(status.code()),
            signal: status.signal(),
        },
        diagnostics,
    }
}

async fn cleanup_graph_setup_error(
    graph: &mut NativeProcessGraph,
    error: NativeCommandError,
) -> NativeCommandError {
    let _ = graph.terminate_and_reap().await;
    error
}

fn classify_native_spawn_error(error: PlatformError) -> NativeCommandError {
    if matches!(
        &error,
        PlatformError::InvalidProcessRedirection | PlatformError::ProcessRedirection { .. }
    ) {
        NativeCommandError::Redirection(error)
    } else {
        NativeCommandError::Platform(error)
    }
}

fn invocation_uses_capture(invocation: &NativeInvocation, id: ProcessCaptureId) -> bool {
    [invocation.stdout, invocation.stderr].contains(&ProcessStdio::Capture(id))
}

fn spec_uses_capture(spec: &NativeProcessSpec, id: ProcessCaptureId) -> bool {
    [spec.stdout, spec.stderr].contains(&ProcessStdio::Capture(id))
}

async fn capture_optional_process_stream<R>(
    reader: Option<R>,
    captured: Arc<AtomicU64>,
    max: u64,
) -> Result<Vec<u8>, NativeCommandError>
where
    R: AsyncRead + Unpin,
{
    match reader {
        Some(reader) => capture_process_stream(reader, captured, max).await,
        None => Ok(Vec::new()),
    }
}

async fn capture_process_stream<R>(
    mut reader: R,
    captured: Arc<AtomicU64>,
    max: u64,
) -> Result<Vec<u8>, NativeCommandError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(NativeCommandError::Capture)?;
        if read == 0 {
            return Ok(output);
        }
        let read = u64::try_from(read).unwrap_or(u64::MAX);
        captured
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(read).filter(|updated| *updated <= max)
            })
            .map_err(|_| NativeCommandError::CaptureLimit { max })?;
        let read = usize::try_from(read).unwrap_or(buffer.len());
        output.extend_from_slice(&buffer[..read]);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParentTaskRedirectionEndpoint {
    Null,
    Pipe(ProcessPipeId),
    File(ParentProcessFileId),
    Capture(PipelineCaptureStream),
}

struct ParentTaskRedirectionPlan {
    files: Vec<ParentProcessFile>,
    stdin: ParentTaskRedirectionEndpoint,
    stdout: ParentTaskRedirectionEndpoint,
}

fn allocate_parent_capture(
    stage_index: usize,
    stream: PipelineCaptureStream,
    next_pipe_id: &mut u32,
    parent_pipe_ends: &mut Vec<ParentProcessPipeEnd>,
    parent_captures: &mut Vec<ParentPipelineCapture>,
) -> ProcessPipeId {
    let pipe = ProcessPipeId::new(*next_pipe_id);
    *next_pipe_id = next_pipe_id
        .checked_add(1)
        .expect("bounded pipeline capture identifiers fit u32");
    parent_pipe_ends.push(ParentProcessPipeEnd::Reader(pipe));
    parent_pipe_ends.push(ParentProcessPipeEnd::Writer(pipe));
    parent_captures.push(ParentPipelineCapture {
        stage_index,
        pipe,
        stream,
    });
    pipe
}

fn materialize_parent_task_endpoint(
    endpoint: ParentTaskRedirectionEndpoint,
    stage_index: usize,
    next_pipe_id: &mut u32,
    parent_pipe_ends: &mut Vec<ParentProcessPipeEnd>,
    parent_captures: &mut Vec<ParentPipelineCapture>,
) -> ParentTaskStdio {
    match endpoint {
        ParentTaskRedirectionEndpoint::Null => ParentTaskStdio::Null,
        ParentTaskRedirectionEndpoint::Pipe(id) => ParentTaskStdio::Pipe(id),
        ParentTaskRedirectionEndpoint::File(id) => ParentTaskStdio::File(id),
        ParentTaskRedirectionEndpoint::Capture(stream) => {
            ParentTaskStdio::Pipe(allocate_parent_capture(
                stage_index,
                stream,
                next_pipe_id,
                parent_pipe_ends,
                parent_captures,
            ))
        }
    }
}

fn plan_parent_task_redirections(
    state: &ShellState,
    redirections: &[Redirection],
    mut stdin: ParentTaskRedirectionEndpoint,
    mut stdout: ParentTaskRedirectionEndpoint,
    mut stderr: ParentTaskRedirectionEndpoint,
    next_parent_file_id: &mut u32,
) -> Result<ParentTaskRedirectionPlan, RedirectionPlanError> {
    let mut files = Vec::with_capacity(redirections.len());
    for redirection in redirections {
        let endpoint = match redirection.target() {
            RedirectionTarget::File { path, mode } => {
                if !matches!(
                    (redirection.descriptor(), mode),
                    (RedirectionDescriptor::Stdin, RedirectionFileMode::Read)
                        | (
                            RedirectionDescriptor::Stdout | RedirectionDescriptor::Stderr,
                            RedirectionFileMode::Write | RedirectionFileMode::Append
                        )
                ) {
                    return Err(RedirectionPlanError {
                        message: "redirection file mode does not match its descriptor".to_owned(),
                        span: redirection.operator_span(),
                    });
                }
                let mut expanded = expand_words(std::slice::from_ref(path), state).into_iter();
                let Some(target) = expanded.next() else {
                    return Err(RedirectionPlanError {
                        message: "redirection target expands to no path".to_owned(),
                        span: path.span(),
                    });
                };
                if expanded.next().is_some() {
                    return Err(RedirectionPlanError {
                        message: "redirection target expands to multiple paths".to_owned(),
                        span: path.span(),
                    });
                }
                let id = ParentProcessFileId::new(*next_parent_file_id);
                *next_parent_file_id =
                    next_parent_file_id
                        .checked_add(1)
                        .ok_or_else(|| RedirectionPlanError {
                            message: "redirection plan contains too many parent file targets"
                                .to_owned(),
                            span: redirection.span(),
                        })?;
                files.push(ParentProcessFile {
                    id,
                    path: state.cwd().join(PathBuf::from(target.into_value())),
                    mode: match mode {
                        RedirectionFileMode::Read => NativeProcessFileMode::Read,
                        RedirectionFileMode::Write => NativeProcessFileMode::Write,
                        RedirectionFileMode::Append => NativeProcessFileMode::Append,
                    },
                });
                ParentTaskRedirectionEndpoint::File(id)
            }
            RedirectionTarget::Descriptor(source) => match source {
                RedirectionDescriptor::Stdin => stdin,
                RedirectionDescriptor::Stdout => stdout,
                RedirectionDescriptor::Stderr => stderr,
            },
        };
        match redirection.descriptor() {
            RedirectionDescriptor::Stdin => stdin = endpoint,
            RedirectionDescriptor::Stdout => stdout = endpoint,
            RedirectionDescriptor::Stderr => stderr = endpoint,
        }
    }

    Ok(ParentTaskRedirectionPlan {
        files,
        stdin,
        stdout,
    })
}

#[allow(clippy::too_many_arguments)]
fn portable_pipeline_invocation(
    state: &ShellState,
    command: PortableCommand,
    arguments: Vec<OsString>,
    redirections: &[Redirection],
    span: SourceSpan,
    stage_index: usize,
    reads_stdin: bool,
    stdin: ParentTaskRedirectionEndpoint,
    stdout: ParentTaskRedirectionEndpoint,
    next_pipe_id: &mut u32,
    next_parent_file_id: &mut u32,
    parent_pipe_ends: &mut Vec<ParentProcessPipeEnd>,
    parent_captures: &mut Vec<ParentPipelineCapture>,
) -> Result<PortablePipelineInvocation, RedirectionPlanError> {
    let plan = plan_parent_task_redirections(
        state,
        redirections,
        stdin,
        stdout,
        ParentTaskRedirectionEndpoint::Capture(PipelineCaptureStream::Stderr),
        next_parent_file_id,
    )?;

    let stdin = if reads_stdin {
        match plan.stdin {
            ParentTaskRedirectionEndpoint::Null => ParentTaskStdio::Null,
            ParentTaskRedirectionEndpoint::Pipe(id) => ParentTaskStdio::Pipe(id),
            ParentTaskRedirectionEndpoint::File(id) => ParentTaskStdio::File(id),
            ParentTaskRedirectionEndpoint::Capture(_) => {
                return Err(RedirectionPlanError {
                    message: "portable stdin cannot use an output capture".to_owned(),
                    span,
                });
            }
        }
    } else {
        ParentTaskStdio::Null
    };
    let stdout = materialize_parent_task_endpoint(
        plan.stdout,
        stage_index,
        next_pipe_id,
        parent_pipe_ends,
        parent_captures,
    );
    Ok(PortablePipelineInvocation {
        command,
        arguments,
        state: state.clone(),
        span,
        files: plan.files,
        stdin,
        stdout,
    })
}

#[allow(clippy::too_many_arguments)]
fn stateful_pipeline_invocation(
    state: &ShellState,
    command: StatefulBuiltin,
    arguments: Vec<OsString>,
    redirections: &[Redirection],
    span: SourceSpan,
    stage_index: usize,
    stdout: ParentTaskRedirectionEndpoint,
    next_pipe_id: &mut u32,
    next_parent_file_id: &mut u32,
    parent_pipe_ends: &mut Vec<ParentProcessPipeEnd>,
    parent_captures: &mut Vec<ParentPipelineCapture>,
) -> Result<StatefulPipelineInvocation, RedirectionPlanError> {
    let plan = plan_parent_task_redirections(
        state,
        redirections,
        ParentTaskRedirectionEndpoint::Null,
        stdout,
        ParentTaskRedirectionEndpoint::Capture(PipelineCaptureStream::Stderr),
        next_parent_file_id,
    )?;
    Ok(StatefulPipelineInvocation {
        command,
        arguments,
        state: state.clone(),
        span,
        files: plan.files,
        stdout: materialize_parent_task_endpoint(
            plan.stdout,
            stage_index,
            next_pipe_id,
            parent_pipe_ends,
            parent_captures,
        ),
    })
}

async fn execute_pipeline<L, R>(
    commands: &[SimpleCommand],
    span: SourceSpan,
    state: &ShellState,
    lookup: &L,
    host: HostPlatform,
    output: &mut ExecutionOutput,
    runner: &R,
) -> ShellStatus
where
    L: NativeCommandLookup + ?Sized,
    R: NativeCommandRunner + ?Sized,
{
    if commands.len() > MAX_NATIVE_PIPELINE_STAGES {
        return invalid_arguments(
            &format!("pipelines support at most {MAX_NATIVE_PIPELINE_STAGES} stages"),
            span,
            &mut output.diagnostics,
        );
    }

    let capture_limit = remaining_capture(output);
    let mut stages = Vec::with_capacity(commands.len());
    let mut parent_pipe_ends = Vec::with_capacity(commands.len().saturating_mul(2));
    let mut parent_captures = Vec::new();
    let mut next_pipe_id =
        u32::try_from(commands.len() - 1).expect("pipeline stage ceiling fits u32");
    let mut next_parent_file_id = 0_u32;
    for (index, command) in commands.iter().enumerate() {
        let expanded = expand_words(command.words(), state);
        let name_span = expanded.first().map(|word| word.span());
        let words: Vec<OsString> = expanded
            .into_iter()
            .map(crate::expand::ExpandedWord::into_value)
            .collect();
        if words.is_empty() {
            return invalid_arguments(
                "a pipeline stage expands to no command",
                command.span(),
                &mut output.diagnostics,
            );
        }
        let Some(name) = words[0].to_str() else {
            return invalid_command_name(
                name_span.expect("a non-empty expansion retains a source span"),
                &mut output.diagnostics,
            );
        };
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
        match resolved {
            Ok(ResolvedCommand::Native { executable, .. }) => {
                let stdin = if index == 0 {
                    ProcessStdio::Null
                } else {
                    ProcessStdio::Pipe(ProcessPipeId::new(
                        u32::try_from(index - 1).expect("pipeline stage ceiling fits u32"),
                    ))
                };
                let stdout = if index + 1 == commands.len() {
                    ProcessStdio::Capture(STDOUT_CAPTURE)
                } else {
                    ProcessStdio::Pipe(ProcessPipeId::new(
                        u32::try_from(index).expect("pipeline stage ceiling fits u32"),
                    ))
                };
                match native_invocation(
                    state,
                    executable,
                    &words[1..],
                    command.redirections(),
                    capture_limit,
                    NativeStdio {
                        stdin,
                        stdout,
                        stderr: ProcessStdio::Capture(STDERR_CAPTURE),
                    },
                ) {
                    Ok(invocation) => stages.push(PipelineStageInvocation::Native(invocation)),
                    Err(error) => {
                        return redirection_failure(
                            error.message,
                            error.span,
                            &mut output.diagnostics,
                        );
                    }
                }
            }
            Ok(ResolvedCommand::StatefulBuiltin(builtin))
                if stateful_builtin_is_implemented(builtin) =>
            {
                let arguments = words[1..].to_vec();
                if let Err(error) = validate_stateful_pipeline_arguments(builtin, &arguments, state)
                {
                    return invalid_arguments(&error, command.span(), &mut output.diagnostics);
                }
                let stdout = if index + 1 == commands.len() {
                    ParentTaskRedirectionEndpoint::Capture(PipelineCaptureStream::Stdout)
                } else {
                    ParentTaskRedirectionEndpoint::Pipe(ProcessPipeId::new(
                        u32::try_from(index).expect("pipeline stage ceiling fits u32"),
                    ))
                };
                match stateful_pipeline_invocation(
                    state,
                    builtin,
                    arguments,
                    command.redirections(),
                    command.span(),
                    index,
                    stdout,
                    &mut next_pipe_id,
                    &mut next_parent_file_id,
                    &mut parent_pipe_ends,
                    &mut parent_captures,
                ) {
                    Ok(invocation) => {
                        stages.push(PipelineStageInvocation::Stateful(invocation));
                    }
                    Err(error) => {
                        return redirection_failure(
                            error.message,
                            error.span,
                            &mut output.diagnostics,
                        );
                    }
                }
            }
            Ok(ResolvedCommand::StatefulBuiltin(builtin)) => {
                return unsupported_pipeline_stage(
                    builtin.name(),
                    command.span(),
                    &mut output.diagnostics,
                );
            }
            Ok(ResolvedCommand::Portable(portable)) => {
                let arguments = words[1..].to_vec();
                let reads_stdin = match portable_pipeline_reads_stdin(portable, &arguments) {
                    Ok(reads_stdin) => reads_stdin,
                    Err(error) => {
                        return invalid_arguments(&error, command.span(), &mut output.diagnostics);
                    }
                };
                let stdin = if index == 0 {
                    ParentTaskRedirectionEndpoint::Null
                } else {
                    ParentTaskRedirectionEndpoint::Pipe(ProcessPipeId::new(
                        u32::try_from(index - 1).expect("pipeline stage ceiling fits u32"),
                    ))
                };
                let stdout = if index + 1 == commands.len() {
                    ParentTaskRedirectionEndpoint::Capture(PipelineCaptureStream::Stdout)
                } else {
                    ParentTaskRedirectionEndpoint::Pipe(ProcessPipeId::new(
                        u32::try_from(index).expect("pipeline stage ceiling fits u32"),
                    ))
                };
                match portable_pipeline_invocation(
                    state,
                    portable,
                    arguments,
                    command.redirections(),
                    command.span(),
                    index,
                    reads_stdin,
                    stdin,
                    stdout,
                    &mut next_pipe_id,
                    &mut next_parent_file_id,
                    &mut parent_pipe_ends,
                    &mut parent_captures,
                ) {
                    Ok(invocation) => {
                        stages.push(PipelineStageInvocation::Portable(invocation));
                    }
                    Err(error) => {
                        return redirection_failure(
                            error.message,
                            error.span,
                            &mut output.diagnostics,
                        );
                    }
                }
            }
            Ok(
                ResolvedCommand::Alias { name, .. }
                | ResolvedCommand::Function { name }
                | ResolvedCommand::Wsl { command: name, .. },
            ) => {
                return unsupported_pipeline_stage(&name, command.span(), &mut output.diagnostics);
            }
            Err(error) => {
                return resolution_failure(
                    error,
                    name_span.expect("a non-empty expansion retains a source span"),
                    &mut output.diagnostics,
                );
            }
        }
    }

    let mut closed_pipe_ends = Vec::with_capacity((stages.len() - 1).saturating_mul(2));
    for index in 0..stages.len() - 1 {
        let pipe_id =
            ProcessPipeId::new(u32::try_from(index).expect("pipeline stage ceiling fits u32"));
        let pipe = ProcessStdio::Pipe(pipe_id);
        match &stages[index] {
            PipelineStageInvocation::Native(stage) => {
                if stage.stdout != pipe && stage.stderr != pipe {
                    closed_pipe_ends.push(ClosedProcessPipeEnd::Writer(pipe_id));
                }
            }
            PipelineStageInvocation::Portable(stage) => {
                if stage.stdout == ParentTaskStdio::Pipe(pipe_id) {
                    parent_pipe_ends.push(ParentProcessPipeEnd::Writer(pipe_id));
                } else {
                    closed_pipe_ends.push(ClosedProcessPipeEnd::Writer(pipe_id));
                }
            }
            PipelineStageInvocation::Stateful(stage) => {
                if stage.stdout == ParentTaskStdio::Pipe(pipe_id) {
                    parent_pipe_ends.push(ParentProcessPipeEnd::Writer(pipe_id));
                } else {
                    closed_pipe_ends.push(ClosedProcessPipeEnd::Writer(pipe_id));
                }
            }
        }
        match &stages[index + 1] {
            PipelineStageInvocation::Native(stage) => {
                if stage.stdin != pipe {
                    closed_pipe_ends.push(ClosedProcessPipeEnd::Reader(pipe_id));
                }
            }
            PipelineStageInvocation::Portable(stage) => {
                if stage.stdin == ParentTaskStdio::Pipe(pipe_id) {
                    parent_pipe_ends.push(ParentProcessPipeEnd::Reader(pipe_id));
                } else {
                    closed_pipe_ends.push(ClosedProcessPipeEnd::Reader(pipe_id));
                }
            }
            PipelineStageInvocation::Stateful(_) => {
                closed_pipe_ends.push(ClosedProcessPipeEnd::Reader(pipe_id));
            }
        }
    }

    match runner
        .run_pipeline(PipelineInvocation {
            stages,
            closed_pipe_ends,
            parent_pipe_ends,
            parent_captures,
            capture_limit,
        })
        .await
    {
        Ok(pipeline) if pipeline.exits.len() == commands.len() => {
            let Some(exit) = select_pipeline_exit(&pipeline.exits, state.options().pipefail())
            else {
                return process_failure(
                    "pipeline supervision returned no exit status".to_owned(),
                    span,
                    &mut output.diagnostics,
                    ShellStatusKind::SpawnError,
                    126,
                );
            };
            output.stdout.extend_from_slice(&pipeline.stdout);
            output.stderr.extend_from_slice(&pipeline.stderr);
            output.diagnostics.extend(pipeline.diagnostics);
            native_exit_status(exit)
        }
        Ok(_) => process_failure(
            "pipeline supervision returned incomplete exit status".to_owned(),
            span,
            &mut output.diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
        Err(NativeCommandError::CaptureLimit { .. }) => process_failure(
            format!(
                "pipeline output exceeds the {MAX_READ_FILE_BYTES}-byte synchronous shell capture ceiling"
            ),
            span,
            &mut output.diagnostics,
            ShellStatusKind::Exited,
            1,
        ),
        Err(NativeCommandError::Redirection(error)) => redirection_failure(
            format!("cannot apply pipeline redirection: {error}"),
            span,
            &mut output.diagnostics,
        ),
        Err(error) => process_failure(
            format!("cannot execute pipeline: {error}"),
            span,
            &mut output.diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
    }
}

fn execute_stateful_redirected(
    state: &mut ShellState,
    command: StatefulBuiltin,
    arguments: &[OsString],
    redirections: &[Redirection],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> (ShellStatus, Option<i64>) {
    if let Err(error) = validate_stateful_pipeline_arguments(command, arguments, state) {
        return (invalid_arguments(&error, span, diagnostics), None);
    }
    let mut next_parent_file_id = 0_u32;
    let plan = match plan_parent_task_redirections(
        state,
        redirections,
        ParentTaskRedirectionEndpoint::Null,
        ParentTaskRedirectionEndpoint::Null,
        ParentTaskRedirectionEndpoint::Null,
        &mut next_parent_file_id,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            return (
                redirection_failure(error.message, error.span, diagnostics),
                None,
            );
        }
    };
    let file_order = plan
        .files
        .iter()
        .map(|endpoint| ProcessGraphFile::Parent(endpoint.id))
        .collect::<Vec<_>>();
    match spawn_native_graph_with_parent_io(&[], &[], &[], &plan.files, &file_order) {
        Ok(graph) => drop(graph),
        Err(error) => {
            return (
                redirection_failure(
                    format!("cannot apply stateful redirection: {error}"),
                    span,
                    diagnostics,
                ),
                None,
            );
        }
    }
    execute_stateful_builtin(state, command, arguments, span, diagnostics)
}

async fn execute_portable_redirected<R>(
    state: &ShellState,
    command: PortableCommand,
    arguments: &[OsString],
    redirections: &[Redirection],
    span: SourceSpan,
    output: &mut ExecutionOutput,
    runner: &R,
) -> ShellStatus
where
    R: NativeCommandRunner + ?Sized,
{
    let reads_stdin = match portable_pipeline_reads_stdin(command, arguments) {
        Ok(reads_stdin) => reads_stdin,
        Err(error) => return invalid_arguments(&error, span, &mut output.diagnostics),
    };
    let mut parent_pipe_ends = Vec::new();
    let mut parent_captures = Vec::new();
    let mut next_pipe_id = 0_u32;
    let mut next_parent_file_id = 0_u32;
    let invocation = match portable_pipeline_invocation(
        state,
        command,
        arguments.to_vec(),
        redirections,
        span,
        0,
        reads_stdin,
        ParentTaskRedirectionEndpoint::Null,
        ParentTaskRedirectionEndpoint::Capture(PipelineCaptureStream::Stdout),
        &mut next_pipe_id,
        &mut next_parent_file_id,
        &mut parent_pipe_ends,
        &mut parent_captures,
    ) {
        Ok(invocation) => invocation,
        Err(error) => {
            return redirection_failure(error.message, error.span, &mut output.diagnostics);
        }
    };
    if reads_stdin && invocation.stdin == ParentTaskStdio::Null {
        return invalid_arguments(
            &format!(
                "{} standard input requires a `<` redirection in a simple command",
                command.name()
            ),
            span,
            &mut output.diagnostics,
        );
    }

    match runner
        .run_pipeline(PipelineInvocation {
            stages: vec![PipelineStageInvocation::Portable(invocation)],
            closed_pipe_ends: Vec::new(),
            parent_pipe_ends,
            parent_captures,
            capture_limit: remaining_capture(output),
        })
        .await
    {
        Ok(portable) if portable.exits.len() == 1 => {
            output.stdout.extend_from_slice(&portable.stdout);
            output.stderr.extend_from_slice(&portable.stderr);
            output.diagnostics.extend(portable.diagnostics);
            native_exit_status(portable.exits[0])
        }
        Ok(_) => process_failure(
            "portable command supervision returned incomplete exit status".to_owned(),
            span,
            &mut output.diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
        Err(NativeCommandError::CaptureLimit { .. }) => process_failure(
            format!(
                "portable command output exceeds the {MAX_READ_FILE_BYTES}-byte synchronous shell capture ceiling"
            ),
            span,
            &mut output.diagnostics,
            ShellStatusKind::Exited,
            1,
        ),
        Err(NativeCommandError::Redirection(error)) => redirection_failure(
            format!("cannot apply portable redirection: {error}"),
            span,
            &mut output.diagnostics,
        ),
        Err(error) => process_failure(
            format!("cannot execute portable command: {error}"),
            span,
            &mut output.diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
    }
}

async fn execute_native<R>(
    state: &ShellState,
    executable: PathBuf,
    arguments: &[OsString],
    redirections: &[Redirection],
    span: SourceSpan,
    output: &mut ExecutionOutput,
    runner: &R,
) -> ShellStatus
where
    R: NativeCommandRunner + ?Sized,
{
    let invocation = match native_invocation(
        state,
        executable.clone(),
        arguments,
        redirections,
        remaining_capture(output),
        NativeStdio {
            stdin: ProcessStdio::Null,
            stdout: ProcessStdio::Capture(STDOUT_CAPTURE),
            stderr: ProcessStdio::Capture(STDERR_CAPTURE),
        },
    ) {
        Ok(invocation) => invocation,
        Err(error) => {
            return redirection_failure(error.message, error.span, &mut output.diagnostics);
        }
    };
    match runner.run(invocation).await {
        Ok(native) => {
            output.stdout.extend_from_slice(&native.stdout);
            output.stderr.extend_from_slice(&native.stderr);
            native_exit_status(native.exit)
        }
        Err(NativeCommandError::CaptureLimit { .. }) => process_failure(
            format!(
                "native command output exceeds the {MAX_READ_FILE_BYTES}-byte synchronous shell capture ceiling"
            ),
            span,
            &mut output.diagnostics,
            ShellStatusKind::Exited,
            1,
        ),
        Err(NativeCommandError::Redirection(error)) => redirection_failure(
            format!(
                "cannot apply redirection for `{}`: {error}",
                display_os_string(executable.as_os_str())
            ),
            span,
            &mut output.diagnostics,
        ),
        Err(error) => process_failure(
            format!(
                "cannot execute `{}`: {error}",
                display_os_string(executable.as_os_str())
            ),
            span,
            &mut output.diagnostics,
            ShellStatusKind::SpawnError,
            126,
        ),
    }
}

fn native_invocation(
    state: &ShellState,
    executable: PathBuf,
    arguments: &[OsString],
    redirections: &[Redirection],
    capture_limit: u64,
    stdio: NativeStdio,
) -> Result<NativeInvocation, RedirectionPlanError> {
    let NativeStdio {
        mut stdin,
        mut stdout,
        mut stderr,
    } = stdio;
    let mut files = Vec::with_capacity(redirections.len());
    for redirection in redirections {
        let endpoint = match redirection.target() {
            RedirectionTarget::File { path, mode } => {
                if !matches!(
                    (redirection.descriptor(), mode),
                    (RedirectionDescriptor::Stdin, RedirectionFileMode::Read)
                        | (
                            RedirectionDescriptor::Stdout | RedirectionDescriptor::Stderr,
                            RedirectionFileMode::Write | RedirectionFileMode::Append
                        )
                ) {
                    return Err(RedirectionPlanError {
                        message: "redirection file mode does not match its descriptor".to_owned(),
                        span: redirection.operator_span(),
                    });
                }
                let mut expanded = expand_words(std::slice::from_ref(path), state).into_iter();
                let Some(target) = expanded.next() else {
                    return Err(RedirectionPlanError {
                        message: "redirection target expands to no path".to_owned(),
                        span: path.span(),
                    });
                };
                if expanded.next().is_some() {
                    return Err(RedirectionPlanError {
                        message: "redirection target expands to multiple paths".to_owned(),
                        span: path.span(),
                    });
                }
                let id = ProcessFileId::new(u32::try_from(files.len()).map_err(|_| {
                    RedirectionPlanError {
                        message: "redirection plan contains too many file targets".to_owned(),
                        span: redirection.span(),
                    }
                })?);
                files.push(NativeProcessFile {
                    id,
                    path: state.cwd().join(PathBuf::from(target.into_value())),
                    mode: match mode {
                        RedirectionFileMode::Read => NativeProcessFileMode::Read,
                        RedirectionFileMode::Write => NativeProcessFileMode::Write,
                        RedirectionFileMode::Append => NativeProcessFileMode::Append,
                    },
                });
                ProcessStdio::File(id)
            }
            RedirectionTarget::Descriptor(source) => match source {
                RedirectionDescriptor::Stdin => stdin,
                RedirectionDescriptor::Stdout => stdout,
                RedirectionDescriptor::Stderr => stderr,
            },
        };
        match redirection.descriptor() {
            RedirectionDescriptor::Stdin => stdin = endpoint,
            RedirectionDescriptor::Stdout => stdout = endpoint,
            RedirectionDescriptor::Stderr => stderr = endpoint,
        }
    }

    Ok(NativeInvocation {
        executable,
        arguments: arguments.to_vec(),
        cwd: state.cwd().to_owned(),
        environment: state
            .environment()
            .iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        capture_limit,
        files,
        stdin,
        stdout,
        stderr,
    })
}

#[derive(Debug)]
struct RedirectionPlanError {
    message: String,
    span: SourceSpan,
}

fn remaining_capture(output: &ExecutionOutput) -> u64 {
    let captured = u64::try_from(output.stdout.len())
        .unwrap_or(u64::MAX)
        .saturating_add(u64::try_from(output.stderr.len()).unwrap_or(u64::MAX));
    MAX_READ_FILE_BYTES.saturating_sub(captured)
}

fn unsupported_pipeline_stage(
    name: &str,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    unsupported(
        format!("pipeline stage `{name}` requires a streaming adapter that is not implemented yet"),
        span,
        diagnostics,
        ShellStatusKind::Exited,
        2,
    )
}

fn native_exit_status(exit: ProcessExit) -> ShellStatus {
    let (code, kind) = if let Some(signal) = exit.signal {
        (128_i64.saturating_add(signal), ShellStatusKind::Interrupted)
    } else {
        (
            exit.code.unwrap_or_else(|| i64::from(!exit.success)),
            ShellStatusKind::Exited,
        )
    };
    ShellStatus::new(code, kind, exit.signal, ExecutionBackend::Native)
}

fn select_pipeline_exit(exits: &[ProcessExit], pipefail: bool) -> Option<ProcessExit> {
    if pipefail {
        exits
            .iter()
            .rev()
            .copied()
            .find(|exit| !exit.success)
            .or_else(|| exits.last().copied())
    } else {
        exits.last().copied()
    }
}

const fn stateful_builtin_is_implemented(command: StatefulBuiltin) -> bool {
    matches!(
        command,
        StatefulBuiltin::Cd
            | StatefulBuiltin::Export
            | StatefulBuiltin::Unset
            | StatefulBuiltin::Set
            | StatefulBuiltin::Exit
    )
}

fn execute_stateful_builtin(
    state: &mut ShellState,
    command: StatefulBuiltin,
    arguments: &[OsString],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> (ShellStatus, Option<i64>) {
    let status = match command {
        StatefulBuiltin::Cd => execute_cd(state, arguments, span, diagnostics),
        StatefulBuiltin::Export => execute_export(state, arguments, span, diagnostics),
        StatefulBuiltin::Unset => execute_unset(state, arguments, span, diagnostics),
        StatefulBuiltin::Set => execute_set(state, arguments, span, diagnostics),
        StatefulBuiltin::Exit => return execute_exit(state, arguments, span, diagnostics),
        StatefulBuiltin::Alias
        | StatefulBuiltin::Jobs
        | StatefulBuiltin::Foreground
        | StatefulBuiltin::Background => {
            unreachable!("unsupported stateful commands are rejected before execution")
        }
    };
    (status, None)
}

fn validate_stateful_pipeline_arguments(
    command: StatefulBuiltin,
    arguments: &[OsString],
    state: &ShellState,
) -> Result<(), String> {
    match command {
        StatefulBuiltin::Cd => validate_cd_arguments(arguments),
        StatefulBuiltin::Export => parse_export_assignment(arguments).map(|_| ()),
        StatefulBuiltin::Unset => parse_unset_name(arguments).map(|_| ()),
        StatefulBuiltin::Set => parse_pipefail_setting(arguments).map(|_| ()),
        StatefulBuiltin::Exit => parse_exit_status(state, arguments).map(|_| ()),
        StatefulBuiltin::Alias
        | StatefulBuiltin::Jobs
        | StatefulBuiltin::Foreground
        | StatefulBuiltin::Background => {
            unreachable!("unsupported stateful pipeline commands are rejected separately")
        }
    }
}

fn portable_pipeline_reads_stdin(
    command: PortableCommand,
    arguments: &[OsString],
) -> Result<bool, String> {
    match command {
        PortableCommand::Pwd => {
            if arguments.is_empty() {
                Ok(false)
            } else {
                Err("pwd does not accept arguments".to_owned())
            }
        }
        PortableCommand::Echo => {
            parse_echo_arguments(arguments)?;
            Ok(false)
        }
        PortableCommand::List => {
            parse_ls_options(arguments)?;
            Ok(false)
        }
        PortableCommand::Cat => {
            let target = parse_cat_path_with_stdin(arguments, true)?;
            Ok(target == "-")
        }
        PortableCommand::Grep => {
            let options = parse_grep_options_with_stdin(arguments, true)?;
            PipelineGrepMatcher::new(&options)
                .map_err(|error| format!("grep regular expression is invalid: {error}"))?;
            Ok(options.path == "-")
        }
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
    let (arguments, newline) = match parse_echo_arguments(arguments) {
        Ok(parsed) => parsed,
        Err(message) => return invalid_arguments(&message, span, diagnostics),
    };
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

fn parse_echo_arguments(arguments: &[OsString]) -> Result<(&[OsString], bool), String> {
    if arguments.first().is_some_and(|argument| argument == "-n") {
        Ok((&arguments[1..], false))
    } else if arguments.first().is_some_and(|argument| argument == "--") {
        Ok((&arguments[1..], true))
    } else if arguments.first().is_some_and(|argument| {
        argument
            .to_str()
            .is_some_and(|argument| argument.starts_with('-') && argument != "-")
    }) {
        Err("echo supports only the `-n` option".to_owned())
    } else {
        Ok((arguments, true))
    }
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
    parse_cat_path_with_stdin(arguments, false)
}

fn parse_cat_path_with_stdin(
    arguments: &[OsString],
    allow_stdin: bool,
) -> Result<OsString, String> {
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
    if target == "-" && !allow_stdin {
        return Err("cat standard-input operand `-` is not implemented yet".to_owned());
    }
    if target != "-"
        && target
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

struct GrepCommandOptions {
    query: String,
    path: OsString,
    pattern: SemanticSearchPattern,
    case_insensitive: bool,
    line_number: bool,
}

fn execute_grep(
    state: &ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    stdout: &mut Vec<u8>,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let options = match parse_grep_options(arguments) {
        Ok(options) => options,
        Err(message) => return invalid_arguments(&message, span, diagnostics),
    };
    if options.path.is_empty() {
        return filesystem_failure("cannot search: path is empty".to_owned(), span, diagnostics);
    }

    let filesystem = match NativeFileSystem::new(state.cwd()) {
        Ok(filesystem) => filesystem,
        Err(error) => {
            return filesystem_failure(
                format!("cannot search from the current directory: {error}"),
                span,
                diagnostics,
            );
        }
    };
    let target = PathBuf::from(&options.path);
    let resolved = match filesystem.resolve_existing(&target) {
        Ok(resolved) => resolved,
        Err(error) => {
            return search_filesystem_failure(&options.path, error, span, diagnostics);
        }
    };
    let metadata = match fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(error) => {
            return search_filesystem_failure(&options.path, error, span, diagnostics);
        }
    };
    if !metadata.is_file() {
        return search_filesystem_failure(
            &options.path,
            "path is not a regular file",
            span,
            diagnostics,
        );
    }

    let services = SemanticServices::new(filesystem);
    let result = match services.search_serial(&SearchQuery::new(
        options.query,
        vec![resolved],
        options.pattern,
        options.case_insensitive,
        false,
    )) {
        Ok(result) => result,
        Err(SemanticError::Regex(error)) => {
            return invalid_arguments(
                &format!("grep regular expression is invalid: {error}"),
                span,
                diagnostics,
            );
        }
        Err(error) => {
            return search_filesystem_failure(&options.path, error, span, diagnostics);
        }
    };
    if result.partial {
        return search_filesystem_failure(
            &options.path,
            "file is not valid UTF-8",
            span,
            diagnostics,
        );
    }
    if result.matches.is_empty() {
        return shell_status(1, ShellStatusKind::Exited);
    }

    let mut rendered = Vec::new();
    for matched in result.matches {
        if options.line_number {
            rendered.extend_from_slice(matched.line.to_string().as_bytes());
            rendered.push(b':');
        }
        rendered.extend_from_slice(matched.text.as_bytes());
        rendered.push(b'\n');
    }
    let remaining = MAX_READ_FILE_BYTES.saturating_sub(stdout.len() as u64);
    if rendered.len() as u64 > remaining {
        return search_filesystem_failure(
            &options.path,
            format!(
                "output exceeds the {MAX_READ_FILE_BYTES}-byte synchronous shell capture ceiling"
            ),
            span,
            diagnostics,
        );
    }
    stdout.extend_from_slice(&rendered);
    ShellStatus::success()
}

fn parse_grep_options(arguments: &[OsString]) -> Result<GrepCommandOptions, String> {
    parse_grep_options_with_stdin(arguments, false)
}

fn parse_grep_options_with_stdin(
    arguments: &[OsString],
    allow_stdin: bool,
) -> Result<GrepCommandOptions, String> {
    let mut selected_pattern = None;
    let mut case_insensitive = false;
    let mut line_number = false;
    let mut options_ended = false;
    let mut operands = Vec::new();

    for argument in arguments {
        if !options_ended {
            if argument == "--" {
                options_ended = true;
                continue;
            }
            match argument.to_str() {
                Some("--extended-regexp") => {
                    select_grep_pattern(&mut selected_pattern, SemanticSearchPattern::Regex)?;
                    continue;
                }
                Some("--fixed-strings") => {
                    select_grep_pattern(&mut selected_pattern, SemanticSearchPattern::Literal)?;
                    continue;
                }
                Some("--ignore-case") => {
                    case_insensitive = true;
                    continue;
                }
                Some("--line-number") => {
                    line_number = true;
                    continue;
                }
                Some(value) if value.starts_with('-') && value != "-" => {
                    if value.starts_with("--") {
                        return Err(format!(
                            "grep does not support option `{}`",
                            display_os_string(argument)
                        ));
                    }
                    for option in &value.as_bytes()[1..] {
                        match option {
                            b'E' => select_grep_pattern(
                                &mut selected_pattern,
                                SemanticSearchPattern::Regex,
                            )?,
                            b'F' => select_grep_pattern(
                                &mut selected_pattern,
                                SemanticSearchPattern::Literal,
                            )?,
                            b'i' => case_insensitive = true,
                            b'n' => line_number = true,
                            _ => {
                                return Err(format!(
                                    "grep does not support option `{}`",
                                    display_os_string(argument)
                                ));
                            }
                        }
                    }
                    continue;
                }
                _ => {}
            }
        }
        operands.push(argument.clone());
    }

    match operands.len() {
        0 => return Err("grep requires a pattern and one path".to_owned()),
        1 => return Err("grep requires one path".to_owned()),
        2 => {}
        _ => return Err("grep accepts exactly one pattern and one path".to_owned()),
    }
    let query = operands[0]
        .to_str()
        .ok_or_else(|| "grep pattern must be valid UTF-8".to_owned())?
        .to_owned();
    let path = operands[1].clone();
    if path == "-" && !allow_stdin {
        return Err("grep standard-input operand `-` is not implemented yet".to_owned());
    }
    Ok(GrepCommandOptions {
        query,
        path,
        pattern: selected_pattern.unwrap_or(SemanticSearchPattern::Regex),
        case_insensitive,
        line_number,
    })
}

fn select_grep_pattern(
    selected: &mut Option<SemanticSearchPattern>,
    pattern: SemanticSearchPattern,
) -> Result<(), String> {
    if selected.is_some_and(|selected| selected != pattern) {
        return Err("grep options `-E` and `-F` cannot be combined".to_owned());
    }
    *selected = Some(pattern);
    Ok(())
}

fn search_filesystem_failure(
    target: &OsStr,
    error: impl std::fmt::Display,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    filesystem_failure(
        format!("cannot search `{}`: {error}", display_os_string(target)),
        span,
        diagnostics,
    )
}

fn execute_export(
    state: &mut ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let (name, value) = match parse_export_assignment(arguments) {
        Ok(assignment) => assignment,
        Err(message) => return invalid_arguments(&message, span, diagnostics),
    };
    if let Err(error) = state.set_variable(name.clone(), value.clone()) {
        return invalid_arguments(&format!("export: {error}"), span, diagnostics);
    }
    state.environment_mut().insert(name, value);
    ShellStatus::success()
}

fn execute_unset(
    state: &mut ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let name = match parse_unset_name(arguments) {
        Ok(name) => name,
        Err(message) => return invalid_arguments(&message, span, diagnostics),
    };
    state.unset_variable(&name);
    state.environment_mut().remove(&name);
    ShellStatus::success()
}

fn execute_set(
    state: &mut ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    let enabled = match parse_pipefail_setting(arguments) {
        Ok(enabled) => enabled,
        Err(message) => return invalid_arguments(&message, span, diagnostics),
    };
    state.options_mut().set_pipefail(enabled);
    ShellStatus::success()
}

fn parse_pipefail_setting(arguments: &[OsString]) -> Result<bool, String> {
    match arguments {
        [mode, option] if mode == OsStr::new("-o") && option == OsStr::new("pipefail") => Ok(true),
        [mode, option] if mode == OsStr::new("+o") && option == OsStr::new("pipefail") => Ok(false),
        _ => Err("set supports exactly `-o pipefail` or `+o pipefail`".to_owned()),
    }
}

fn execute_exit(
    state: &ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> (ShellStatus, Option<i64>) {
    match parse_exit_status(state, arguments) {
        Ok(code) => (shell_status(code, ShellStatusKind::Exited), Some(code)),
        Err(message) => (invalid_arguments(&message, span, diagnostics), None),
    }
}

fn parse_exit_status(state: &ShellState, arguments: &[OsString]) -> Result<i64, String> {
    let arguments = if arguments.first().is_some_and(|argument| argument == "--") {
        &arguments[1..]
    } else {
        arguments
    };
    let code = match arguments {
        [] => state.last_status().code(),
        [code] => {
            let Some(code) = code.to_str() else {
                return Err("exit status must be valid UTF-8".to_owned());
            };
            let Ok(code) = code.parse::<i64>() else {
                return Err("exit status must be an integer from 0 through 255".to_owned());
            };
            if !(0..=i64::from(u8::MAX)).contains(&code) {
                return Err("exit status must be an integer from 0 through 255".to_owned());
            }
            code
        }
        _ => {
            return Err("exit accepts at most one status".to_owned());
        }
    };
    Ok(code)
}

fn parse_export_assignment(arguments: &[OsString]) -> Result<(String, OsString), String> {
    let assignment = parse_single_state_argument(arguments, "export", "NAME=VALUE assignment")?;
    let assignment = assignment
        .to_str()
        .ok_or_else(|| "export assignment must be valid UTF-8".to_owned())?;
    let Some((name, value)) = assignment.split_once('=') else {
        return Err("export requires a NAME=VALUE assignment".to_owned());
    };
    validate_identifier(name).map_err(|error| format!("export: {error}"))?;
    Ok((name.to_owned(), OsString::from(value)))
}

fn parse_unset_name(arguments: &[OsString]) -> Result<String, String> {
    let name = parse_single_state_argument(arguments, "unset", "name")?;
    let name = name
        .to_str()
        .ok_or_else(|| "unset name must be valid UTF-8".to_owned())?;
    validate_identifier(name).map_err(|error| format!("unset: {error}"))?;
    Ok(name.to_owned())
}

fn parse_single_state_argument(
    arguments: &[OsString],
    command: &str,
    description: &str,
) -> Result<OsString, String> {
    let arguments = if arguments.first().is_some_and(|argument| argument == "--") {
        &arguments[1..]
    } else {
        if let Some(option) = arguments.first().filter(|argument| {
            argument
                .to_str()
                .is_some_and(|argument| argument.starts_with('-') && argument != "-")
        }) {
            return Err(format!(
                "{command} does not support option `{}`",
                display_os_string(option)
            ));
        }
        arguments
    };
    match arguments.len() {
        0 => Err(format!("{command} requires exactly one {description}")),
        1 => Ok(arguments[0].clone()),
        _ => Err(format!("{command} accepts exactly one {description}")),
    }
}

fn execute_cd(
    state: &mut ShellState,
    arguments: &[OsString],
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    if let Err(message) = validate_cd_arguments(arguments) {
        return invalid_arguments(&message, span, diagnostics);
    }
    let target = if let Some(target) = arguments.first() {
        if target.is_empty() {
            return filesystem_failure(
                "cannot change directory: path is empty".to_owned(),
                span,
                diagnostics,
            );
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

fn validate_cd_arguments(arguments: &[OsString]) -> Result<(), String> {
    if arguments.len() > 1 {
        return Err("cd accepts at most one path".to_owned());
    }
    if arguments.first().is_some_and(|target| target == "-") {
        return Err("cd - is not implemented yet".to_owned());
    }
    Ok(())
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

fn invalid_command_name(
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    diagnostics.push(ExecutionDiagnostic {
        code: ExecutionDiagnosticCode::Resolution,
        message: "expanded command name must be valid UTF-8".to_owned(),
        span,
    });
    shell_status(127, ShellStatusKind::ResolutionError)
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

fn redirection_failure(
    message: String,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
) -> ShellStatus {
    diagnostics.push(ExecutionDiagnostic {
        code: ExecutionDiagnosticCode::Redirection,
        message,
        span,
    });
    shell_status(1, ShellStatusKind::RedirectionError)
}

fn process_failure(
    message: String,
    span: SourceSpan,
    diagnostics: &mut Vec<ExecutionDiagnostic>,
    kind: ShellStatusKind,
    code: i64,
) -> ShellStatus {
    diagnostics.push(ExecutionDiagnostic {
        code: ExecutionDiagnosticCode::Process,
        message,
        span,
    });
    shell_status(code, kind)
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
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use ash_platform::{
        ClosedProcessPipeEnd, NativeProcessFileMode, ParentProcessFileId, ParentProcessPipeEnd,
        ProcessExit, ProcessPipeId, ProcessStdio,
    };

    use super::{
        ExecutionDiagnosticCode, NativeCommandError, NativeCommandOutput, NativeCommandRunner,
        NativeInvocation, ParentPipelineCapture, ParentTaskStdio, PipelineCaptureStream,
        PipelineInvocation, PipelineOutput, PipelineStageInvocation, STDERR_CAPTURE,
        STDOUT_CAPTURE, execute_source, execute_source_with, execute_source_with_runner,
        run_pipeline,
    };
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

    fn compile_pipeline_helper(directory: &TestDirectory) -> String {
        let bin_directory = directory.0.join("bin");
        fs::create_dir(&bin_directory).expect("bin directory");
        let source = directory.0.join("pipeline-helper.rs");
        fs::write(
            &source,
            r#"
use std::{
    convert::TryFrom,
    env, fs,
    io::{self, Read, Write},
    process, thread,
    time::Duration,
};

fn write_payload(length: usize) {
    let mut output = io::stdout().lock();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut offset = 0_usize;
    while offset < length {
        let count = (length - offset).min(buffer.len());
        for (index, byte) in buffer[..count].iter_mut().enumerate() {
            *byte = u8::try_from((offset + index) % 251).expect("bounded byte");
        }
        output.write_all(&buffer[..count]).expect("write payload");
        offset += count;
    }
    output.flush().expect("flush payload");
}

fn main() {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        Some("produce") => {
            let length = arguments
                .next()
                .expect("payload length")
                .parse::<usize>()
                .expect("numeric payload length");
            write_payload(length);
            eprintln!("producer-stderr");
        }
        Some("produce-after-input") => {
            let length = arguments
                .next()
                .expect("payload length")
                .parse::<usize>()
                .expect("numeric payload length");
            let mut byte = [0_u8; 1];
            io::stdin()
                .lock()
                .read_exact(&mut byte)
                .expect("read producer gate");
            write_payload(length);
        }
        Some("produce-soft") => {
            let length = arguments
                .next()
                .expect("payload length")
                .parse::<usize>()
                .expect("numeric payload length");
            let mut output = io::stdout().lock();
            let mut buffer = vec![0_u8; 64 * 1024];
            let mut offset = 0_usize;
            while offset < length {
                let count = (length - offset).min(buffer.len());
                for (index, byte) in buffer[..count].iter_mut().enumerate() {
                    *byte = u8::try_from((offset + index) % 251).expect("bounded byte");
                }
                if let Err(error) = output.write_all(&buffer[..count]) {
                    if error.kind() == io::ErrorKind::BrokenPipe {
                        process::exit(9);
                    }
                    panic!("write payload: {}", error);
                }
                offset += count;
            }
            output.flush().expect("flush payload");
            eprintln!("producer-stderr");
        }
        Some("copy") => {
            let label = arguments.next().expect("copy label");
            let mut input = io::stdin().lock();
            let mut output = io::stdout().lock();
            io::copy(&mut input, &mut output).expect("copy pipeline stream");
            output.flush().expect("flush copied stream");
            eprintln!("{label}-stderr");
        }
        Some("ordered") => {
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            stdout.write_all(b"stdout-a\n").expect("write stdout a");
            stdout.flush().expect("flush stdout a");
            stderr.write_all(b"stderr-a\n").expect("write stderr a");
            stderr.flush().expect("flush stderr a");
            stdout.write_all(b"stdout-b\n").expect("write stdout b");
            stdout.flush().expect("flush stdout b");
            stderr.write_all(b"stderr-b\n").expect("write stderr b");
            stderr.flush().expect("flush stderr b");
        }
        Some("hold") => {
            let ready = arguments.next().expect("ready path");
            let escaped = arguments.next().expect("escaped path");
            let _child = process::Command::new(env::current_exe().expect("current executable"))
                .arg("child")
                .arg(escaped)
                .spawn()
                .expect("spawn descendant");
            fs::write(ready, b"ready").expect("write ready marker");
            let mut output = io::stdout().lock();
            output.write_all(b"x").expect("open producer gate");
            output.flush().expect("flush producer gate");
            thread::sleep(Duration::from_secs(10));
        }
        Some("child") => {
            let escaped = arguments.next().expect("escaped path");
            thread::sleep(Duration::from_secs(1));
            fs::write(escaped, b"escaped").expect("write escaped marker");
            thread::sleep(Duration::from_secs(10));
        }
        _ => process::exit(2),
    }
}
"#,
        )
        .expect("write pipeline helper");
        let executable_name = if cfg!(windows) {
            "pipeline-helper.exe"
        } else {
            "pipeline-helper"
        };
        let status = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(bin_directory.join(executable_name))
            .status()
            .expect("run rustc");
        assert!(status.success(), "compile pipeline helper");
        executable_name.to_owned()
    }

    struct RecordingRunner {
        invocation: Mutex<Option<NativeInvocation>>,
        output: Mutex<Option<NativeCommandOutput>>,
    }

    impl RecordingRunner {
        fn new(output: NativeCommandOutput) -> Self {
            Self {
                invocation: Mutex::new(None),
                output: Mutex::new(Some(output)),
            }
        }
    }

    impl NativeCommandRunner for RecordingRunner {
        async fn run(
            &self,
            invocation: NativeInvocation,
        ) -> Result<NativeCommandOutput, NativeCommandError> {
            *self.invocation.lock().expect("invocation lock") = Some(invocation);
            Ok(self
                .output
                .lock()
                .expect("output lock")
                .take()
                .expect("one output"))
        }

        async fn run_pipeline(
            &self,
            _invocation: PipelineInvocation,
        ) -> Result<PipelineOutput, NativeCommandError> {
            panic!("single-command recording runner received a pipeline")
        }
    }

    struct FailingRunner {
        capture_limit: bool,
    }

    impl NativeCommandRunner for FailingRunner {
        async fn run(
            &self,
            invocation: NativeInvocation,
        ) -> Result<NativeCommandOutput, NativeCommandError> {
            if self.capture_limit {
                Err(NativeCommandError::CaptureLimit {
                    max: invocation.capture_limit,
                })
            } else {
                Err(NativeCommandError::MissingStream("stdout"))
            }
        }

        async fn run_pipeline(
            &self,
            invocation: PipelineInvocation,
        ) -> Result<PipelineOutput, NativeCommandError> {
            if self.capture_limit {
                Err(NativeCommandError::CaptureLimit {
                    max: invocation.capture_limit,
                })
            } else {
                Err(NativeCommandError::MissingStream("final stdout"))
            }
        }
    }

    struct RecordingPipelineRunner {
        invocation: Mutex<Option<PipelineInvocation>>,
        output: Mutex<Option<PipelineOutput>>,
    }

    impl RecordingPipelineRunner {
        fn new(output: PipelineOutput) -> Self {
            Self {
                invocation: Mutex::new(None),
                output: Mutex::new(Some(output)),
            }
        }
    }

    impl NativeCommandRunner for RecordingPipelineRunner {
        async fn run(
            &self,
            _invocation: NativeInvocation,
        ) -> Result<NativeCommandOutput, NativeCommandError> {
            panic!("pipeline recording runner received a simple command")
        }

        async fn run_pipeline(
            &self,
            invocation: PipelineInvocation,
        ) -> Result<PipelineOutput, NativeCommandError> {
            *self.invocation.lock().expect("invocation lock") = Some(invocation);
            Ok(self
                .output
                .lock()
                .expect("output lock")
                .take()
                .expect("one pipeline output"))
        }
    }

    fn successful_pipeline_output(stage_count: usize) -> PipelineOutput {
        PipelineOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exits: std::iter::repeat_n(
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
                stage_count,
            )
            .collect(),
            diagnostics: Vec::new(),
        }
    }

    #[tokio::test]
    async fn pwd_echo_and_cd_share_state_across_sequential_commands() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        let mut state = ShellState::new(&directory.0);

        let execution = execute_source(
            "pwd; echo \"hello world\"; cd child; pwd; echo -n done",
            &mut state,
        )
        .await;

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

    #[tokio::test]
    async fn exit_stops_the_current_source_and_exposes_the_requested_status() {
        let mut state = ShellState::new(".");

        let execution = execute_source("echo before; exit 23; echo after", &mut state).await;

        assert_eq!(execution.stdout(), b"before\n");
        assert!(execution.stderr().is_empty());
        assert!(execution.diagnostics().is_empty());
        assert_eq!(execution.status().code(), 23);
        assert_eq!(execution.exit_requested(), Some(23));
        assert_eq!(state.last_status().code(), 23);
    }

    #[tokio::test]
    async fn exit_without_status_uses_the_previous_command_status() {
        let mut state = ShellState::new(".");
        let lookup = |_command: &str,
                      _cwd: &std::path::Path,
                      _environment: &crate::PlatformEnvironment| None;

        let execution = execute_source_with(
            "missing; exit; echo after",
            &mut state,
            &lookup,
            HostPlatform::Linux,
        )
        .await;

        assert!(execution.stdout().is_empty());
        assert_eq!(execution.diagnostics().len(), 1);
        assert_eq!(execution.status().code(), 127);
        assert_eq!(execution.exit_requested(), Some(127));
        assert_eq!(state.last_status().code(), 127);
    }

    #[tokio::test]
    async fn invalid_exit_status_reports_status_two_without_exiting() {
        for source in [
            "exit -1; echo recovered",
            "exit 256; echo recovered",
            "exit nope; echo recovered",
            "exit 1 2; echo recovered",
        ] {
            let mut state = ShellState::new(".");
            let execution = execute_source(source, &mut state).await;

            assert_eq!(execution.stdout(), b"recovered\n", "source={source}");
            assert_eq!(execution.diagnostics().len(), 1, "source={source}");
            assert_eq!(
                execution.diagnostics()[0].code(),
                ExecutionDiagnosticCode::InvalidArguments,
                "source={source}"
            );
            assert_eq!(execution.exit_requested(), None, "source={source}");
            assert_eq!(execution.status().code(), 0, "source={source}");
        }
    }

    #[tokio::test]
    async fn empty_source_preserves_the_previous_status() {
        let mut state = ShellState::new(".");
        state.set_last_status(crate::ShellStatus::new(
            37,
            ShellStatusKind::Exited,
            None,
            crate::ExecutionBackend::Native,
        ));

        let execution = execute_source("# no command", &mut state).await;

        assert_eq!(execution.status().code(), 37);
        assert_eq!(execution.exit_requested(), None);
        assert_eq!(state.last_status().code(), 37);
    }

    #[tokio::test]
    async fn failed_resolution_is_typed_and_later_commands_still_run() {
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
        )
        .await;

        assert_eq!(execution.stdout(), b"recovered\n");
        assert_eq!(execution.diagnostics().len(), 1);
        assert_eq!(
            execution.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Resolution
        );
        assert_eq!(execution.status().code(), 0);
        assert_eq!(state.last_status().code(), 0);
    }

    #[tokio::test]
    async fn native_execution_preserves_exact_state_argv_output_and_status() {
        let mut state = ShellState::new("/fixture/work");
        state.environment_mut().insert("PATH", "/fixture/bin");
        state
            .environment_mut()
            .insert("ASH_NATIVE_TOKEN", "present");
        let lookup =
            |command: &str, _cwd: &std::path::Path, _environment: &crate::PlatformEnvironment| {
                (command == "tool").then(|| PathBuf::from("/fixture/bin/tool"))
            };
        let runner = RecordingRunner::new(NativeCommandOutput {
            stdout: b"native-stdout\n".to_vec(),
            stderr: b"native-stderr\n".to_vec(),
            exit: ProcessExit {
                success: false,
                code: Some(23),
                signal: None,
            },
        });

        let execution = execute_source_with_runner(
            "native:tool 'alpha beta' plain",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &runner,
        )
        .await;

        assert_eq!(execution.stdout(), b"native-stdout\n");
        assert_eq!(execution.stderr(), b"native-stderr\n");
        assert_eq!(execution.rendered_stderr(), b"native-stderr\n");
        assert!(execution.diagnostics().is_empty());
        assert_eq!(execution.status().code(), 23);
        assert_eq!(execution.status().kind(), ShellStatusKind::Exited);
        assert_eq!(state.last_status().code(), 23);
        let invocation = runner
            .invocation
            .lock()
            .expect("invocation lock")
            .clone()
            .expect("invocation");
        assert_eq!(invocation.executable, PathBuf::from("/fixture/bin/tool"));
        assert_eq!(invocation.cwd, PathBuf::from("/fixture/work"));
        assert_eq!(
            invocation.arguments,
            [OsString::from("alpha beta"), OsString::from("plain")]
        );
        assert_eq!(
            invocation.environment,
            [
                (
                    OsString::from("ASH_NATIVE_TOKEN"),
                    OsString::from("present"),
                ),
                (OsString::from("PATH"), OsString::from("/fixture/bin")),
            ]
        );
        assert_eq!(invocation.capture_limit, super::MAX_READ_FILE_BYTES);
        assert!(invocation.files.is_empty());
        assert_eq!(invocation.stdin, ProcessStdio::Null);
        assert_eq!(
            invocation.stdout,
            ProcessStdio::Capture(super::STDOUT_CAPTURE)
        );
        assert_eq!(
            invocation.stderr,
            ProcessStdio::Capture(super::STDERR_CAPTURE)
        );

        let signaled = super::native_exit_status(ProcessExit {
            success: false,
            code: None,
            signal: Some(15),
        });
        assert_eq!(signaled.code(), 143);
        assert_eq!(signaled.kind(), ShellStatusKind::Interrupted);
        assert_eq!(signaled.signal(), Some(15));
    }

    #[tokio::test]
    async fn native_redirections_apply_in_source_order_without_buffering_files() {
        let directory = TestDirectory::new();
        let executable = compile_pipeline_helper(&directory);
        let mut state = ShellState::from_process().expect("process shell state");
        state.set_cwd(&directory.0);
        state
            .environment_mut()
            .insert("PATH", directory.0.join("bin").into_os_string());

        fs::write(directory.0.join("first.log"), b"stale").expect("seed first target");
        let merged_source = format!("{executable} ordered >first.log >merged.log 2>&1");
        let merged = execute_source(&merged_source, &mut state).await;
        assert_eq!(merged.status().code(), 0);
        assert!(merged.stdout().is_empty());
        assert!(merged.stderr().is_empty());
        assert_eq!(fs::read(directory.0.join("first.log")).expect("first"), b"");
        assert_eq!(
            fs::read(directory.0.join("merged.log")).expect("merged"),
            b"stdout-a\nstderr-a\nstdout-b\nstderr-b\n"
        );

        let ordered_source = format!("{executable} ordered 2>&1 >stdout.log");
        let ordered = execute_source(&ordered_source, &mut state).await;
        assert_eq!(ordered.status().code(), 0);
        assert_eq!(ordered.stdout(), b"stderr-a\nstderr-b\n");
        assert!(ordered.stderr().is_empty());
        assert_eq!(
            fs::read(directory.0.join("stdout.log")).expect("stdout target"),
            b"stdout-a\nstdout-b\n"
        );

        let stderr_source = format!("{executable} ordered 1>&2");
        let stderr_merged = execute_source(&stderr_source, &mut state).await;
        assert_eq!(stderr_merged.status().code(), 0);
        assert!(stderr_merged.stdout().is_empty());
        assert_eq!(
            stderr_merged.stderr(),
            b"stdout-a\nstderr-a\nstdout-b\nstderr-b\n"
        );

        let append_source = format!(
            "{executable} ordered >append.out 2>append.err; {executable} ordered >>append.out 2>>append.err"
        );
        let appended = execute_source(&append_source, &mut state).await;
        assert_eq!(appended.status().code(), 0);
        assert!(appended.stdout().is_empty());
        assert!(appended.stderr().is_empty());
        assert_eq!(
            fs::read(directory.0.join("append.out")).expect("appended stdout"),
            b"stdout-a\nstdout-b\nstdout-a\nstdout-b\n"
        );
        assert_eq!(
            fs::read(directory.0.join("append.err")).expect("appended stderr"),
            b"stderr-a\nstderr-b\nstderr-a\nstderr-b\n"
        );

        fs::write(directory.0.join("input.bin"), b"pipeline-input").expect("pipeline input");
        let pipeline_source = format!(
            "{executable} copy first <input.bin 2>first.err | {executable} copy final >pipeline.bin 2>final.err"
        );
        let pipeline = execute_source(&pipeline_source, &mut state).await;
        assert_eq!(pipeline.status().code(), 0);
        assert!(pipeline.stdout().is_empty());
        assert!(pipeline.stderr().is_empty());
        assert_eq!(
            fs::read(directory.0.join("pipeline.bin")).expect("pipeline output"),
            b"pipeline-input"
        );
        assert_eq!(
            fs::read(directory.0.join("first.err")).expect("first stderr"),
            b"first-stderr\n"
        );
        assert_eq!(
            fs::read(directory.0.join("final.err")).expect("final stderr"),
            b"final-stderr\n"
        );

        let merged_pipeline_source = format!(
            "{executable} ordered 2>&1 | {executable} copy merged >merged-pipeline.log 2>merged-final.err"
        );
        let merged_pipeline = execute_source(&merged_pipeline_source, &mut state).await;
        assert_eq!(merged_pipeline.status().code(), 0);
        assert!(merged_pipeline.stdout().is_empty());
        assert!(merged_pipeline.stderr().is_empty());
        assert_eq!(
            fs::read(directory.0.join("merged-pipeline.log")).expect("merged pipeline"),
            b"stdout-a\nstderr-a\nstdout-b\nstderr-b\n"
        );
        assert_eq!(
            fs::read(directory.0.join("merged-final.err")).expect("merged final stderr"),
            b"merged-stderr\n"
        );

        let producer_redirect_source = format!(
            "{executable} produce 1 >producer-only.bin | {executable} copy eof >eof.bin 2>eof.err"
        );
        let producer_redirect = execute_source(&producer_redirect_source, &mut state).await;
        assert_eq!(producer_redirect.status().code(), 0);
        assert_eq!(producer_redirect.stderr(), b"producer-stderr\n");
        assert!(producer_redirect.stdout().is_empty());
        assert!(producer_redirect.diagnostics().is_empty());
        assert_eq!(
            fs::read(directory.0.join("producer-only.bin")).expect("producer output"),
            [0]
        );
        assert!(
            fs::read(directory.0.join("eof.bin"))
                .expect("EOF output")
                .is_empty()
        );
        assert_eq!(
            fs::read(directory.0.join("eof.err")).expect("EOF stderr"),
            b"eof-stderr\n"
        );

        let descriptor_source = format!(
            "{executable} ordered 2>&1 >descriptor-stdout.log | {executable} copy descriptor >descriptor-pipe.log 2>descriptor-final.err"
        );
        let descriptor = execute_source(&descriptor_source, &mut state).await;
        assert_eq!(descriptor.status().code(), 0);
        assert!(descriptor.stdout().is_empty());
        assert!(descriptor.stderr().is_empty());
        assert!(descriptor.diagnostics().is_empty());
        assert_eq!(
            fs::read(directory.0.join("descriptor-stdout.log")).expect("descriptor stdout"),
            b"stdout-a\nstdout-b\n"
        );
        assert_eq!(
            fs::read(directory.0.join("descriptor-pipe.log")).expect("descriptor pipe"),
            b"stderr-a\nstderr-b\n"
        );
        assert_eq!(
            fs::read(directory.0.join("descriptor-final.err")).expect("descriptor final stderr"),
            b"descriptor-stderr\n"
        );

        let both_source = format!(
            "{executable} produce 1 >both-producer.bin 2>both-producer.err | {executable} copy both <input.bin >both-consumer.bin 2>both-consumer.err"
        );
        let both = execute_source(&both_source, &mut state).await;
        assert_eq!(both.status().code(), 0);
        assert!(both.stdout().is_empty());
        assert!(both.stderr().is_empty());
        assert!(both.diagnostics().is_empty());
        assert_eq!(
            fs::read(directory.0.join("both-producer.bin")).expect("both producer output"),
            [0]
        );
        assert_eq!(
            fs::read(directory.0.join("both-producer.err")).expect("both producer stderr"),
            b"producer-stderr\n"
        );
        assert_eq!(
            fs::read(directory.0.join("both-consumer.bin")).expect("both consumer output"),
            b"pipeline-input"
        );
        assert_eq!(
            fs::read(directory.0.join("both-consumer.err")).expect("both consumer stderr"),
            b"both-stderr\n"
        );

        state.options_mut().set_pipefail(true);
        let reader_redirect_source = format!(
            "{executable} produce {} 2>broken-producer.err | {executable} copy redirected <input.bin >reader-redirect.bin 2>reader-redirect.err",
            8 * 1024 * 1024
        );
        let reader_redirect = execute_source(&reader_redirect_source, &mut state).await;
        state.options_mut().set_pipefail(false);
        assert_ne!(reader_redirect.status().code(), 0);
        assert!(reader_redirect.stdout().is_empty());
        assert!(reader_redirect.stderr().is_empty());
        assert!(reader_redirect.diagnostics().is_empty());
        assert_eq!(
            fs::read(directory.0.join("reader-redirect.bin")).expect("redirected reader output"),
            b"pipeline-input"
        );
        assert_eq!(
            fs::read(directory.0.join("reader-redirect.err")).expect("redirected reader stderr"),
            b"redirected-stderr\n"
        );
        assert!(
            !fs::read(directory.0.join("broken-producer.err"))
                .expect("broken producer stderr")
                .is_empty()
        );

        state
            .set_variable("REDIRECT", "one two")
            .expect("redirection variable");
        let ambiguous_source = format!("{executable} ordered >$REDIRECT");
        let ambiguous = execute_source(&ambiguous_source, &mut state).await;
        assert_eq!(ambiguous.status().code(), 1);
        assert_eq!(ambiguous.status().kind(), ShellStatusKind::RedirectionError);
        assert_eq!(ambiguous.diagnostics().len(), 1);
        assert_eq!(
            ambiguous.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Redirection
        );
        assert_eq!(
            ambiguous.diagnostics()[0].message(),
            "redirection target expands to multiple paths"
        );

        let missing_source = format!("{executable} copy missing <missing.bin");
        let missing = execute_source(&missing_source, &mut state).await;
        assert_eq!(missing.status().code(), 1);
        assert_eq!(missing.status().kind(), ShellStatusKind::RedirectionError);
        assert_eq!(missing.diagnostics().len(), 1);
        assert_eq!(
            missing.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Redirection
        );
        assert!(missing.diagnostics()[0].message().contains("missing.bin"));
    }

    #[tokio::test]
    async fn stateful_redirections_open_before_parent_mutation_and_keep_shell_diagnostics() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child directory");
        fs::write(directory.0.join("first.log"), b"stale").expect("seed first target");
        fs::write(directory.0.join("append.log"), b"preserved").expect("seed append target");
        fs::write(directory.0.join("stateful-input"), b"ignored").expect("seed stateful input");
        let mut state = ShellState::new(&directory.0);

        let sequential = execute_source(
            "export STATEFUL=value <stateful-input >first.log >export.log; \
             set -o pipefail >>append.log; \
             cd child >before-cd.log; pwd",
            &mut state,
        )
        .await;

        let child = fs::canonicalize(directory.0.join("child")).expect("canonical child");
        assert_eq!(
            sequential.stdout(),
            format!("{}\n", child.display()).as_bytes()
        );
        assert!(sequential.stderr().is_empty());
        assert!(sequential.diagnostics().is_empty());
        assert_eq!(state.cwd(), child);
        assert_eq!(
            state.environment().get("STATEFUL"),
            Some(OsStr::new("value"))
        );
        assert!(state.options().pipefail());
        assert!(
            fs::read(directory.0.join("first.log"))
                .expect("superseded stateful target")
                .is_empty()
        );
        assert!(
            fs::read(directory.0.join("export.log"))
                .expect("final stateful target")
                .is_empty()
        );
        assert_eq!(
            fs::read(directory.0.join("append.log")).expect("stateful append target"),
            b"preserved"
        );
        assert!(
            fs::read(directory.0.join("before-cd.log"))
                .expect("pre-cd target")
                .is_empty(),
            "the redirection path must resolve before cd changes cwd"
        );

        let failed_cd = execute_source("cd missing 2>diagnostic.log", &mut state).await;
        assert_eq!(failed_cd.status().code(), 1);
        assert_eq!(failed_cd.diagnostics().len(), 1);
        assert_eq!(
            failed_cd.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(
            failed_cd.stderr(),
            failed_cd.diagnostics()[0].render().as_bytes()
        );
        assert!(
            fs::read(child.join("diagnostic.log"))
                .expect("stateful diagnostic target")
                .is_empty(),
            "source-spanned shell diagnostics are not raw command stderr"
        );
        assert_eq!(state.cwd(), child);

        let invalid = execute_source("export INVALID >invalid.log", &mut state).await;
        assert_eq!(invalid.status().code(), 2);
        assert_eq!(
            invalid.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert!(
            !child.join("invalid.log").exists(),
            "invalid stateful arguments must fail before file-open side effects"
        );

        let failed_open =
            execute_source("export BLOCKED=value >missing/output.log", &mut state).await;
        assert_eq!(
            failed_open.status().kind(),
            ShellStatusKind::RedirectionError
        );
        assert_eq!(
            failed_open.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Redirection
        );
        assert!(state.environment().get("BLOCKED").is_none());

        let exited = execute_source("exit 7 >exit.log; echo unreachable", &mut state).await;
        assert_eq!(exited.status().code(), 7);
        assert_eq!(exited.exit_requested(), Some(7));
        assert!(exited.stdout().is_empty());
        assert!(exited.diagnostics().is_empty());
        assert!(
            fs::read(child.join("exit.log"))
                .expect("stateful exit target")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn portable_redirections_stream_files_descriptors_and_replaced_pipeline_ends() {
        let directory = TestDirectory::new();
        let input = b"alpha\r\nneedle one\nneedle two\r\nomega\n";
        fs::write(directory.0.join("input.txt"), input).expect("portable redirect input");
        let mut state = ShellState::new(&directory.0);

        let redirected = execute_source(
            "echo first >output.log; echo second >>output.log; \
             cat - <input.txt >copy.txt; grep -n needle - <input.txt >matches.txt; \
             echo routed 1>&2; echo file 2>stderr.log 1>&2; \
             echo retained 1>&2 2>late-stderr.log",
            &mut state,
        )
        .await;
        assert_eq!(redirected.status().code(), 0);
        assert!(redirected.stdout().is_empty());
        assert_eq!(redirected.stderr(), b"routed\nretained\n");
        assert!(redirected.diagnostics().is_empty());
        assert_eq!(
            fs::read(directory.0.join("output.log")).expect("portable append output"),
            b"first\nsecond\n"
        );
        assert_eq!(
            fs::read(directory.0.join("copy.txt")).expect("portable copied input"),
            input
        );
        assert_eq!(
            fs::read(directory.0.join("matches.txt")).expect("portable grep output"),
            b"2:needle one\n3:needle two\n"
        );
        assert_eq!(
            fs::read(directory.0.join("stderr.log")).expect("portable descriptor output"),
            b"file\n"
        );
        assert!(
            fs::read(directory.0.join("late-stderr.log"))
                .expect("superseding portable stderr file")
                .is_empty()
        );

        let invalid_stdin = execute_source("cat - >must-not-open.log", &mut state).await;
        assert_eq!(invalid_stdin.status().code(), 2);
        assert_eq!(
            invalid_stdin.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert!(!directory.0.join("must-not-open.log").exists());

        state.options_mut().set_pipefail(true);
        let replaced_reader = execute_source(
            "echo upstream | cat - <input.txt >reader-copy.txt",
            &mut state,
        )
        .await;
        state.options_mut().set_pipefail(false);
        assert_ne!(replaced_reader.status().code(), 0);
        assert!(replaced_reader.stdout().is_empty());
        assert!(replaced_reader.stderr().is_empty());
        assert!(replaced_reader.diagnostics().is_empty());
        assert_eq!(
            fs::read(directory.0.join("reader-copy.txt")).expect("redirected portable reader"),
            input
        );

        let replaced_writer = execute_source("echo side >side.log | cat -", &mut state).await;
        assert_eq!(replaced_writer.status().code(), 0);
        assert!(replaced_writer.stdout().is_empty());
        assert!(replaced_writer.stderr().is_empty());
        assert_eq!(
            fs::read(directory.0.join("side.log")).expect("redirected portable writer"),
            b"side\n"
        );

        let descriptor_pipeline = execute_source("echo diagnostic 1>&2 | cat -", &mut state).await;
        assert_eq!(descriptor_pipeline.status().code(), 0);
        assert!(descriptor_pipeline.stdout().is_empty());
        assert_eq!(descriptor_pipeline.stderr(), b"diagnostic\n");
        assert!(descriptor_pipeline.diagnostics().is_empty());
    }

    #[tokio::test]
    async fn mixed_pipeline_files_open_in_global_stage_and_source_order() {
        let directory = TestDirectory::new();
        let mut state = ShellState::new(&directory.0);
        let lookup =
            |command: &str, _cwd: &std::path::Path, _environment: &crate::PlatformEnvironment| {
                (command == "consumer").then(|| directory.0.join("unreachable-consumer"))
            };

        let parent_first = execute_source_with(
            "echo value >opened-first.log | native:consumer <missing-input",
            &mut state,
            &lookup,
            HostPlatform::Linux,
        )
        .await;
        assert_eq!(
            parent_first.status().kind(),
            ShellStatusKind::RedirectionError
        );
        assert_eq!(
            parent_first.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Redirection
        );
        assert!(
            directory.0.join("opened-first.log").exists(),
            "the earlier portable file must open before the later native failure"
        );

        let native_first = execute_source_with(
            "native:consumer <missing-input | echo value >must-stay-unopened.log",
            &mut state,
            &lookup,
            HostPlatform::Linux,
        )
        .await;
        assert_eq!(
            native_first.status().kind(),
            ShellStatusKind::RedirectionError
        );
        assert!(
            !directory.0.join("must-stay-unopened.log").exists(),
            "the later portable file must not open after an earlier native failure"
        );

        let stateful_first = execute_source_with(
            "export GLOBAL_ORDER=value >stateful-opened-first.log | native:consumer <missing-input",
            &mut state,
            &lookup,
            HostPlatform::Linux,
        )
        .await;
        assert_eq!(
            stateful_first.status().kind(),
            ShellStatusKind::RedirectionError
        );
        assert!(directory.0.join("stateful-opened-first.log").exists());
        assert!(
            state.environment().get("GLOBAL_ORDER").is_none(),
            "pipeline stateful stages must not mutate parent state"
        );

        let native_before_stateful = execute_source_with(
            "native:consumer <missing-input | export GLOBAL_ORDER=value >stateful-must-stay-unopened.log",
            &mut state,
            &lookup,
            HostPlatform::Linux,
        )
        .await;
        assert_eq!(
            native_before_stateful.status().kind(),
            ShellStatusKind::RedirectionError
        );
        assert!(
            !directory.0.join("stateful-must-stay-unopened.log").exists(),
            "the later stateful file must not open after an earlier native failure"
        );
    }

    #[tokio::test]
    async fn native_pipelines_preflight_every_stage_and_apply_the_status_policy() {
        let mut state = ShellState::new("/fixture/work");
        state.environment_mut().insert("PATH", "/fixture/bin");
        let lookup =
            |command: &str, _cwd: &std::path::Path, _environment: &crate::PlatformEnvironment| {
                match command {
                    "first" | "second" | "third" => {
                        Some(PathBuf::from(format!("/fixture/bin/{command}")))
                    }
                    _ => None,
                }
            };
        let runner = RecordingPipelineRunner::new(PipelineOutput {
            stdout: b"pipeline-stdout\n".to_vec(),
            stderr: b"first-stderr\nsecond-stderr\n".to_vec(),
            exits: vec![
                ProcessExit {
                    success: false,
                    code: Some(9),
                    signal: None,
                },
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
            ],
            diagnostics: Vec::new(),
        });

        let execution = execute_source_with_runner(
            "native:first alpha | native:second 'beta gamma'; echo $?",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &runner,
        )
        .await;

        assert_eq!(execution.stdout(), b"pipeline-stdout\n0\n");
        assert_eq!(execution.stderr(), b"first-stderr\nsecond-stderr\n");
        assert!(execution.diagnostics().is_empty());
        assert_eq!(execution.status().code(), 0);
        let invocation = runner
            .invocation
            .lock()
            .expect("invocation lock")
            .clone()
            .expect("pipeline invocation");
        let [
            PipelineStageInvocation::Native(first),
            PipelineStageInvocation::Native(second),
        ] = invocation.stages.as_slice()
        else {
            panic!("expected two native stages");
        };
        assert_eq!(first.executable, PathBuf::from("/fixture/bin/first"));
        assert_eq!(first.arguments, [OsString::from("alpha")]);
        assert_eq!(second.executable, PathBuf::from("/fixture/bin/second"));
        assert_eq!(second.arguments, [OsString::from("beta gamma")]);
        assert_eq!(invocation.capture_limit, super::MAX_READ_FILE_BYTES);
        assert!(invocation.closed_pipe_ends.is_empty());
        assert!(invocation.parent_pipe_ends.is_empty());
        assert!(invocation.parent_captures.is_empty());
        assert_eq!(first.capture_limit, invocation.capture_limit);
        assert_eq!(second.capture_limit, invocation.capture_limit);

        let mixed_runner = RecordingPipelineRunner::new(successful_pipeline_output(2));
        let mixed = execute_source_with_runner(
            "echo hello | native:second",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &mixed_runner,
        )
        .await;
        assert_eq!(mixed.status().code(), 0);
        assert!(mixed.diagnostics().is_empty());
        let mixed_invocation = mixed_runner
            .invocation
            .lock()
            .expect("mixed invocation lock")
            .clone()
            .expect("mixed pipeline invocation");
        let [
            PipelineStageInvocation::Portable(portable),
            PipelineStageInvocation::Native(native),
        ] = mixed_invocation.stages.as_slice()
        else {
            panic!("expected portable-to-native stages");
        };
        assert_eq!(portable.command, crate::PortableCommand::Echo);
        assert_eq!(portable.arguments, [OsString::from("hello")]);
        assert_eq!(portable.stdin, ParentTaskStdio::Null);
        assert_eq!(
            portable.stdout,
            ParentTaskStdio::Pipe(ProcessPipeId::new(0))
        );
        assert!(portable.files.is_empty());
        assert_eq!(native.stdin, ProcessStdio::Pipe(ProcessPipeId::new(0)));
        assert_eq!(
            mixed_invocation.parent_pipe_ends,
            [ParentProcessPipeEnd::Writer(ProcessPipeId::new(0))]
        );
        assert!(mixed_invocation.closed_pipe_ends.is_empty());
        assert!(mixed_invocation.parent_captures.is_empty());

        let final_portable_runner = RecordingPipelineRunner::new(successful_pipeline_output(2));
        let final_portable = execute_source_with_runner(
            "native:first | pwd",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &final_portable_runner,
        )
        .await;
        assert_eq!(final_portable.status().code(), 0);
        assert!(final_portable.diagnostics().is_empty());
        let final_portable_invocation = final_portable_runner
            .invocation
            .lock()
            .expect("final portable invocation lock")
            .clone()
            .expect("final portable pipeline invocation");
        let [
            PipelineStageInvocation::Native(native),
            PipelineStageInvocation::Portable(portable),
        ] = final_portable_invocation.stages.as_slice()
        else {
            panic!("expected native-to-portable stages");
        };
        assert_eq!(native.stdout, ProcessStdio::Pipe(ProcessPipeId::new(0)));
        assert_eq!(portable.command, crate::PortableCommand::Pwd);
        assert_eq!(portable.stdin, ParentTaskStdio::Null);
        assert_eq!(
            portable.stdout,
            ParentTaskStdio::Pipe(ProcessPipeId::new(1))
        );
        assert_eq!(
            final_portable_invocation.closed_pipe_ends,
            [ClosedProcessPipeEnd::Reader(ProcessPipeId::new(0))]
        );
        assert_eq!(
            final_portable_invocation.parent_pipe_ends,
            [
                ParentProcessPipeEnd::Reader(ProcessPipeId::new(1)),
                ParentProcessPipeEnd::Writer(ProcessPipeId::new(1)),
            ]
        );
        assert_eq!(
            final_portable_invocation.parent_captures,
            [ParentPipelineCapture {
                stage_index: 1,
                pipe: ProcessPipeId::new(1),
                stream: PipelineCaptureStream::Stdout,
            }]
        );

        let portable_runner = RecordingPipelineRunner::new(successful_pipeline_output(2));
        let portable_only = execute_source_with_runner(
            "echo alpha | grep -Fn alpha -",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &portable_runner,
        )
        .await;
        assert_eq!(portable_only.status().code(), 0);
        assert!(portable_only.diagnostics().is_empty());
        let portable_invocation = portable_runner
            .invocation
            .lock()
            .expect("portable invocation lock")
            .clone()
            .expect("portable pipeline invocation");
        let [
            PipelineStageInvocation::Portable(echo),
            PipelineStageInvocation::Portable(grep),
        ] = portable_invocation.stages.as_slice()
        else {
            panic!("expected two portable stages");
        };
        assert_eq!(echo.command, crate::PortableCommand::Echo);
        assert_eq!(echo.stdin, ParentTaskStdio::Null);
        assert_eq!(echo.stdout, ParentTaskStdio::Pipe(ProcessPipeId::new(0)));
        assert_eq!(grep.command, crate::PortableCommand::Grep);
        assert_eq!(
            grep.arguments,
            [
                OsString::from("-Fn"),
                OsString::from("alpha"),
                OsString::from("-"),
            ]
        );
        assert_eq!(grep.stdin, ParentTaskStdio::Pipe(ProcessPipeId::new(0)));
        assert_eq!(grep.stdout, ParentTaskStdio::Pipe(ProcessPipeId::new(1)));
        assert!(portable_invocation.closed_pipe_ends.is_empty());
        assert_eq!(
            portable_invocation.parent_pipe_ends,
            [
                ParentProcessPipeEnd::Reader(ProcessPipeId::new(1)),
                ParentProcessPipeEnd::Writer(ProcessPipeId::new(1)),
                ParentProcessPipeEnd::Writer(ProcessPipeId::new(0)),
                ParentProcessPipeEnd::Reader(ProcessPipeId::new(0)),
            ]
        );
        assert_eq!(
            portable_invocation.parent_captures,
            [ParentPipelineCapture {
                stage_index: 1,
                pipe: ProcessPipeId::new(1),
                stream: PipelineCaptureStream::Stdout,
            }]
        );

        let redirected_runner = RecordingPipelineRunner::new(PipelineOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exits: vec![
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
            ],
            diagnostics: Vec::new(),
        });
        let redirected = execute_source_with_runner(
            "native:first >producer.log | native:second <consumer.log",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &redirected_runner,
        )
        .await;
        assert_eq!(redirected.status().code(), 0);
        assert!(redirected.diagnostics().is_empty());
        let redirected_invocation = redirected_runner
            .invocation
            .lock()
            .expect("redirected invocation lock")
            .clone()
            .expect("redirected pipeline invocation");
        assert_eq!(
            redirected_invocation.closed_pipe_ends,
            [
                ClosedProcessPipeEnd::Writer(ProcessPipeId::new(0)),
                ClosedProcessPipeEnd::Reader(ProcessPipeId::new(0)),
            ]
        );

        let duplicated_runner = RecordingPipelineRunner::new(PipelineOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exits: vec![
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
            ],
            diagnostics: Vec::new(),
        });
        let duplicated = execute_source_with_runner(
            "native:first 2>&1 >producer.log | native:second",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &duplicated_runner,
        )
        .await;
        assert_eq!(duplicated.status().code(), 0);
        assert!(duplicated.diagnostics().is_empty());
        let duplicated_invocation = duplicated_runner
            .invocation
            .lock()
            .expect("duplicated invocation lock")
            .clone()
            .expect("duplicated pipeline invocation");
        assert!(duplicated_invocation.closed_pipe_ends.is_empty());

        let redirected_consumer_runner =
            RecordingPipelineRunner::new(successful_pipeline_output(2));
        let redirected_consumer = execute_source_with_runner(
            "echo hi | native:second <consumer.log",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &redirected_consumer_runner,
        )
        .await;
        assert_eq!(redirected_consumer.status().code(), 0);
        assert!(redirected_consumer.diagnostics().is_empty());
        let redirected_consumer_invocation = redirected_consumer_runner
            .invocation
            .lock()
            .expect("redirected consumer invocation lock")
            .clone()
            .expect("redirected consumer pipeline invocation");
        assert_eq!(
            redirected_consumer_invocation.closed_pipe_ends,
            [ClosedProcessPipeEnd::Reader(ProcessPipeId::new(0))]
        );
        assert_eq!(
            redirected_consumer_invocation.parent_pipe_ends,
            [ParentProcessPipeEnd::Writer(ProcessPipeId::new(0))]
        );

        let redirected_producer_runner =
            RecordingPipelineRunner::new(successful_pipeline_output(2));
        let redirected_producer = execute_source_with_runner(
            "native:first >producer.log | cat -",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &redirected_producer_runner,
        )
        .await;
        assert_eq!(redirected_producer.status().code(), 0);
        assert!(redirected_producer.diagnostics().is_empty());
        let redirected_producer_invocation = redirected_producer_runner
            .invocation
            .lock()
            .expect("redirected producer invocation lock")
            .clone()
            .expect("redirected producer pipeline invocation");
        assert_eq!(
            redirected_producer_invocation.closed_pipe_ends,
            [ClosedProcessPipeEnd::Writer(ProcessPipeId::new(0))]
        );
        assert_eq!(
            redirected_producer_invocation.parent_pipe_ends,
            [
                ParentProcessPipeEnd::Reader(ProcessPipeId::new(1)),
                ParentProcessPipeEnd::Writer(ProcessPipeId::new(1)),
                ParentProcessPipeEnd::Reader(ProcessPipeId::new(0)),
            ]
        );

        state.options_mut().set_pipefail(true);
        let successful_runner = RecordingPipelineRunner::new(PipelineOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exits: vec![
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
            ],
            diagnostics: Vec::new(),
        });
        let successful = execute_source_with_runner(
            "native:first | native:second",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &successful_runner,
        )
        .await;
        assert_eq!(successful.status().code(), 0);
        assert!(successful.diagnostics().is_empty());

        let pipefail_runner = RecordingPipelineRunner::new(PipelineOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exits: vec![
                ProcessExit {
                    success: false,
                    code: Some(9),
                    signal: None,
                },
                ProcessExit {
                    success: false,
                    code: Some(7),
                    signal: None,
                },
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
            ],
            diagnostics: Vec::new(),
        });
        let pipefail = execute_source_with_runner(
            "native:first | native:second | native:third; echo $?",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &pipefail_runner,
        )
        .await;
        assert_eq!(pipefail.stdout(), b"7\n");
        assert!(pipefail.stderr().is_empty());
        assert!(pipefail.diagnostics().is_empty());

        let signaled_runner = RecordingPipelineRunner::new(PipelineOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exits: vec![
                ProcessExit {
                    success: false,
                    code: Some(9),
                    signal: None,
                },
                ProcessExit {
                    success: false,
                    code: None,
                    signal: Some(15),
                },
                ProcessExit {
                    success: true,
                    code: Some(0),
                    signal: None,
                },
            ],
            diagnostics: Vec::new(),
        });
        let signaled = execute_source_with_runner(
            "native:first | native:second | native:third",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &signaled_runner,
        )
        .await;
        assert_eq!(signaled.status().code(), 143);
        assert_eq!(signaled.status().kind(), ShellStatusKind::Interrupted);
        assert_eq!(signaled.status().signal(), Some(15));

        let stateful_runner = RecordingPipelineRunner::new(successful_pipeline_output(2));
        let stateful = execute_source_with_runner(
            "cd child | native:second",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &stateful_runner,
        )
        .await;
        assert_eq!(stateful.status().code(), 0);
        assert!(stateful.diagnostics().is_empty());
        let stateful_invocation = stateful_runner
            .invocation
            .lock()
            .expect("stateful invocation lock")
            .clone()
            .expect("stateful pipeline invocation");
        let [
            PipelineStageInvocation::Stateful(cd),
            PipelineStageInvocation::Native(native),
        ] = stateful_invocation.stages.as_slice()
        else {
            panic!("expected stateful-to-native stages");
        };
        assert_eq!(cd.command, crate::StatefulBuiltin::Cd);
        assert_eq!(cd.arguments, [OsString::from("child")]);
        assert_eq!(cd.state.cwd(), state.cwd());
        assert_eq!(cd.stdout, ParentTaskStdio::Pipe(ProcessPipeId::new(0)));
        assert!(cd.files.is_empty());
        assert_eq!(native.stdin, ProcessStdio::Pipe(ProcessPipeId::new(0)));
        assert_eq!(
            stateful_invocation.parent_pipe_ends,
            [ParentProcessPipeEnd::Writer(ProcessPipeId::new(0))]
        );
        assert!(stateful_invocation.closed_pipe_ends.is_empty());
        assert!(stateful_invocation.parent_captures.is_empty());
        assert_eq!(state.cwd(), std::path::Path::new("/fixture/work"));

        let rejected_runner = RecordingPipelineRunner::new(PipelineOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exits: vec![],
            diagnostics: Vec::new(),
        });
        let unsupported = execute_source_with_runner(
            "alias value | native:second",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &rejected_runner,
        )
        .await;
        assert_eq!(unsupported.status().code(), 2);
        assert_eq!(unsupported.diagnostics().len(), 1);
        assert_eq!(
            unsupported.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Unsupported
        );
        assert_eq!(unsupported.diagnostics()[0].span(), SourceSpan::new(0, 11));
        assert!(
            rejected_runner
                .invocation
                .lock()
                .expect("invocation lock")
                .is_none(),
            "unsupported stages must fail before spawn"
        );

        let preflight_directory = TestDirectory::new();
        let mut preflight_state = ShellState::new(&preflight_directory.0);
        preflight_state
            .environment_mut()
            .insert("PATH", "/fixture/bin");
        let invalid_arguments_runner = RecordingPipelineRunner::new(successful_pipeline_output(2));
        let invalid_arguments = execute_source_with_runner(
            "native:first >should-not-exist | grep -E '[' -",
            &mut preflight_state,
            &lookup,
            HostPlatform::Linux,
            &invalid_arguments_runner,
        )
        .await;
        assert_eq!(invalid_arguments.status().code(), 2);
        assert_eq!(invalid_arguments.diagnostics().len(), 1);
        assert_eq!(
            invalid_arguments.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert!(
            invalid_arguments.diagnostics()[0]
                .message()
                .contains("regular expression is invalid")
        );
        assert!(
            invalid_arguments_runner
                .invocation
                .lock()
                .expect("invalid arguments invocation lock")
                .is_none(),
            "invalid portable arguments must fail before spawn"
        );
        assert!(!preflight_directory.0.join("should-not-exist").exists());

        let invalid_stateful_runner = RecordingPipelineRunner::new(successful_pipeline_output(2));
        let invalid_stateful = execute_source_with_runner(
            "native:first >stateful-should-not-exist | export INVALID",
            &mut preflight_state,
            &lookup,
            HostPlatform::Linux,
            &invalid_stateful_runner,
        )
        .await;
        assert_eq!(invalid_stateful.status().code(), 2);
        assert_eq!(invalid_stateful.diagnostics().len(), 1);
        assert_eq!(
            invalid_stateful.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(
            invalid_stateful.diagnostics()[0].message(),
            "export requires a NAME=VALUE assignment"
        );
        assert!(
            invalid_stateful_runner
                .invocation
                .lock()
                .expect("invalid stateful invocation lock")
                .is_none(),
            "invalid stateful arguments must fail before spawn"
        );
        assert!(
            !preflight_directory
                .0
                .join("stateful-should-not-exist")
                .exists()
        );

        let portable_redirection_runner =
            RecordingPipelineRunner::new(successful_pipeline_output(2));
        let portable_redirection = execute_source_with_runner(
            "echo hi >portable.log | native:second",
            &mut preflight_state,
            &lookup,
            HostPlatform::Linux,
            &portable_redirection_runner,
        )
        .await;
        assert_eq!(portable_redirection.status().code(), 0);
        assert!(portable_redirection.diagnostics().is_empty());
        let portable_redirection_invocation = portable_redirection_runner
            .invocation
            .lock()
            .expect("portable redirection invocation lock")
            .clone()
            .expect("portable redirection invocation");
        let [
            PipelineStageInvocation::Portable(portable),
            PipelineStageInvocation::Native(native),
        ] = portable_redirection_invocation.stages.as_slice()
        else {
            panic!("expected redirected portable-to-native stages");
        };
        let parent_file = ParentProcessFileId::new(0);
        assert_eq!(portable.stdout, ParentTaskStdio::File(parent_file));
        assert_eq!(portable.stdin, ParentTaskStdio::Null);
        assert_eq!(portable.files.len(), 1);
        assert_eq!(portable.files[0].id, parent_file);
        assert_eq!(portable.files[0].mode, NativeProcessFileMode::Write);
        assert_eq!(
            portable.files[0].path,
            preflight_directory.0.join("portable.log")
        );
        assert_eq!(native.stdin, ProcessStdio::Pipe(ProcessPipeId::new(0)));
        assert_eq!(
            portable_redirection_invocation.closed_pipe_ends,
            [ClosedProcessPipeEnd::Writer(ProcessPipeId::new(0))]
        );
        assert!(portable_redirection_invocation.parent_pipe_ends.is_empty());
        assert!(portable_redirection_invocation.parent_captures.is_empty());
        assert!(!preflight_directory.0.join("portable.log").exists());

        let stateful_redirection_runner =
            RecordingPipelineRunner::new(successful_pipeline_output(2));
        let stateful_redirection = execute_source_with_runner(
            "export CHILD=value >stateful-first.log 2>stateful.log 1>&2 | native:second",
            &mut preflight_state,
            &lookup,
            HostPlatform::Linux,
            &stateful_redirection_runner,
        )
        .await;
        assert_eq!(stateful_redirection.status().code(), 0);
        assert!(stateful_redirection.diagnostics().is_empty());
        let stateful_redirection_invocation = stateful_redirection_runner
            .invocation
            .lock()
            .expect("stateful redirection invocation lock")
            .clone()
            .expect("stateful redirection invocation");
        let [
            PipelineStageInvocation::Stateful(stateful),
            PipelineStageInvocation::Native(native),
        ] = stateful_redirection_invocation.stages.as_slice()
        else {
            panic!("expected redirected stateful-to-native stages");
        };
        let first_parent_file = ParentProcessFileId::new(0);
        let final_parent_file = ParentProcessFileId::new(1);
        assert_eq!(stateful.command, crate::StatefulBuiltin::Export);
        assert_eq!(stateful.arguments, [OsString::from("CHILD=value")]);
        assert_eq!(stateful.stdout, ParentTaskStdio::File(final_parent_file));
        assert_eq!(stateful.files.len(), 2);
        assert_eq!(stateful.files[0].id, first_parent_file);
        assert_eq!(stateful.files[1].id, final_parent_file);
        assert_eq!(stateful.files[0].mode, NativeProcessFileMode::Write);
        assert_eq!(stateful.files[1].mode, NativeProcessFileMode::Write);
        assert_eq!(
            stateful.files[0].path,
            preflight_directory.0.join("stateful-first.log")
        );
        assert_eq!(
            stateful.files[1].path,
            preflight_directory.0.join("stateful.log")
        );
        assert_eq!(native.stdin, ProcessStdio::Pipe(ProcessPipeId::new(0)));
        assert_eq!(
            stateful_redirection_invocation.closed_pipe_ends,
            [ClosedProcessPipeEnd::Writer(ProcessPipeId::new(0))]
        );
        assert!(stateful_redirection_invocation.parent_pipe_ends.is_empty());
        assert!(stateful_redirection_invocation.parent_captures.is_empty());
        assert!(preflight_state.environment().get("CHILD").is_none());
        assert!(!preflight_directory.0.join("stateful-first.log").exists());
        assert!(!preflight_directory.0.join("stateful.log").exists());

        let too_many = std::iter::repeat_n("native:first", super::MAX_NATIVE_PIPELINE_STAGES + 1)
            .collect::<Vec<_>>()
            .join(" | ");
        let oversized = execute_source_with_runner(
            &too_many,
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &rejected_runner,
        )
        .await;
        assert_eq!(oversized.status().code(), 2);
        assert_eq!(oversized.diagnostics().len(), 1);
        assert_eq!(
            oversized.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert!(
            rejected_runner
                .invocation
                .lock()
                .expect("invocation lock")
                .is_none(),
            "oversized pipelines must fail before resolution or spawn"
        );
    }

    #[tokio::test]
    async fn set_toggles_only_the_documented_pipefail_option_in_parent_state() {
        let mut state = ShellState::new(".");
        assert!(!state.options().pipefail());

        let enabled = execute_source("set -o pipefail", &mut state).await;
        assert_eq!(enabled.status().code(), 0);
        assert!(enabled.diagnostics().is_empty());
        assert!(state.options().pipefail());

        let disabled = execute_source("set +o pipefail", &mut state).await;
        assert_eq!(disabled.status().code(), 0);
        assert!(disabled.diagnostics().is_empty());
        assert!(!state.options().pipefail());

        let pipeline = execute_source("set -o pipefail | echo ignored", &mut state).await;
        assert_eq!(pipeline.status().code(), 0);
        assert_eq!(pipeline.stdout(), b"ignored\n");
        assert!(pipeline.diagnostics().is_empty());
        assert!(!state.options().pipefail());

        for source in ["set", "set -o", "set -o unknown", "set -o pipefail extra"] {
            let invalid = execute_source(source, &mut state).await;
            assert_eq!(invalid.status().code(), 2, "source={source}");
            assert_eq!(invalid.diagnostics().len(), 1, "source={source}");
            assert_eq!(
                invalid.diagnostics()[0].code(),
                ExecutionDiagnosticCode::InvalidArguments,
                "source={source}"
            );
            assert_eq!(
                invalid.diagnostics()[0].message(),
                "set supports exactly `-o pipefail` or `+o pipefail`",
                "source={source}"
            );
            assert!(!state.options().pipefail(), "source={source}");
        }
    }

    #[tokio::test]
    async fn stateful_pipeline_builtins_use_cloned_state_and_preserve_stream_status() {
        const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child directory");
        let executable = compile_pipeline_helper(&directory);
        let mut state = ShellState::from_process().expect("process shell state");
        state.set_cwd(&directory.0);
        state
            .environment_mut()
            .insert("PATH", directory.0.join("bin").into_os_string());
        state
            .set_variable("PIPE_VALUE", "parent")
            .expect("pipeline variable");
        state.environment_mut().insert("PIPE_VALUE", "parent");

        let cloned_cwd = execute_source("cd child | pwd; pwd", &mut state).await;
        assert_eq!(
            cloned_cwd.stdout(),
            format!("{}\n{}\n", directory.0.display(), directory.0.display()).as_bytes()
        );
        assert!(cloned_cwd.diagnostics().is_empty());
        assert_eq!(state.cwd(), directory.0);

        let cloned_export = execute_source(
            "export PIPE_VALUE=child | echo \"$PIPE_VALUE\"; echo \"$PIPE_VALUE\"",
            &mut state,
        )
        .await;
        assert_eq!(cloned_export.stdout(), b"parent\nparent\n");
        assert!(cloned_export.diagnostics().is_empty());
        assert_eq!(state.variable("PIPE_VALUE"), Some(OsStr::new("parent")));
        assert_eq!(
            state.environment().get("PIPE_VALUE"),
            Some(OsStr::new("parent"))
        );

        let cloned_unset = execute_source(
            "unset PIPE_VALUE | echo \"$PIPE_VALUE\"; echo \"$PIPE_VALUE\"",
            &mut state,
        )
        .await;
        assert_eq!(cloned_unset.stdout(), b"parent\nparent\n");
        assert!(cloned_unset.diagnostics().is_empty());
        assert_eq!(state.variable("PIPE_VALUE"), Some(OsStr::new("parent")));

        let cloned_control = execute_source("set -o pipefail | exit 7; echo $?", &mut state).await;
        assert_eq!(cloned_control.stdout(), b"7\n");
        assert!(cloned_control.diagnostics().is_empty());
        assert_eq!(cloned_control.exit_requested(), None);
        assert!(!state.options().pipefail());

        state.options_mut().set_pipefail(true);
        let failed_cd = execute_source("cd missing | echo continued; echo $?", &mut state).await;
        assert_eq!(failed_cd.stdout(), b"continued\n1\n");
        assert_eq!(failed_cd.diagnostics().len(), 1);
        assert_eq!(
            failed_cd.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(state.cwd(), directory.0);

        let portable_broken_pipe = execute_source("echo ignored | cd .; echo $?", &mut state).await;
        assert_eq!(portable_broken_pipe.stdout(), b"1\n");
        assert!(portable_broken_pipe.stderr().is_empty());
        assert!(portable_broken_pipe.diagnostics().is_empty());

        let broken_pipe_source =
            format!("{executable} produce-soft {PAYLOAD_BYTES} | cd .; echo $?");
        let broken_pipe = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_source(&broken_pipe_source, &mut state),
        )
        .await
        .expect("stateful closed reader delivers native broken pipe");
        assert_eq!(broken_pipe.stdout(), b"9\n");
        assert!(broken_pipe.stderr().is_empty());
        assert!(broken_pipe.diagnostics().is_empty());

        let downstream_eof = execute_source(
            &format!("export PIPE_VALUE=child | {executable} copy stateful"),
            &mut state,
        )
        .await;
        assert_eq!(downstream_eof.status().code(), 0);
        assert!(downstream_eof.stdout().is_empty());
        assert_eq!(downstream_eof.stderr(), b"stateful-stderr\n");
        assert!(downstream_eof.diagnostics().is_empty());
        assert_eq!(
            state.environment().get("PIPE_VALUE"),
            Some(OsStr::new("parent"))
        );

        let redirected_output = execute_source(
            &format!(
                "export PIPE_VALUE=child >stateful-stage.log | {executable} copy stateful-redirected"
            ),
            &mut state,
        )
        .await;
        assert_eq!(redirected_output.status().code(), 0);
        assert!(redirected_output.stdout().is_empty());
        assert_eq!(redirected_output.stderr(), b"stateful-redirected-stderr\n");
        assert!(redirected_output.diagnostics().is_empty());
        assert!(
            fs::read(directory.0.join("stateful-stage.log"))
                .expect("redirected stateful output")
                .is_empty()
        );
        assert_eq!(
            state.environment().get("PIPE_VALUE"),
            Some(OsStr::new("parent"))
        );

        let redirected_diagnostic = execute_source(
            "cd missing 2>stateful-pipeline-diagnostic.log | echo continued",
            &mut state,
        )
        .await;
        assert_eq!(redirected_diagnostic.status().code(), 1);
        assert_eq!(redirected_diagnostic.stdout(), b"continued\n");
        assert_eq!(redirected_diagnostic.diagnostics().len(), 1);
        assert_eq!(
            redirected_diagnostic.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(
            redirected_diagnostic.stderr(),
            redirected_diagnostic.diagnostics()[0].render().as_bytes()
        );
        assert!(
            fs::read(directory.0.join("stateful-pipeline-diagnostic.log"))
                .expect("stateful pipeline diagnostic target")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn native_and_portable_pipelines_stream_with_backpressure_and_pipefail() {
        const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

        let directory = TestDirectory::new();
        let executable = compile_pipeline_helper(&directory);
        let mut state = ShellState::from_process().expect("process shell state");
        state.set_cwd(&directory.0);
        state
            .environment_mut()
            .insert("PATH", directory.0.join("bin").into_os_string());
        let expected: Vec<u8> = (0..PAYLOAD_BYTES)
            .map(|index| u8::try_from(index % 251).expect("bounded byte"))
            .collect();
        fs::write(directory.0.join("portable-input.bin"), &expected)
            .expect("write portable redirection payload");
        let redirected_input_source =
            format!("cat - <portable-input.bin | {executable} copy redirected-input");
        let redirected_input = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_source(&redirected_input_source, &mut state),
        )
        .await
        .expect("portable file reader streams with backpressure");
        assert_eq!(redirected_input.status().code(), 0);
        assert!(redirected_input.diagnostics().is_empty());
        assert_eq!(redirected_input.stderr(), b"redirected-input-stderr\n");
        assert_eq!(redirected_input.stdout().len(), expected.len());
        assert!(
            redirected_input.stdout() == expected,
            "portable file input changed the payload"
        );

        let redirected_output_source =
            format!("{executable} produce {PAYLOAD_BYTES} | cat - >portable-output.bin");
        let redirected_output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_source(&redirected_output_source, &mut state),
        )
        .await
        .expect("portable file writer streams with backpressure");
        assert_eq!(redirected_output.status().code(), 0);
        assert!(redirected_output.stdout().is_empty());
        assert_eq!(redirected_output.stderr(), b"producer-stderr\n");
        assert!(redirected_output.diagnostics().is_empty());
        assert_eq!(
            fs::read(directory.0.join("portable-output.bin")).expect("portable redirected output"),
            expected
        );

        let source = format!(
            "{executable} produce {PAYLOAD_BYTES} | {executable} copy middle | {executable} copy final"
        );

        let execution = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_source(&source, &mut state),
        )
        .await
        .expect("native pipeline completes without deadlock");

        assert_eq!(execution.status().code(), 0);
        assert!(execution.diagnostics().is_empty());
        assert_eq!(
            execution.stderr(),
            b"producer-stderr\nmiddle-stderr\nfinal-stderr\n"
        );
        assert_eq!(execution.stdout().len(), expected.len());
        assert!(execution.stdout() == expected, "pipeline payload changed");

        let portable_producer = execute_source(
            &format!("echo portable-stage | {executable} copy echo"),
            &mut state,
        )
        .await;
        assert_eq!(portable_producer.status().code(), 0);
        assert!(portable_producer.diagnostics().is_empty());
        assert_eq!(portable_producer.stdout(), b"portable-stage\n");
        assert_eq!(portable_producer.stderr(), b"echo-stderr\n");

        let mixed_source =
            format!("{executable} produce {PAYLOAD_BYTES} | cat - | {executable} copy mixed");
        let mixed = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_source(&mixed_source, &mut state),
        )
        .await
        .expect("mixed pipeline completes without deadlock");
        assert_eq!(mixed.status().code(), 0);
        assert!(mixed.diagnostics().is_empty());
        assert_eq!(mixed.stderr(), b"producer-stderr\nmixed-stderr\n");
        assert_eq!(mixed.stdout().len(), expected.len());
        assert!(mixed.stdout() == expected, "mixed pipeline payload changed");

        let final_portable_source = format!("{executable} produce {PAYLOAD_BYTES} | cat -");
        let final_portable = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_source(&final_portable_source, &mut state),
        )
        .await
        .expect("final portable capture completes without deadlock");
        assert_eq!(final_portable.status().code(), 0);
        assert!(final_portable.diagnostics().is_empty());
        assert_eq!(final_portable.stderr(), b"producer-stderr\n");
        assert_eq!(final_portable.stdout().len(), expected.len());
        assert!(
            final_portable.stdout() == expected,
            "final portable capture changed the payload"
        );

        fs::write(
            directory.0.join("search.txt"),
            b"zero\nneedle one\nneedle two\nlast\n",
        )
        .expect("write portable pipeline input");
        let searched = execute_source(
            &format!("cat search.txt | grep -Fn needle - | {executable} copy grep"),
            &mut state,
        )
        .await;
        assert_eq!(searched.status().code(), 0);
        assert!(searched.diagnostics().is_empty());
        assert_eq!(searched.stdout(), b"2:needle one\n3:needle two\n");
        assert_eq!(searched.stderr(), b"grep-stderr\n");

        let echo_and_grep = execute_source("echo alpha | grep -F alpha -", &mut state).await;
        assert_eq!(echo_and_grep.status().code(), 0);
        assert!(echo_and_grep.diagnostics().is_empty());
        assert_eq!(echo_and_grep.stdout(), b"alpha\n");
        assert!(echo_and_grep.stderr().is_empty());

        let pwd_and_cat = execute_source("pwd | cat -", &mut state).await;
        assert_eq!(pwd_and_cat.status().code(), 0);
        assert!(pwd_and_cat.diagnostics().is_empty());
        assert_eq!(
            pwd_and_cat.stdout(),
            format!("{}\n", state.cwd().display()).as_bytes()
        );
        assert!(pwd_and_cat.stderr().is_empty());

        let ls_and_grep = execute_source("ls -1 | grep -F search.txt -", &mut state).await;
        assert_eq!(ls_and_grep.status().code(), 0);
        assert!(ls_and_grep.diagnostics().is_empty());
        assert_eq!(ls_and_grep.stdout(), b"search.txt\n");
        assert!(ls_and_grep.stderr().is_empty());

        let missing_without_pipefail = execute_source(
            &format!("cat missing-input | {executable} copy missing; echo $?"),
            &mut state,
        )
        .await;
        assert_eq!(missing_without_pipefail.status().code(), 0);
        assert_eq!(missing_without_pipefail.stdout(), b"0\n");
        assert_eq!(missing_without_pipefail.diagnostics().len(), 1);
        assert_eq!(
            missing_without_pipefail.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert!(
            missing_without_pipefail.diagnostics()[0]
                .message()
                .contains("missing-input")
        );
        assert!(
            missing_without_pipefail
                .stderr()
                .starts_with(b"missing-stderr\nash: cannot read `missing-input`")
        );

        state.options_mut().set_pipefail(true);
        let missing_with_pipefail = execute_source(
            &format!("cat missing-input | {executable} copy missing; echo $?"),
            &mut state,
        )
        .await;
        assert_eq!(missing_with_pipefail.status().code(), 0);
        assert_eq!(missing_with_pipefail.stdout(), b"1\n");
        assert_eq!(missing_with_pipefail.diagnostics().len(), 1);

        let broken_pipe = execute_source(
            &format!("{executable} produce-soft {PAYLOAD_BYTES} | pwd; echo $?"),
            &mut state,
        )
        .await;
        assert_eq!(broken_pipe.status().code(), 0);
        assert!(broken_pipe.diagnostics().is_empty());
        assert_eq!(
            broken_pipe.stdout(),
            format!("{}\n9\n", state.cwd().display()).as_bytes()
        );
        assert!(broken_pipe.stderr().is_empty());
    }

    #[tokio::test]
    async fn pipeline_capture_failure_reaps_every_supervised_process_tree() {
        let directory = TestDirectory::new();
        let executable_name = compile_pipeline_helper(&directory);
        let executable = directory.0.join("bin").join(executable_name);
        let pipe = ProcessPipeId::new(0);
        let invocation = |arguments: Vec<OsString>, stdin, stdout, stderr| NativeInvocation {
            executable: executable.clone(),
            arguments,
            cwd: directory.0.clone(),
            environment: Vec::new(),
            capture_limit: 64,
            files: Vec::new(),
            stdin,
            stdout,
            stderr,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_pipeline(PipelineInvocation {
                stages: vec![
                    PipelineStageInvocation::Native(invocation(
                        vec![
                            OsString::from("hold"),
                            OsString::from("ready"),
                            OsString::from("escaped"),
                        ],
                        ProcessStdio::Null,
                        ProcessStdio::Pipe(pipe),
                        ProcessStdio::Null,
                    )),
                    PipelineStageInvocation::Native(invocation(
                        vec![
                            OsString::from("produce-after-input"),
                            OsString::from("4096"),
                        ],
                        ProcessStdio::Pipe(pipe),
                        ProcessStdio::Capture(STDOUT_CAPTURE),
                        ProcessStdio::Capture(STDERR_CAPTURE),
                    )),
                ],
                closed_pipe_ends: Vec::new(),
                parent_pipe_ends: Vec::new(),
                parent_captures: Vec::new(),
                capture_limit: 64,
            }),
        )
        .await
        .expect("capture failure cleanup completes");
        assert!(matches!(
            result,
            Err(NativeCommandError::CaptureLimit { max: 64 })
        ));
        assert!(
            directory.0.join("ready").is_file(),
            "the supervised process tree must start before capture failure"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
        assert!(
            !directory.0.join("escaped").exists(),
            "a descendant survived capture-failure cleanup"
        );
    }

    #[tokio::test]
    async fn parameter_expansion_preserves_native_argv_fields_and_observes_previous_status() {
        let mut state = ShellState::new("/fixture/work");
        state
            .set_variable("VALUE", " alpha  beta ")
            .expect("variable");
        let lookup =
            |command: &str, _cwd: &std::path::Path, _environment: &crate::PlatformEnvironment| {
                (command == "tool").then(|| PathBuf::from("/fixture/bin/tool"))
            };
        let runner = RecordingRunner::new(NativeCommandOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit: ProcessExit {
                success: false,
                code: Some(23),
                signal: None,
            },
        });

        let execution = execute_source_with_runner(
            "tool pre${VALUE}post \"$VALUE\" '$VALUE' \\$VALUE $MISSING \"$MISSING\"; echo $?",
            &mut state,
            &lookup,
            HostPlatform::Linux,
            &runner,
        )
        .await;

        assert_eq!(execution.stdout(), b"23\n");
        assert!(execution.stderr().is_empty());
        assert!(execution.diagnostics().is_empty());
        assert_eq!(execution.status().code(), 0);
        assert_eq!(state.last_status().code(), 0);
        let invocation = runner
            .invocation
            .lock()
            .expect("invocation lock")
            .clone()
            .expect("invocation");
        assert_eq!(
            invocation.arguments,
            [
                OsString::from("pre"),
                OsString::from("alpha"),
                OsString::from("beta"),
                OsString::from("post"),
                OsString::from(" alpha  beta "),
                OsString::from("$VALUE"),
                OsString::from("$VALUE"),
                OsString::new(),
            ]
        );
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn non_utf8_expanded_command_names_fail_explicitly() {
        #[cfg(unix)]
        let command = {
            use std::os::unix::ffi::OsStringExt as _;
            OsString::from_vec(vec![b't', 0xff])
        };
        #[cfg(windows)]
        let command = {
            use std::os::windows::ffi::OsStringExt as _;
            OsString::from_wide(&[u16::from(b't'), 0xd800])
        };
        let mut state = ShellState::new(".");
        state
            .set_variable("COMMAND", command)
            .expect("native command variable");

        let execution = execute_source("$COMMAND", &mut state).await;

        assert!(execution.stdout().is_empty());
        assert_eq!(execution.diagnostics().len(), 1);
        assert_eq!(
            execution.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Resolution
        );
        assert_eq!(
            execution.diagnostics()[0].message(),
            "expanded command name must be valid UTF-8"
        );
        assert_eq!(execution.diagnostics()[0].span(), SourceSpan::new(0, 8));
        assert_eq!(execution.status().code(), 127);
        assert_eq!(execution.status().kind(), ShellStatusKind::ResolutionError);
    }

    #[tokio::test]
    async fn native_infrastructure_and_capture_failures_are_typed() {
        let lookup =
            |_command: &str, _cwd: &std::path::Path, _environment: &crate::PlatformEnvironment| {
                Some(PathBuf::from("/fixture/bin/tool"))
            };
        for (capture_limit, code, kind, command_message, pipeline_message) in [
            (
                false,
                126,
                ShellStatusKind::SpawnError,
                "cannot execute `/fixture/bin/tool`: the process did not expose its captured stdout",
                "cannot execute pipeline: the process did not expose its captured final stdout",
            ),
            (
                true,
                1,
                ShellStatusKind::Exited,
                "native command output exceeds the 134217728-byte synchronous shell capture ceiling",
                "pipeline output exceeds the 134217728-byte synchronous shell capture ceiling",
            ),
        ] {
            for (source, message) in [("tool", command_message), ("tool | tool", pipeline_message)]
            {
                let mut state = ShellState::new("/fixture");
                let failure = execute_source_with_runner(
                    source,
                    &mut state,
                    &lookup,
                    HostPlatform::Linux,
                    &FailingRunner { capture_limit },
                )
                .await;

                assert!(failure.stdout().is_empty(), "source={source}");
                assert_eq!(failure.diagnostics().len(), 1, "source={source}");
                assert_eq!(
                    failure.diagnostics()[0].code(),
                    ExecutionDiagnosticCode::Process,
                    "source={source}"
                );
                assert_eq!(
                    failure.diagnostics()[0].message(),
                    message,
                    "source={source}"
                );
                assert_eq!(failure.status().code(), code, "source={source}");
                assert_eq!(failure.status().kind(), kind, "source={source}");
                assert_eq!(
                    failure.rendered_stderr(),
                    failure.diagnostics()[0].render().into_bytes(),
                    "source={source}"
                );
            }
        }
    }

    #[tokio::test]
    async fn export_and_unset_persist_variable_environment_and_home_state() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        let mut state = ShellState::new(&directory.0);

        let exported =
            execute_source("export TOKEN='alpha beta=gamma'; export EMPTY=", &mut state).await;
        assert!(exported.stdout().is_empty());
        assert!(exported.diagnostics().is_empty());
        assert_eq!(exported.status().code(), 0);
        assert_eq!(
            state.variable("TOKEN"),
            Some(OsStr::new("alpha beta=gamma"))
        );
        assert_eq!(
            state.environment().get("TOKEN"),
            Some(OsStr::new("alpha beta=gamma"))
        );
        assert_eq!(state.variable("EMPTY"), Some(OsStr::new("")));
        assert_eq!(state.environment().get("EMPTY"), Some(OsStr::new("")));

        let overwritten = execute_source("export -- TOKEN=second", &mut state).await;
        assert!(overwritten.diagnostics().is_empty());
        assert_eq!(state.variable("TOKEN"), Some(OsStr::new("second")));
        assert_eq!(state.environment().get("TOKEN"), Some(OsStr::new("second")));

        let home = execute_source("export HOME=child; cd; pwd", &mut state).await;
        let child = fs::canonicalize(directory.0.join("child")).expect("canonical child");
        assert_eq!(home.stdout(), format!("{}\n", child.display()).as_bytes());
        assert!(home.diagnostics().is_empty());
        assert_eq!(state.cwd(), child);

        let unset = execute_source("unset -- TOKEN; unset MISSING; unset HOME", &mut state).await;
        assert!(unset.stdout().is_empty());
        assert!(unset.diagnostics().is_empty());
        assert_eq!(unset.status().code(), 0);
        assert_eq!(state.variable("TOKEN"), None);
        assert_eq!(state.environment().get("TOKEN"), None);
        assert_eq!(state.variable("HOME"), None);
        assert_eq!(state.environment().get("HOME"), None);
    }

    #[tokio::test]
    async fn exported_values_expand_in_later_stateful_and_portable_commands() {
        let mut state = ShellState::new(".");

        let execution = execute_source(
            "export BASE='alpha beta'; export COPY=\"$BASE\"; echo \"${COPY}\"; $MISSING; echo $?",
            &mut state,
        )
        .await;

        assert_eq!(execution.stdout(), b"alpha beta\n0\n");
        assert!(execution.stderr().is_empty());
        assert!(execution.diagnostics().is_empty());
        assert_eq!(state.variable("COPY"), Some(OsStr::new("alpha beta")));
        assert_eq!(
            state.environment().get("COPY"),
            Some(OsStr::new("alpha beta"))
        );
        assert_eq!(execution.status().code(), 0);
    }

    #[tokio::test]
    async fn export_and_unset_reject_options_arity_assignments_and_invalid_names() {
        let mut state = ShellState::new(".");
        let cases = [
            (
                "export",
                "export requires exactly one NAME=VALUE assignment",
            ),
            (
                "export A=1 B=2",
                "export accepts exactly one NAME=VALUE assignment",
            ),
            ("export -p", "export does not support option `-p`"),
            ("export TOKEN", "export requires a NAME=VALUE assignment"),
            (
                "export 1TOKEN=value",
                "export: shell names must be non-empty ASCII identifiers beginning with a letter or underscore",
            ),
            ("unset", "unset requires exactly one name"),
            ("unset A B", "unset accepts exactly one name"),
            ("unset -f", "unset does not support option `-f`"),
            (
                "unset bad-name",
                "unset: shell names must be non-empty ASCII identifiers beginning with a letter or underscore",
            ),
        ];

        for (source, message) in cases {
            let failure = execute_source(source, &mut state).await;
            assert!(failure.stdout().is_empty(), "source={source}");
            assert_eq!(
                failure.diagnostics()[0].code(),
                ExecutionDiagnosticCode::InvalidArguments,
                "source={source}"
            );
            assert_eq!(
                failure.diagnostics()[0].message(),
                message,
                "source={source}"
            );
            assert_eq!(failure.status().code(), 2, "source={source}");
        }
        assert_eq!(state.variable("TOKEN"), None);
        assert_eq!(state.environment().get("TOKEN"), None);
    }

    #[tokio::test]
    async fn parse_argument_filesystem_and_unsupported_failures_are_distinct() {
        let mut state = ShellState::new(".");
        let parse = execute_source("echo 'unterminated", &mut state).await;
        assert!(matches!(
            parse.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Parse(_)
        ));
        assert_eq!(parse.status().kind(), ShellStatusKind::ParseError);
        assert_eq!(parse.status().code(), 2);

        let builtin = execute_source("pwd extra", &mut state).await;
        assert_eq!(
            builtin.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(builtin.status().kind(), ShellStatusKind::Exited);
        assert_eq!(builtin.status().code(), 2);

        let filesystem = execute_source("cd ''", &mut state).await;
        assert_eq!(
            filesystem.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(filesystem.diagnostics()[0].span(), SourceSpan::new(0, 5));
        assert_eq!(filesystem.status().kind(), ShellStatusKind::Exited);
        assert_eq!(filesystem.status().code(), 1);

        state.environment_mut().insert("HOME", "");
        let empty_home = execute_source("cd", &mut state).await;
        assert_eq!(
            empty_home.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(empty_home.status().code(), 1);

        let unsupported = execute_source("alias", &mut state).await;
        assert_eq!(
            unsupported.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Unsupported
        );
        assert_eq!(unsupported.diagnostics()[0].span(), SourceSpan::new(0, 5));
        assert_eq!(unsupported.status().kind(), ShellStatusKind::Exited);
        assert_eq!(unsupported.status().code(), 2);
    }

    #[tokio::test]
    async fn ls_lists_direct_children_in_stable_order_and_observes_cd() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        fs::write(directory.0.join("b.txt"), b"b").expect("write b");
        fs::write(directory.0.join("a.txt"), b"a").expect("write a");
        fs::write(directory.0.join(".hidden"), b"hidden").expect("write hidden");
        fs::write(directory.0.join("child/nested.txt"), b"nested").expect("write nested");
        let mut state = ShellState::new(&directory.0);

        let visible = execute_source("ls -1", &mut state).await;
        assert_eq!(visible.stdout(), b"a.txt\nb.txt\nchild\n");
        assert!(visible.diagnostics().is_empty());
        assert_eq!(visible.status().code(), 0);

        let all = execute_source("ls --all", &mut state).await;
        assert_eq!(all.stdout(), b".hidden\na.txt\nb.txt\nchild\n");

        let nested = execute_source("cd child; ls", &mut state).await;
        assert_eq!(nested.stdout(), b"nested.txt\n");
        assert!(nested.diagnostics().is_empty());
    }

    #[tokio::test]
    async fn ls_supports_directory_end_of_options_and_clear_failures() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        fs::write(directory.0.join("sample.txt"), b"sample").expect("write sample");
        fs::write(directory.0.join("-dash"), b"dash").expect("write dash");
        let mut state = ShellState::new(&directory.0);

        let file = execute_source("ls sample.txt", &mut state).await;
        assert_eq!(file.stdout(), b"sample.txt\n");
        assert!(file.diagnostics().is_empty());

        let directory_only = execute_source("ls -ad1 child", &mut state).await;
        assert_eq!(directory_only.stdout(), b"child\n");
        let dash = execute_source("ls -- -dash", &mut state).await;
        assert_eq!(dash.stdout(), b"-dash\n");

        let option = execute_source("ls -l", &mut state).await;
        assert_eq!(
            option.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(
            option.diagnostics()[0].message(),
            "ls does not support option `-l`"
        );
        assert_eq!(option.status().code(), 2);

        let paths = execute_source("ls child sample.txt", &mut state).await;
        assert_eq!(
            paths.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(
            paths.diagnostics()[0].message(),
            "ls accepts at most one path"
        );

        let missing = execute_source("ls missing", &mut state).await;
        assert_eq!(
            missing.diagnostics()[0].code(),
            ExecutionDiagnosticCode::Filesystem
        );
        assert_eq!(missing.status().code(), 1);

        let empty = execute_source("ls ''", &mut state).await;
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
    #[tokio::test]
    async fn ls_preserves_and_stably_sorts_native_names() {
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

        let execution = execute_source("ls", &mut state).await;

        #[cfg(target_os = "linux")]
        assert_eq!(execution.stdout(), b"a\n\x80\n\xe9\x9b\xaa\n\xff\n");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(execution.stdout(), "a\n雪\n".as_bytes());
        assert!(execution.diagnostics().is_empty());
    }

    #[tokio::test]
    async fn cat_emits_exact_binary_bytes_and_observes_cd() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        fs::write(directory.0.join("雪.bin"), b"root").expect("write unicode path");
        fs::write(directory.0.join("child/payload.bin"), [0x00, 0xff, b'\n'])
            .expect("write binary payload");
        let mut state = ShellState::new(&directory.0);

        let execution = execute_source("cat '雪.bin'; cd child; cat payload.bin", &mut state).await;

        assert_eq!(execution.stdout(), b"root\x00\xff\n");
        assert!(execution.diagnostics().is_empty());
        assert_eq!(execution.status().code(), 0);
        assert_eq!(
            state.cwd(),
            fs::canonicalize(directory.0.join("child")).expect("canonical child")
        );
    }

    #[tokio::test]
    async fn cat_supports_end_of_options_and_clear_bounded_failures() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("-"), b"hyphen").expect("write hyphen");
        fs::create_dir(directory.0.join("child")).expect("create child");
        let oversized =
            fs::File::create(directory.0.join("oversized.bin")).expect("create oversized file");
        oversized
            .set_len(super::MAX_READ_FILE_BYTES + 1)
            .expect("set oversized length");
        let mut state = ShellState::new(&directory.0);

        let dash = execute_source("cat -- -", &mut state).await;
        assert_eq!(dash.stdout(), b"hyphen");
        assert!(dash.diagnostics().is_empty());

        let option = execute_source("cat -n", &mut state).await;
        assert_eq!(
            option.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert_eq!(
            option.diagnostics()[0].message(),
            "cat does not support option `-n`"
        );
        assert_eq!(option.status().code(), 2);

        let stdin = execute_source("cat -", &mut state).await;
        assert_eq!(
            stdin.diagnostics()[0].message(),
            "cat standard-input operand `-` is not implemented yet"
        );
        assert_eq!(stdin.status().code(), 2);

        let missing_argument = execute_source("cat", &mut state).await;
        assert_eq!(
            missing_argument.diagnostics()[0].message(),
            "cat requires exactly one path"
        );
        let paths = execute_source("cat one two", &mut state).await;
        assert_eq!(
            paths.diagnostics()[0].message(),
            "cat accepts exactly one path"
        );

        for source in ["cat missing", "cat child", "cat ''", "cat oversized.bin"] {
            let failure = execute_source(source, &mut state).await;
            assert!(failure.stdout().is_empty(), "source={source}");
            assert_eq!(
                failure.diagnostics()[0].code(),
                ExecutionDiagnosticCode::Filesystem,
                "source={source}"
            );
            assert_eq!(failure.status().code(), 1, "source={source}");
        }
    }

    #[tokio::test]
    async fn grep_matches_regex_literal_case_and_line_modes_after_cd() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("child")).expect("create child");
        fs::write(
            directory.0.join("雪.txt"),
            b"alpha one\r\nBeta 42\nliteral a+b\nALPHA two\n",
        )
        .expect("write unicode path");
        fs::write(directory.0.join("child/-data.txt"), b"-needle\nNeedle\n")
            .expect("write leading-dash path");
        let mut state = ShellState::new(&directory.0);

        let regex =
            execute_source("grep --extended-regexp 'Beta [0-9]+' '雪.txt'", &mut state).await;
        assert_eq!(regex.stdout(), b"Beta 42\n");
        assert!(regex.diagnostics().is_empty());
        assert_eq!(regex.status().code(), 0);

        let literal = execute_source("grep --fixed-strings 'a+b' '雪.txt'", &mut state).await;
        assert_eq!(literal.stdout(), b"literal a+b\n");

        let long_flags = execute_source(
            "grep --fixed-strings --ignore-case --line-number alpha '雪.txt'",
            &mut state,
        )
        .await;
        assert_eq!(long_flags.stdout(), b"1:alpha one\n4:ALPHA two\n");

        let combined = execute_source("grep -inE '^alpha' '雪.txt'", &mut state).await;
        assert_eq!(combined.stdout(), b"1:alpha one\n4:ALPHA two\n");

        let no_match = execute_source("grep absent '雪.txt'", &mut state).await;
        assert!(no_match.stdout().is_empty());
        assert!(no_match.diagnostics().is_empty());
        assert_eq!(no_match.status().code(), 1);

        let dash = execute_source("cd child; grep -Fn -- -needle -data.txt", &mut state).await;
        assert_eq!(dash.stdout(), b"1:-needle\n");
        assert!(dash.diagnostics().is_empty());
        assert_eq!(dash.status().code(), 0);
        assert_eq!(
            state.cwd(),
            fs::canonicalize(directory.0.join("child")).expect("canonical child")
        );
    }

    #[tokio::test]
    async fn grep_reports_clear_argument_regex_text_and_bounded_failures() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("sample.txt"), b"needle\n").expect("write sample");
        fs::write(directory.0.join("binary.bin"), [0xff, b'n']).expect("write binary");
        fs::create_dir(directory.0.join("child")).expect("create child");
        let oversized =
            fs::File::create(directory.0.join("oversized.txt")).expect("create oversized file");
        oversized
            .set_len(ash_ops::MAX_SEARCH_FILE_BYTES + 1)
            .expect("set oversized length");
        let mut state = ShellState::new(&directory.0);

        let no_arguments = execute_source("grep", &mut state).await;
        assert_eq!(
            no_arguments.diagnostics()[0].message(),
            "grep requires a pattern and one path"
        );
        let no_path = execute_source("grep needle", &mut state).await;
        assert_eq!(no_path.diagnostics()[0].message(), "grep requires one path");
        let paths = execute_source("grep needle one two", &mut state).await;
        assert_eq!(
            paths.diagnostics()[0].message(),
            "grep accepts exactly one pattern and one path"
        );

        let option = execute_source("grep -r needle sample.txt", &mut state).await;
        assert_eq!(
            option.diagnostics()[0].message(),
            "grep does not support option `-r`"
        );
        let conflict = execute_source("grep -EF needle sample.txt", &mut state).await;
        assert_eq!(
            conflict.diagnostics()[0].message(),
            "grep options `-E` and `-F` cannot be combined"
        );
        let stdin = execute_source("grep needle -", &mut state).await;
        assert_eq!(
            stdin.diagnostics()[0].message(),
            "grep standard-input operand `-` is not implemented yet"
        );
        for failure in [&no_arguments, &no_path, &paths, &option, &conflict, &stdin] {
            assert_eq!(
                failure.diagnostics()[0].code(),
                ExecutionDiagnosticCode::InvalidArguments
            );
            assert_eq!(failure.status().code(), 2);
        }

        let regex = execute_source("grep '[' sample.txt", &mut state).await;
        assert_eq!(
            regex.diagnostics()[0].code(),
            ExecutionDiagnosticCode::InvalidArguments
        );
        assert!(
            regex.diagnostics()[0]
                .message()
                .starts_with("grep regular expression is invalid:")
        );
        assert_eq!(regex.status().code(), 2);

        for source in [
            "grep needle missing",
            "grep needle child",
            "grep needle ''",
            "grep needle binary.bin",
            "grep needle oversized.txt",
        ] {
            let failure = execute_source(source, &mut state).await;
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
    #[tokio::test]
    async fn pwd_preserves_or_reversibly_escapes_native_path_units() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};

            let path = PathBuf::from(std::ffi::OsString::from_vec(b"native-\xff-path".to_vec()));
            let mut state = ShellState::new(&path);
            let execution = execute_source("pwd", &mut state).await;

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
            let execution = execute_source("pwd", &mut state).await;

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
