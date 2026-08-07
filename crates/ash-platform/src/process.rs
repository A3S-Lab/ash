use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{PipeReader, PipeWriter};
use std::path::PathBuf;
use std::process::Stdio;

use futures::future::join_all;
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(windows)]
use std::os::windows::io::OwnedHandle;
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

use crate::{PlatformError, Workspace};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentChange {
    Set(OsString, OsString),
    Remove(OsString),
}

/// Stable identifier for one operating-system pipe inside a process graph.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProcessPipeId(u32);

impl ProcessPipeId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}

/// One process-graph pipe endpoint that the parent closes after spawning.
///
/// This explicitly models a pipeline endpoint replaced by another redirection:
/// closing a writer makes a connected child reader observe EOF, while closing a
/// reader makes a connected child writer observe the platform's broken-pipe
/// behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosedProcessPipeEnd {
    Reader(ProcessPipeId),
    Writer(ProcessPipeId),
}

/// One process-graph pipe endpoint retained by the parent after spawning.
///
/// Parent readers and writers are returned as asynchronous files on
/// [`NativeProcessGraph`] so in-process tasks can participate in the same OS
/// pipe graph without relaying child-to-child edges through the parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParentProcessPipeEnd {
    Reader(ProcessPipeId),
    Writer(ProcessPipeId),
}

/// Stable graph-local identifier for one file owned by an in-process parent task.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ParentProcessFileId(u32);

impl ParentProcessFileId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}

/// Stable specification-local identifier for one opened native process file.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProcessFileId(u32);

impl ProcessFileId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}

/// Stable specification-local identifier for one parent-facing output capture.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProcessCaptureId(u32);

impl ProcessCaptureId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}

/// File-open mode for one native child or in-process parent redirection resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeProcessFileMode {
    Read,
    Write,
    Append,
}

/// One source-ordered file resource opened before a native child starts.
///
/// Plan-local identifiers must be unique. Multiple final descriptors may name
/// the same identifier so their cloned handles share one open-file description.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProcessFile {
    pub id: ProcessFileId,
    pub path: PathBuf,
    pub mode: NativeProcessFileMode,
}

/// One source-ordered file resource retained for an in-process parent task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParentProcessFile {
    pub id: ParentProcessFileId,
    pub path: PathBuf,
    pub mode: NativeProcessFileMode,
}

/// One entry in the global file-open order for a native process graph.
///
/// Native file identifiers remain local to their process specification, while
/// parent file identifiers are graph-local. A complete order names every file
/// exactly once so mixed child and in-process stages preserve shell source order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProcessGraphFile {
    Process {
        process_index: usize,
        file: ProcessFileId,
    },
    Parent(ParentProcessFileId),
}

/// Explicit standard-I/O mode for one child-process stream.
///
/// A [`ProcessStdio::Piped`] endpoint exposes its corresponding handle through
/// [`ProcessHandle`]. A [`ProcessStdio::Pipe`] endpoint is internal to a graph
/// launched by [`spawn_native_graph`],
/// [`spawn_native_graph_with_closed_pipe_ends`],
/// [`spawn_native_graph_with_parent_pipe_ends`],
/// [`spawn_native_graph_with_parent_io`], or [`Workspace::spawn_graph`].
/// [`ProcessStdio::File`] references the source-ordered file plan on
/// [`NativeProcessSpec`], while [`ProcessStdio::Capture`] exposes one named pipe
/// through [`ProcessHandle::take_capture`]. Reusing a file or capture identifier
/// makes descriptors in that specification share the same underlying resource;
/// equal identifiers in different graph specifications remain independent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProcessStdio {
    /// Reuse the parent's corresponding standard handle.
    Inherit,
    /// Attach the platform null device.
    Null,
    /// Create a parent-facing operating-system pipe owned by [`ProcessHandle`].
    Piped,
    /// Connect this stream to one named operating-system pipe in a process graph.
    ///
    /// An stdin endpoint reads from the pipe. A stdout or stderr endpoint writes
    /// to it. Internal graph endpoints do not expose handles on [`ProcessHandle`].
    Pipe(ProcessPipeId),
    /// Connect this stream to one source-ordered native file resource.
    File(ProcessFileId),
    /// Connect one or more output descriptors to one parent-facing capture pipe.
    Capture(ProcessCaptureId),
}

impl ProcessStdio {
    fn standalone_stdio(self) -> Result<Stdio, PlatformError> {
        match self {
            Self::Inherit => Ok(Stdio::inherit()),
            Self::Null => Ok(Stdio::null()),
            Self::Piped => Ok(Stdio::piped()),
            Self::Pipe(_) => Err(PlatformError::InvalidProcessGraph),
            Self::File(_) | Self::Capture(_) => Err(PlatformError::InvalidProcessRedirection),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    pub executable: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub environment: Vec<EnvironmentChange>,
    pub clear_environment: bool,
    pub stdin: ProcessStdio,
    pub stdout: ProcessStdio,
    pub stderr: ProcessStdio,
}

/// Native-string process description for host-authority frontends.
///
/// Unlike [`ProcessSpec`], paths are already resolved native paths and are not
/// interpreted relative to an ASH workspace capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProcessSpec {
    pub executable: OsString,
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: Vec<EnvironmentChange>,
    pub clear_environment: bool,
    /// Source-ordered file-open plan for this process.
    ///
    /// Every entry is opened in order, including entries superseded by a later
    /// descriptor assignment and therefore absent from the final stdio fields.
    pub files: Vec<NativeProcessFile>,
    pub stdin: ProcessStdio,
    pub stdout: ProcessStdio,
    pub stderr: ProcessStdio,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    pub success: bool,
    pub code: Option<i64>,
    pub signal: Option<i64>,
}

pub struct ProcessHandle {
    child: Box<dyn ChildWrapper>,
    captures: BTreeMap<ProcessCaptureId, tokio::fs::File>,
}

/// Running native process graph plus explicitly retained parent pipe ends and files.
pub struct NativeProcessGraph {
    processes: Vec<ProcessHandle>,
    pipe_readers: BTreeMap<ProcessPipeId, tokio::fs::File>,
    pipe_writers: BTreeMap<ProcessPipeId, tokio::fs::File>,
    parent_files: BTreeMap<ParentProcessFileId, tokio::fs::File>,
}

/// Running native members of one process graph, supervised as a single job.
///
/// Exit statuses retain specification order even though graph consumers spawn
/// before their producers. A wait failure terminates and reaps every remaining
/// process tree before it is returned. Explicit termination likewise covers all
/// members and does not complete until their owned descendants have been reaped.
pub struct NativeProcessJob {
    processes: Vec<ProcessHandle>,
}

impl NativeProcessGraph {
    /// Takes one declared parent reader by its graph-local pipe identifier.
    pub fn take_pipe_reader(&mut self, id: ProcessPipeId) -> Option<tokio::fs::File> {
        self.pipe_readers.remove(&id)
    }

    /// Takes one declared parent writer by its graph-local pipe identifier.
    pub fn take_pipe_writer(&mut self, id: ProcessPipeId) -> Option<tokio::fs::File> {
        self.pipe_writers.remove(&id)
    }

    /// Takes one declared parent-owned file by its graph-local identifier.
    pub fn take_parent_file(&mut self, id: ParentProcessFileId) -> Option<tokio::fs::File> {
        self.parent_files.remove(&id)
    }

    /// Converts the constructed graph into its job-wide process supervisor.
    ///
    /// Any parent pipe ends or files that were not taken are closed here, before
    /// supervision begins.
    #[must_use]
    pub fn into_job(self) -> NativeProcessJob {
        NativeProcessJob {
            processes: self.processes,
        }
    }

    /// Terminates and reaps every native member while graph resources are still owned.
    pub async fn terminate_and_reap(&mut self) -> Result<(), PlatformError> {
        terminate_and_reap_processes(&mut self.processes).await
    }

    /// Consumes the graph wrapper and returns process handles in specification order.
    #[must_use]
    pub fn into_processes(self) -> Vec<ProcessHandle> {
        self.processes
    }
}

impl NativeProcessJob {
    /// Returns the number of native members in this job.
    #[must_use]
    pub fn len(&self) -> usize {
        self.processes.len()
    }

    /// Returns whether this job has no native members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    /// Returns one member's current platform process identifier.
    #[must_use]
    pub fn process_id(&self, process_index: usize) -> Option<u32> {
        self.processes
            .get(process_index)
            .and_then(ProcessHandle::id)
    }

    /// Takes one member's parent-facing stdin handle.
    pub fn take_stdin(&mut self, process_index: usize) -> Option<ChildStdin> {
        self.processes.get_mut(process_index)?.take_stdin()
    }

    /// Takes one member's parent-facing stdout handle.
    pub fn take_stdout(&mut self, process_index: usize) -> Option<ChildStdout> {
        self.processes.get_mut(process_index)?.take_stdout()
    }

    /// Takes one member's parent-facing stderr handle.
    pub fn take_stderr(&mut self, process_index: usize) -> Option<ChildStderr> {
        self.processes.get_mut(process_index)?.take_stderr()
    }

    /// Takes one member's named parent-facing capture pipe.
    pub fn take_capture(
        &mut self,
        process_index: usize,
        id: ProcessCaptureId,
    ) -> Option<tokio::fs::File> {
        self.processes.get_mut(process_index)?.take_capture(id)
    }

    /// Waits for every native member and returns exits in specification order.
    ///
    /// If any wait fails, every remaining process tree is terminated and reaped
    /// before the original wait error is returned.
    pub async fn wait(&mut self) -> Result<Vec<ProcessExit>, PlatformError> {
        let results = join_all(self.processes.iter_mut().map(|process| process.wait())).await;
        let mut exits = Vec::with_capacity(results.len());
        let mut first_error = None;
        for result in results {
            match result {
                Ok(exit) => exits.push(exit),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            let _ = self.terminate_and_reap().await;
            Err(error)
        } else {
            Ok(exits)
        }
    }

    /// Terminates every native member and waits for all owned process trees.
    ///
    /// All members are asked to terminate even when one operation fails. The
    /// first termination error in specification order takes precedence over the
    /// first subsequent wait error.
    pub async fn terminate_and_reap(&mut self) -> Result<(), PlatformError> {
        terminate_and_reap_processes(&mut self.processes).await
    }
}

async fn terminate_and_reap_processes(
    processes: &mut [ProcessHandle],
) -> Result<(), PlatformError> {
    let termination_error = join_all(processes.iter_mut().map(|process| process.terminate()))
        .await
        .into_iter()
        .find_map(Result::err);
    let wait_error = join_all(processes.iter_mut().map(|process| process.wait()))
        .await
        .into_iter()
        .find_map(Result::err);
    match termination_error.or(wait_error) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

impl Workspace {
    pub fn spawn(&self, spec: &ProcessSpec) -> Result<ProcessHandle, PlatformError> {
        spawn_native(&self.resolve_process_spec(spec)?)
    }

    /// Resolves and launches a complete process graph inside this workspace.
    ///
    /// Connected pipe endpoints must have exactly one reader process and one
    /// writer process. One writer may attach both stdout and stderr to the same
    /// pipe. Cycles and unmatched endpoints are rejected before any child starts.
    pub fn spawn_graph(&self, specs: &[ProcessSpec]) -> Result<Vec<ProcessHandle>, PlatformError> {
        let specs = specs
            .iter()
            .map(|spec| self.resolve_process_spec(spec))
            .collect::<Result<Vec<_>, _>>()?;
        spawn_native_graph(&specs)
    }

    fn resolve_process_spec(&self, spec: &ProcessSpec) -> Result<NativeProcessSpec, PlatformError> {
        validate_process_spec(spec)?;
        let cwd = self.resolve_existing(&spec.cwd)?;
        let executable = if spec.executable.contains('/') {
            self.resolve_existing(&spec.executable)?.native
        } else if spec.executable.contains(['\\', ':']) || spec.executable.is_empty() {
            return Err(PlatformError::InvalidLogicalPath);
        } else {
            spec.executable.clone().into()
        };
        Ok(NativeProcessSpec {
            executable: executable.into_os_string(),
            argv: spec.argv.iter().map(OsString::from).collect(),
            cwd: cwd.native,
            environment: spec.environment.clone(),
            clear_environment: spec.clear_environment,
            files: Vec::new(),
            stdin: spec.stdin,
            stdout: spec.stdout,
            stderr: spec.stderr,
        })
    }
}

/// Launches one host executable directly from an already-resolved native
/// specification. No command shell or workspace path interpretation is added.
/// The complete redirection plan is validated before its file entries are opened
/// in source order and before the child starts.
pub fn spawn_native(spec: &NativeProcessSpec) -> Result<ProcessHandle, PlatformError> {
    let graph_pipes = BTreeMap::new();
    let mut prepared = PreparedProcessStdio::new(spec)?;
    let stdin = prepared.input_stdio(spec.stdin, &graph_pipes)?;
    let stdout = prepared.output_stdio(spec.stdout, &graph_pipes)?;
    let stderr = prepared.output_stdio(spec.stderr, &graph_pipes)?;
    let captures = prepared.take_captures();
    spawn_native_with_stdio(spec, stdin, stdout, stderr, captures)
}

/// Launches a complete native process graph connected by operating-system pipes.
///
/// The pipe graph and every redirection plan are validated before file-open side
/// effects begin. File entries are then opened in `specs` order and their own
/// source order, and every pipe is created before the first child starts.
/// Consumers are spawned before their producers, then all parent pipe copies are
/// closed before this function returns. Returned handles retain the same order as
/// `specs`.
pub fn spawn_native_graph(
    specs: &[NativeProcessSpec],
) -> Result<Vec<ProcessHandle>, PlatformError> {
    spawn_native_graph_with_closed_pipe_ends(specs, &[])
}

/// Launches a native process graph with explicitly parent-closed pipe ends.
///
/// Every pipe reader and writer must be represented exactly once, either by a
/// child-process endpoint or by a corresponding entry in `closed_ends`. A real
/// endpoint and closed marker for the same end, duplicate markers, unmatched
/// ends, and cycles are rejected before file-open or child-process side effects.
/// Pipes are still created for closed ends and all parent copies are dropped
/// after every child has spawned, producing EOF or broken-pipe behavior without
/// exposing internal handles.
pub fn spawn_native_graph_with_closed_pipe_ends(
    specs: &[NativeProcessSpec],
    closed_ends: &[ClosedProcessPipeEnd],
) -> Result<Vec<ProcessHandle>, PlatformError> {
    Ok(spawn_native_graph_with_parent_pipe_ends(specs, closed_ends, &[])?.into_processes())
}

/// Launches a native graph with parent-owned asynchronous pipe endpoints.
///
/// For every pipe, each reader and writer must be represented exactly once by
/// a child endpoint, a closed marker, or a parent marker. Duplicate or
/// conflicting ownership, unmatched ends, and cycles fail before file-open or
/// child-process side effects. Child-to-child edges remain direct OS pipe
/// connections; only endpoints listed in `parent_ends` are returned to the
/// caller.
pub fn spawn_native_graph_with_parent_pipe_ends(
    specs: &[NativeProcessSpec],
    closed_ends: &[ClosedProcessPipeEnd],
    parent_ends: &[ParentProcessPipeEnd],
) -> Result<NativeProcessGraph, PlatformError> {
    let file_order = default_process_file_order(specs);
    spawn_native_graph_with_parent_io(specs, closed_ends, parent_ends, &[], &file_order)
}

/// Launches a native graph with parent-owned pipe endpoints and files.
///
/// `file_order` must name every native and parent file exactly once. The graph,
/// every redirection plan, all parent file identifiers, and the complete order
/// are validated before any file is opened. Files are then opened in that exact
/// order before pipes are created or children are spawned. Only parent files and
/// pipe ends explicitly declared here are exposed by [`NativeProcessGraph`].
pub fn spawn_native_graph_with_parent_io(
    specs: &[NativeProcessSpec],
    closed_ends: &[ClosedProcessPipeEnd],
    parent_ends: &[ParentProcessPipeEnd],
    parent_files: &[ParentProcessFile],
    file_order: &[ProcessGraphFile],
) -> Result<NativeProcessGraph, PlatformError> {
    let graph = validate_process_graph_with_parent_pipe_ends(specs, closed_ends, parent_ends)?;
    for spec in specs {
        validate_process_redirections(spec)?;
    }
    validate_parent_process_files(parent_files)?;
    validate_process_file_order(specs, parent_files, file_order)?;

    let mut process_files: Vec<BTreeMap<ProcessFileId, File>> =
        std::iter::repeat_with(BTreeMap::new)
            .take(specs.len())
            .collect();
    let mut opened_parent_files = BTreeMap::new();
    for entry in file_order {
        match *entry {
            ProcessGraphFile::Process {
                process_index,
                file,
            } => {
                let endpoint = specs[process_index]
                    .files
                    .iter()
                    .find(|endpoint| endpoint.id == file)
                    .ok_or(PlatformError::InvalidProcessRedirection)?;
                process_files[process_index]
                    .insert(file, open_process_file(&endpoint.path, endpoint.mode)?);
            }
            ProcessGraphFile::Parent(id) => {
                let endpoint = parent_files
                    .iter()
                    .find(|endpoint| endpoint.id == id)
                    .ok_or(PlatformError::InvalidProcessRedirection)?;
                opened_parent_files.insert(id, open_process_file(&endpoint.path, endpoint.mode)?);
            }
        }
    }
    let mut prepared = specs
        .iter()
        .zip(process_files)
        .map(|(spec, files)| PreparedProcessStdio::with_files(spec, files))
        .collect::<Result<Vec<_>, _>>()?;
    let mut pipes = BTreeMap::new();
    for id in &graph.pipe_ids {
        pipes.insert(*id, std::io::pipe()?);
    }

    let mut processes: Vec<Option<ProcessHandle>> =
        std::iter::repeat_with(|| None).take(specs.len()).collect();
    for index in graph.spawn_order.into_iter().rev() {
        let spec = &specs[index];
        let stdin = prepared[index].input_stdio(spec.stdin, &pipes)?;
        let stdout = prepared[index].output_stdio(spec.stdout, &pipes)?;
        let stderr = prepared[index].output_stdio(spec.stderr, &pipes)?;
        let captures = prepared[index].take_captures();
        processes[index] = Some(spawn_native_with_stdio(
            spec, stdin, stdout, stderr, captures,
        )?);
    }
    drop(prepared);

    let processes = processes
        .into_iter()
        .map(|process| process.ok_or(PlatformError::InvalidProcessGraph))
        .collect::<Result<Vec<_>, _>>()?;
    let mut pipe_readers = BTreeMap::new();
    let mut pipe_writers = BTreeMap::new();
    for (id, (reader, writer)) in pipes {
        if graph.parent_readers.binary_search(&id).is_ok() {
            pipe_readers.insert(id, async_pipe_reader(reader));
        } else {
            drop(reader);
        }
        if graph.parent_writers.binary_search(&id).is_ok() {
            pipe_writers.insert(id, async_pipe_writer(writer));
        } else {
            drop(writer);
        }
    }
    Ok(NativeProcessGraph {
        processes,
        pipe_readers,
        pipe_writers,
        parent_files: opened_parent_files
            .into_iter()
            .map(|(id, file)| (id, tokio::fs::File::from_std(file)))
            .collect(),
    })
}

fn default_process_file_order(specs: &[NativeProcessSpec]) -> Vec<ProcessGraphFile> {
    specs
        .iter()
        .enumerate()
        .flat_map(|(process_index, spec)| {
            spec.files
                .iter()
                .map(move |endpoint| ProcessGraphFile::Process {
                    process_index,
                    file: endpoint.id,
                })
        })
        .collect()
}

fn validate_parent_process_files(parent_files: &[ParentProcessFile]) -> Result<(), PlatformError> {
    let mut ids = BTreeMap::new();
    for endpoint in parent_files {
        if ids.insert(endpoint.id, endpoint.mode).is_some() {
            return Err(PlatformError::InvalidProcessRedirection);
        }
    }
    Ok(())
}

fn validate_process_file_order(
    specs: &[NativeProcessSpec],
    parent_files: &[ParentProcessFile],
    file_order: &[ProcessGraphFile],
) -> Result<(), PlatformError> {
    let expected = default_process_file_order(specs)
        .into_iter()
        .chain(
            parent_files
                .iter()
                .map(|endpoint| ProcessGraphFile::Parent(endpoint.id)),
        )
        .collect::<std::collections::BTreeSet<_>>();
    let actual = file_order
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if expected.len() != file_order.len() || actual != expected {
        return Err(PlatformError::InvalidProcessRedirection);
    }
    Ok(())
}

fn spawn_native_with_stdio(
    spec: &NativeProcessSpec,
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
    captures: BTreeMap<ProcessCaptureId, tokio::fs::File>,
) -> Result<ProcessHandle, PlatformError> {
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.argv)
        .current_dir(&spec.cwd)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    if spec.clear_environment {
        command.env_clear();
    }
    for change in &spec.environment {
        match change {
            EnvironmentChange::Set(name, value) => {
                command.env(name, value);
            }
            EnvironmentChange::Remove(name) => {
                command.env_remove(name);
            }
        }
    }
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    Ok(ProcessHandle {
        child: command.spawn()?,
        captures,
    })
}

struct PreparedProcessStdio {
    files: BTreeMap<ProcessFileId, File>,
    capture_readers: BTreeMap<ProcessCaptureId, PipeReader>,
    capture_writers: BTreeMap<ProcessCaptureId, PipeWriter>,
}

impl PreparedProcessStdio {
    fn new(spec: &NativeProcessSpec) -> Result<Self, PlatformError> {
        validate_process_redirections(spec)?;
        if [spec.stdin, spec.stdout, spec.stderr]
            .into_iter()
            .any(|endpoint| matches!(endpoint, ProcessStdio::Pipe(_)))
        {
            return Err(PlatformError::InvalidProcessGraph);
        }
        Self::open(spec)
    }

    fn open(spec: &NativeProcessSpec) -> Result<Self, PlatformError> {
        let mut files = BTreeMap::new();
        for endpoint in &spec.files {
            files.insert(
                endpoint.id,
                open_process_file(&endpoint.path, endpoint.mode)?,
            );
        }

        Self::with_files(spec, files)
    }

    fn with_files(
        spec: &NativeProcessSpec,
        files: BTreeMap<ProcessFileId, File>,
    ) -> Result<Self, PlatformError> {
        let mut capture_readers = BTreeMap::new();
        let mut capture_writers = BTreeMap::new();
        for endpoint in [spec.stdout, spec.stderr] {
            if let ProcessStdio::Capture(id) = endpoint
                && !capture_readers.contains_key(&id)
            {
                let (reader, writer) = std::io::pipe()?;
                capture_readers.insert(id, reader);
                capture_writers.insert(id, writer);
            }
        }
        Ok(Self {
            files,
            capture_readers,
            capture_writers,
        })
    }

    fn input_stdio(
        &self,
        endpoint: ProcessStdio,
        pipes: &BTreeMap<ProcessPipeId, (PipeReader, PipeWriter)>,
    ) -> Result<Stdio, PlatformError> {
        match endpoint {
            ProcessStdio::Pipe(id) => pipes
                .get(&id)
                .ok_or(PlatformError::InvalidProcessGraph)?
                .0
                .try_clone()
                .map(Stdio::from)
                .map_err(PlatformError::from),
            ProcessStdio::File(id) => self.file_stdio(id),
            ProcessStdio::Capture(_) => Err(PlatformError::InvalidProcessRedirection),
            endpoint => endpoint.standalone_stdio(),
        }
    }

    fn output_stdio(
        &self,
        endpoint: ProcessStdio,
        pipes: &BTreeMap<ProcessPipeId, (PipeReader, PipeWriter)>,
    ) -> Result<Stdio, PlatformError> {
        match endpoint {
            ProcessStdio::Pipe(id) => pipes
                .get(&id)
                .ok_or(PlatformError::InvalidProcessGraph)?
                .1
                .try_clone()
                .map(Stdio::from)
                .map_err(PlatformError::from),
            ProcessStdio::File(id) => self.file_stdio(id),
            ProcessStdio::Capture(id) => self
                .capture_writers
                .get(&id)
                .ok_or(PlatformError::InvalidProcessRedirection)?
                .try_clone()
                .map(Stdio::from)
                .map_err(PlatformError::from),
            endpoint => endpoint.standalone_stdio(),
        }
    }

    fn file_stdio(&self, id: ProcessFileId) -> Result<Stdio, PlatformError> {
        self.files
            .get(&id)
            .ok_or(PlatformError::InvalidProcessRedirection)?
            .try_clone()
            .map(Stdio::from)
            .map_err(PlatformError::from)
    }

    fn take_captures(&mut self) -> BTreeMap<ProcessCaptureId, tokio::fs::File> {
        std::mem::take(&mut self.capture_readers)
            .into_iter()
            .map(|(id, reader)| (id, async_pipe_reader(reader)))
            .collect()
    }
}

fn open_process_file(path: &PathBuf, mode: NativeProcessFileMode) -> Result<File, PlatformError> {
    let mut options = OpenOptions::new();
    match mode {
        NativeProcessFileMode::Read => {
            options.read(true);
        }
        NativeProcessFileMode::Write => {
            options.write(true).create(true).truncate(true);
        }
        NativeProcessFileMode::Append => {
            options.write(true).create(true).append(true);
        }
    }
    options
        .open(path)
        .map_err(|source| PlatformError::ProcessRedirection {
            path: path.clone(),
            source,
        })
}

fn validate_process_redirections(spec: &NativeProcessSpec) -> Result<(), PlatformError> {
    let mut modes = BTreeMap::new();
    for endpoint in &spec.files {
        if modes.insert(endpoint.id, endpoint.mode).is_some() {
            return Err(PlatformError::InvalidProcessRedirection);
        }
    }
    match spec.stdin {
        ProcessStdio::File(id) if modes.get(&id).copied() == Some(NativeProcessFileMode::Read) => {}
        ProcessStdio::File(_) | ProcessStdio::Capture(_) => {
            return Err(PlatformError::InvalidProcessRedirection);
        }
        _ => {}
    }
    for endpoint in [spec.stdout, spec.stderr] {
        match endpoint {
            ProcessStdio::File(id)
                if matches!(
                    modes.get(&id),
                    Some(NativeProcessFileMode::Write | NativeProcessFileMode::Append)
                ) => {}
            ProcessStdio::File(_) => return Err(PlatformError::InvalidProcessRedirection),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn async_pipe_reader(reader: PipeReader) -> tokio::fs::File {
    let descriptor = OwnedFd::from(reader);
    tokio::fs::File::from_std(File::from(descriptor))
}

#[cfg(unix)]
fn async_pipe_writer(writer: PipeWriter) -> tokio::fs::File {
    let descriptor = OwnedFd::from(writer);
    tokio::fs::File::from_std(File::from(descriptor))
}

#[cfg(windows)]
fn async_pipe_writer(writer: PipeWriter) -> tokio::fs::File {
    let handle = OwnedHandle::from(writer);
    tokio::fs::File::from_std(File::from(handle))
}

#[cfg(not(any(unix, windows)))]
fn async_pipe_writer(writer: PipeWriter) -> tokio::fs::File {
    drop(writer);
    panic!("process task pipes require a supported native host")
}

#[cfg(windows)]
fn async_pipe_reader(reader: PipeReader) -> tokio::fs::File {
    let handle = OwnedHandle::from(reader);
    tokio::fs::File::from_std(File::from(handle))
}

#[cfg(not(any(unix, windows)))]
fn async_pipe_reader(reader: PipeReader) -> tokio::fs::File {
    drop(reader);
    panic!("process capture pipes require a supported native host")
}

struct ProcessGraph {
    pipe_ids: Vec<ProcessPipeId>,
    parent_readers: Vec<ProcessPipeId>,
    parent_writers: Vec<ProcessPipeId>,
    spawn_order: Vec<usize>,
}

#[derive(Default)]
struct PipeUsage {
    reader: Option<usize>,
    writer: Option<usize>,
    reader_closed: bool,
    writer_closed: bool,
    reader_parent: bool,
    writer_parent: bool,
}

#[cfg(test)]
fn validate_process_graph(specs: &[NativeProcessSpec]) -> Result<ProcessGraph, PlatformError> {
    validate_process_graph_with_parent_pipe_ends(specs, &[], &[])
}

#[cfg(test)]
fn validate_process_graph_with_closed_pipe_ends(
    specs: &[NativeProcessSpec],
    closed_ends: &[ClosedProcessPipeEnd],
) -> Result<ProcessGraph, PlatformError> {
    validate_process_graph_with_parent_pipe_ends(specs, closed_ends, &[])
}

fn validate_process_graph_with_parent_pipe_ends(
    specs: &[NativeProcessSpec],
    closed_ends: &[ClosedProcessPipeEnd],
    parent_ends: &[ParentProcessPipeEnd],
) -> Result<ProcessGraph, PlatformError> {
    let mut usages = BTreeMap::<ProcessPipeId, PipeUsage>::new();
    for (index, spec) in specs.iter().enumerate() {
        if let ProcessStdio::Pipe(id) = spec.stdin {
            let usage = usages.entry(id).or_default();
            if usage.reader.replace(index).is_some() {
                return Err(PlatformError::InvalidProcessGraph);
            }
        }
        for output in [spec.stdout, spec.stderr] {
            if let ProcessStdio::Pipe(id) = output {
                let usage = usages.entry(id).or_default();
                if usage.writer.is_some_and(|writer| writer != index) {
                    return Err(PlatformError::InvalidProcessGraph);
                }
                usage.writer = Some(index);
            }
        }
    }

    for closed_end in closed_ends {
        match *closed_end {
            ClosedProcessPipeEnd::Reader(id) => {
                let usage = usages.entry(id).or_default();
                if usage.reader.is_some() || std::mem::replace(&mut usage.reader_closed, true) {
                    return Err(PlatformError::InvalidProcessGraph);
                }
            }
            ClosedProcessPipeEnd::Writer(id) => {
                let usage = usages.entry(id).or_default();
                if usage.writer.is_some() || std::mem::replace(&mut usage.writer_closed, true) {
                    return Err(PlatformError::InvalidProcessGraph);
                }
            }
        }
    }

    for parent_end in parent_ends {
        match *parent_end {
            ParentProcessPipeEnd::Reader(id) => {
                let usage = usages.entry(id).or_default();
                if usage.reader.is_some()
                    || usage.reader_closed
                    || std::mem::replace(&mut usage.reader_parent, true)
                {
                    return Err(PlatformError::InvalidProcessGraph);
                }
            }
            ParentProcessPipeEnd::Writer(id) => {
                let usage = usages.entry(id).or_default();
                if usage.writer.is_some()
                    || usage.writer_closed
                    || std::mem::replace(&mut usage.writer_parent, true)
                {
                    return Err(PlatformError::InvalidProcessGraph);
                }
            }
        }
    }

    let mut outgoing = vec![Vec::new(); specs.len()];
    let mut incoming = vec![0_usize; specs.len()];
    for usage in usages.values() {
        let readers = usize::from(usage.reader.is_some())
            + usize::from(usage.reader_closed)
            + usize::from(usage.reader_parent);
        let writers = usize::from(usage.writer.is_some())
            + usize::from(usage.writer_closed)
            + usize::from(usage.writer_parent);
        if readers != 1 || writers != 1 {
            return Err(PlatformError::InvalidProcessGraph);
        }
        if let (Some(writer), Some(reader)) = (usage.writer, usage.reader) {
            if writer == reader {
                return Err(PlatformError::InvalidProcessGraph);
            }
            outgoing[writer].push(reader);
            incoming[reader] = incoming[reader]
                .checked_add(1)
                .ok_or(PlatformError::InvalidProcessGraph)?;
        }
    }

    let mut ready = incoming
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect::<VecDeque<_>>();
    let mut spawn_order = Vec::with_capacity(specs.len());
    while let Some(index) = ready.pop_front() {
        spawn_order.push(index);
        for &next in &outgoing[index] {
            incoming[next] = incoming[next]
                .checked_sub(1)
                .ok_or(PlatformError::InvalidProcessGraph)?;
            if incoming[next] == 0 {
                ready.push_back(next);
            }
        }
    }
    if spawn_order.len() != specs.len() {
        return Err(PlatformError::InvalidProcessGraph);
    }

    let pipe_ids = usages.keys().copied().collect();
    let parent_readers = usages
        .iter()
        .filter_map(|(id, usage)| usage.reader_parent.then_some(*id))
        .collect();
    let parent_writers = usages
        .iter()
        .filter_map(|(id, usage)| usage.writer_parent.then_some(*id))
        .collect();
    Ok(ProcessGraph {
        pipe_ids,
        parent_readers,
        parent_writers,
        spawn_order,
    })
}

fn validate_process_spec(spec: &ProcessSpec) -> Result<(), PlatformError> {
    if spec.executable.contains('\0') || spec.argv.iter().any(|argument| argument.contains('\0')) {
        return Err(PlatformError::InvalidLogicalPath);
    }
    for change in &spec.environment {
        let (name, value) = match change {
            EnvironmentChange::Set(name, value) => (name, Some(value)),
            EnvironmentChange::Remove(name) => (name, None),
        };
        let Some(name) = name.to_str() else {
            return Err(PlatformError::InvalidEnvironment);
        };
        let mut bytes = name.bytes();
        if !matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || value.is_some_and(|value| value.to_string_lossy().contains('\0'))
        {
            return Err(PlatformError::InvalidEnvironment);
        }
    }
    Ok(())
}

impl ProcessHandle {
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin().take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout().take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr().take()
    }

    /// Takes one parent-facing capture pipe by its plan-local identifier.
    pub fn take_capture(&mut self, id: ProcessCaptureId) -> Option<tokio::fs::File> {
        self.captures.remove(&id)
    }

    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub async fn wait(&mut self) -> Result<ProcessExit, PlatformError> {
        let status = self.child.wait().await?;
        Ok(normalize_exit(status))
    }

    pub async fn terminate(&mut self) -> Result<(), PlatformError> {
        match Box::into_pin(self.child.kill()).await {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::InvalidInput | std::io::ErrorKind::NotFound
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // `start_kill` is overridden by the Unix process-group and Windows
        // Job Object wrappers, so aborting an async request cannot orphan its
        // descendants even though Drop itself cannot await reaping.
        let _ = self.child.start_kill();
    }
}

#[cfg(unix)]
fn normalize_exit(status: std::process::ExitStatus) -> ProcessExit {
    use std::os::unix::process::ExitStatusExt;

    ProcessExit {
        success: status.success(),
        code: status.code().map(i64::from),
        signal: status.signal().map(i64::from),
    }
}

#[cfg(windows)]
fn normalize_exit(status: std::process::ExitStatus) -> ProcessExit {
    ProcessExit {
        success: status.success(),
        code: status.code().map(i64::from),
        signal: None,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{
        ClosedProcessPipeEnd, EnvironmentChange, NativeProcessSpec, ParentProcessPipeEnd,
        ProcessGraphFile, ProcessPipeId, ProcessSpec, ProcessStdio, spawn_native,
        spawn_native_graph, spawn_native_graph_with_closed_pipe_ends,
        spawn_native_graph_with_parent_io, spawn_native_graph_with_parent_pipe_ends,
        validate_process_graph, validate_process_graph_with_closed_pipe_ends,
        validate_process_graph_with_parent_pipe_ends,
    };
    use crate::{PlatformError, Workspace};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-process-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn compile_process_tree_helper(directory: &TestDirectory) -> String {
        let bin_directory = directory.0.join("bin");
        fs::create_dir(&bin_directory).expect("create bin directory");
        let source = directory.0.join("process-tree-helper.rs");
        fs::write(
            &source,
            r#"
use std::{convert::TryFrom, env, fs, io, io::Write, process::Command, thread, time::Duration};

fn main() {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        Some("inspect") => {
            let argument = arguments.next().expect("argument");
            let cwd = env::current_dir().expect("current directory");
            println!("cwd={}", cwd.file_name().expect("directory name").to_string_lossy());
            println!("token={}", env::var("ASH_NATIVE_TOKEN").expect("environment"));
            println!("argument={argument}");
            eprintln!("native-stderr");
        }
        Some("copy") => {
            let mut input = io::stdin().lock();
            let mut output = io::stdout().lock();
            io::copy(&mut input, &mut output).expect("copy stdin to stdout");
            output.flush().expect("flush stdout");
        }
        Some("produce") => {
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
                output
                    .write_all(&buffer[..count])
                    .expect("write generated stdout");
                offset += count;
            }
            output.flush().expect("flush generated stdout");
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
        Some("exit") => {
            let code = arguments
                .next()
                .expect("exit code")
                .parse::<i32>()
                .expect("numeric exit code");
            std::process::exit(code);
        }
        Some("quiet") => {}
        Some("parent") => {
            let ready = arguments.next().expect("ready path");
            let escaped = arguments.next().expect("escaped path");
            let _child = Command::new(env::current_exe().expect("current executable"))
                .arg("child")
                .arg(escaped)
                .spawn()
                .expect("spawn descendant");
            fs::write(ready, b"ready").expect("write ready marker");
            thread::sleep(Duration::from_secs(10));
        }
        Some("child") => {
            let escaped = arguments.next().expect("escaped path");
            thread::sleep(Duration::from_secs(1));
            fs::write(escaped, b"escaped").expect("write escaped marker");
            thread::sleep(Duration::from_secs(10));
        }
        _ => panic!("unknown helper mode"),
    }
}
"#,
        )
        .expect("write helper source");
        let executable_name = if cfg!(windows) {
            "process-tree-helper.exe"
        } else {
            "process-tree-helper"
        };
        let executable = bin_directory.join(executable_name);
        let status = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .expect("run rustc");
        assert!(status.success(), "compile process-tree helper");
        format!("bin/{executable_name}")
    }

    #[tokio::test]
    async fn native_process_spec_preserves_cwd_environment_and_argv() {
        let directory = TestDirectory::new();
        let executable = directory.0.join(compile_process_tree_helper(&directory));
        let cwd = directory.0.join("native-cwd");
        fs::create_dir(&cwd).expect("native cwd");
        let mut process = spawn_native(&NativeProcessSpec {
            executable: executable.into_os_string(),
            argv: vec![OsString::from("inspect"), OsString::from("alpha beta")],
            cwd,
            environment: vec![EnvironmentChange::Set(
                OsString::from("ASH_NATIVE_TOKEN"),
                OsString::from("present"),
            )],
            clear_environment: true,
            files: Vec::new(),
            stdin: ProcessStdio::Null,
            stdout: ProcessStdio::Piped,
            stderr: ProcessStdio::Piped,
        })
        .expect("spawn native helper");
        let mut stdout = process.take_stdout().expect("stdout");
        let mut stderr = process.take_stderr().expect("stderr");
        let (stdout, stderr, exit) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.expect("stdout");
                bytes
            },
            async {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.expect("stderr");
                bytes
            },
            process.wait(),
        );

        assert!(exit.expect("wait").success, "stderr={stderr:?}");
        assert_eq!(
            stdout,
            b"cwd=native-cwd\ntoken=present\nargument=alpha beta\n"
        );
        assert_eq!(stderr, b"native-stderr\n");
    }

    #[tokio::test]
    async fn explicit_stdio_endpoints_stream_and_expose_only_piped_handles() {
        let directory = TestDirectory::new();
        let executable = directory.0.join(compile_process_tree_helper(&directory));
        let mut process = spawn_native(&NativeProcessSpec {
            executable: executable.clone().into_os_string(),
            argv: vec![OsString::from("copy")],
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            files: Vec::new(),
            stdin: ProcessStdio::Piped,
            stdout: ProcessStdio::Piped,
            stderr: ProcessStdio::Null,
        })
        .expect("spawn streaming helper");
        let mut stdin = process.take_stdin().expect("piped stdin");
        let mut stdout = process.take_stdout().expect("piped stdout");
        assert!(process.take_stderr().is_none(), "null stderr has no handle");
        let expected: Vec<u8> = (0..8 * 1024 * 1024)
            .map(|index| u8::try_from(index % 251).expect("bounded byte"))
            .collect();
        let input = expected.clone();

        let writer = async move {
            stdin.write_all(&input).await.expect("write stdin");
            stdin.shutdown().await.expect("close stdin");
            drop(stdin);
        };
        let reader = async {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.expect("read stdout");
            bytes
        };
        let ((), actual, exit) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(writer, reader, process.wait())
        })
        .await
        .expect("streaming helper completes without deadlock");
        assert!(exit.expect("wait for streaming helper").success);
        assert_eq!(actual.len(), expected.len());
        assert!(actual == expected, "streaming payload changed");

        let mut inherited = spawn_native(&NativeProcessSpec {
            executable: executable.into_os_string(),
            argv: vec![OsString::from("quiet")],
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            files: Vec::new(),
            stdin: ProcessStdio::Inherit,
            stdout: ProcessStdio::Inherit,
            stderr: ProcessStdio::Inherit,
        })
        .expect("spawn inherited-stdio helper");
        assert!(inherited.take_stdin().is_none());
        assert!(inherited.take_stdout().is_none());
        assert!(inherited.take_stderr().is_none());
        assert!(
            inherited
                .wait()
                .await
                .expect("wait for quiet helper")
                .success
        );
    }

    #[tokio::test]
    async fn native_file_and_capture_endpoints_are_ordered_and_shared() {
        let directory = TestDirectory::new();
        let executable = directory.0.join(compile_process_tree_helper(&directory));
        let first = directory.0.join("first.log");
        let shared = directory.0.join("shared.log");
        fs::write(&first, b"stale").expect("seed first target");
        let first_id = super::ProcessFileId::new(1);
        let shared_id = super::ProcessFileId::new(2);
        let mut redirected = spawn_native(&NativeProcessSpec {
            executable: executable.clone().into_os_string(),
            argv: vec![OsString::from("ordered")],
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            files: vec![
                super::NativeProcessFile {
                    id: first_id,
                    path: first.clone(),
                    mode: super::NativeProcessFileMode::Write,
                },
                super::NativeProcessFile {
                    id: shared_id,
                    path: shared.clone(),
                    mode: super::NativeProcessFileMode::Write,
                },
            ],
            stdin: ProcessStdio::Null,
            stdout: ProcessStdio::File(shared_id),
            stderr: ProcessStdio::File(shared_id),
        })
        .expect("spawn file-redirected helper");
        assert!(
            redirected
                .wait()
                .await
                .expect("wait for redirected helper")
                .success
        );
        assert_eq!(fs::read(first).expect("read first target"), b"");
        assert_eq!(
            fs::read(shared).expect("read shared target"),
            b"stdout-a\nstderr-a\nstdout-b\nstderr-b\n"
        );

        let capture_id = super::ProcessCaptureId::new(9);
        let mut captured = spawn_native(&NativeProcessSpec {
            executable: executable.clone().into_os_string(),
            argv: vec![OsString::from("ordered")],
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            files: Vec::new(),
            stdin: ProcessStdio::Null,
            stdout: ProcessStdio::Capture(capture_id),
            stderr: ProcessStdio::Capture(capture_id),
        })
        .expect("spawn shared-capture helper");
        let mut capture = captured.take_capture(capture_id).expect("shared capture");
        let (bytes, exit) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                capture
                    .read_to_end(&mut bytes)
                    .await
                    .expect("read shared capture");
                bytes
            },
            captured.wait(),
        );
        assert!(exit.expect("wait for captured helper").success);
        assert_eq!(bytes, b"stdout-a\nstderr-a\nstdout-b\nstderr-b\n");

        let input = directory.0.join("input.bin");
        fs::write(&input, b"redirected-input").expect("write input fixture");
        let input_id = super::ProcessFileId::new(3);
        let output_id = super::ProcessCaptureId::new(10);
        let mut copied = spawn_native(&NativeProcessSpec {
            executable: executable.into_os_string(),
            argv: vec![OsString::from("copy")],
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            files: vec![super::NativeProcessFile {
                id: input_id,
                path: input,
                mode: super::NativeProcessFileMode::Read,
            }],
            stdin: ProcessStdio::File(input_id),
            stdout: ProcessStdio::Capture(output_id),
            stderr: ProcessStdio::Null,
        })
        .expect("spawn input-redirected helper");
        let mut output = copied.take_capture(output_id).expect("copy output capture");
        let (bytes, exit) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                output
                    .read_to_end(&mut bytes)
                    .await
                    .expect("read copied input");
                bytes
            },
            copied.wait(),
        );
        assert!(exit.expect("wait for copied input").success);
        assert_eq!(bytes, b"redirected-input");
    }

    #[test]
    fn connected_stdio_requires_a_complete_acyclic_process_graph() {
        let first_pipe = ProcessPipeId::new(1);
        let second_pipe = ProcessPipeId::new(2);
        let spec = |stdin, stdout| NativeProcessSpec {
            executable: OsString::from("missing-process"),
            argv: vec![],
            cwd: PathBuf::from("."),
            environment: vec![],
            clear_environment: false,
            files: Vec::new(),
            stdin,
            stdout,
            stderr: ProcessStdio::Null,
        };

        assert!(matches!(
            spawn_native(&spec(ProcessStdio::Pipe(first_pipe), ProcessStdio::Null)),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(matches!(
            spawn_native(&spec(
                ProcessStdio::Capture(super::ProcessCaptureId::new(1)),
                ProcessStdio::Null
            )),
            Err(PlatformError::InvalidProcessRedirection)
        ));
        assert!(matches!(
            spawn_native(&spec(
                ProcessStdio::Null,
                ProcessStdio::File(super::ProcessFileId::new(1))
            )),
            Err(PlatformError::InvalidProcessRedirection)
        ));

        let standalone_directory = TestDirectory::new();
        let unopened_standalone = standalone_directory.0.join("unopened-standalone.log");
        let standalone_file_id = super::ProcessFileId::new(10);
        let mut invalid_standalone = spec(
            ProcessStdio::Pipe(first_pipe),
            ProcessStdio::File(standalone_file_id),
        );
        invalid_standalone.files.push(super::NativeProcessFile {
            id: standalone_file_id,
            path: unopened_standalone.clone(),
            mode: super::NativeProcessFileMode::Write,
        });
        assert!(matches!(
            spawn_native(&invalid_standalone),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(
            !unopened_standalone.exists(),
            "standalone graph endpoints must fail before file-open side effects"
        );

        assert!(matches!(
            spawn_native_graph(&[spec(ProcessStdio::Null, ProcessStdio::Pipe(first_pipe))]),
            Err(PlatformError::InvalidProcessGraph)
        ));

        let writer_only = [spec(ProcessStdio::Null, ProcessStdio::Pipe(first_pipe))];
        let graph = validate_process_graph_with_closed_pipe_ends(
            &writer_only,
            &[ClosedProcessPipeEnd::Reader(first_pipe)],
        )
        .expect("a parent-closed reader completes a writer-only pipe");
        assert_eq!(graph.pipe_ids, vec![first_pipe]);
        assert_eq!(graph.spawn_order, vec![0]);

        let reader_only = [spec(ProcessStdio::Pipe(first_pipe), ProcessStdio::Null)];
        validate_process_graph_with_closed_pipe_ends(
            &reader_only,
            &[ClosedProcessPipeEnd::Writer(first_pipe)],
        )
        .expect("a parent-closed writer completes a reader-only pipe");
        let closed_graph = validate_process_graph_with_closed_pipe_ends(
            &[],
            &[
                ClosedProcessPipeEnd::Reader(second_pipe),
                ClosedProcessPipeEnd::Writer(second_pipe),
            ],
        )
        .expect("both ends may be explicitly parent-closed");
        assert_eq!(closed_graph.pipe_ids, vec![second_pipe]);
        assert!(closed_graph.spawn_order.is_empty());

        let parent_reader_graph = validate_process_graph_with_parent_pipe_ends(
            &writer_only,
            &[],
            &[ParentProcessPipeEnd::Reader(first_pipe)],
        )
        .expect("a parent reader completes a child-writer pipe");
        assert_eq!(parent_reader_graph.parent_readers, vec![first_pipe]);
        assert!(parent_reader_graph.parent_writers.is_empty());
        let parent_writer_graph = validate_process_graph_with_parent_pipe_ends(
            &reader_only,
            &[],
            &[ParentProcessPipeEnd::Writer(first_pipe)],
        )
        .expect("a parent writer completes a child-reader pipe");
        assert!(parent_writer_graph.parent_readers.is_empty());
        assert_eq!(parent_writer_graph.parent_writers, vec![first_pipe]);
        let parent_graph = validate_process_graph_with_parent_pipe_ends(
            &[],
            &[],
            &[
                ParentProcessPipeEnd::Reader(second_pipe),
                ParentProcessPipeEnd::Writer(second_pipe),
            ],
        )
        .expect("both pipe ends may belong to parent tasks");
        assert_eq!(parent_graph.pipe_ids, vec![second_pipe]);
        assert_eq!(parent_graph.parent_readers, vec![second_pipe]);
        assert_eq!(parent_graph.parent_writers, vec![second_pipe]);

        for parent_ends in [
            vec![ParentProcessPipeEnd::Reader(first_pipe)],
            vec![
                ParentProcessPipeEnd::Reader(first_pipe),
                ParentProcessPipeEnd::Reader(first_pipe),
                ParentProcessPipeEnd::Writer(first_pipe),
            ],
        ] {
            assert!(matches!(
                validate_process_graph_with_parent_pipe_ends(&[], &[], &parent_ends),
                Err(PlatformError::InvalidProcessGraph)
            ));
        }
        assert!(matches!(
            validate_process_graph_with_parent_pipe_ends(
                &writer_only,
                &[ClosedProcessPipeEnd::Reader(first_pipe)],
                &[ParentProcessPipeEnd::Reader(first_pipe)],
            ),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(matches!(
            validate_process_graph_with_parent_pipe_ends(
                &reader_only,
                &[],
                &[
                    ParentProcessPipeEnd::Reader(first_pipe),
                    ParentProcessPipeEnd::Writer(first_pipe),
                ],
            ),
            Err(PlatformError::InvalidProcessGraph)
        ));

        for closed_ends in [
            vec![ClosedProcessPipeEnd::Reader(first_pipe)],
            vec![
                ClosedProcessPipeEnd::Reader(first_pipe),
                ClosedProcessPipeEnd::Reader(first_pipe),
                ClosedProcessPipeEnd::Writer(first_pipe),
            ],
        ] {
            assert!(matches!(
                validate_process_graph_with_closed_pipe_ends(&[], &closed_ends),
                Err(PlatformError::InvalidProcessGraph)
            ));
        }
        assert!(matches!(
            validate_process_graph_with_closed_pipe_ends(
                &writer_only,
                &[
                    ClosedProcessPipeEnd::Writer(first_pipe),
                    ClosedProcessPipeEnd::Reader(first_pipe),
                ],
            ),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(matches!(
            validate_process_graph_with_closed_pipe_ends(
                &reader_only,
                &[
                    ClosedProcessPipeEnd::Reader(first_pipe),
                    ClosedProcessPipeEnd::Writer(first_pipe),
                ],
            ),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(matches!(
            spawn_native_graph(&[
                spec(ProcessStdio::Null, ProcessStdio::Pipe(first_pipe)),
                spec(ProcessStdio::Pipe(first_pipe), ProcessStdio::Null),
                spec(ProcessStdio::Pipe(first_pipe), ProcessStdio::Null),
            ]),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(matches!(
            spawn_native_graph(&[
                spec(ProcessStdio::Null, ProcessStdio::Pipe(first_pipe)),
                spec(ProcessStdio::Null, ProcessStdio::Pipe(first_pipe)),
                spec(ProcessStdio::Pipe(first_pipe), ProcessStdio::Null),
            ]),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(matches!(
            spawn_native_graph(&[spec(
                ProcessStdio::Pipe(first_pipe),
                ProcessStdio::Pipe(first_pipe)
            )]),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(matches!(
            spawn_native_graph(&[
                spec(
                    ProcessStdio::Pipe(second_pipe),
                    ProcessStdio::Pipe(first_pipe),
                ),
                spec(
                    ProcessStdio::Pipe(first_pipe),
                    ProcessStdio::Pipe(second_pipe),
                ),
            ]),
            Err(PlatformError::InvalidProcessGraph)
        ));

        let directory = TestDirectory::new();
        let unopened = directory.0.join("unopened.log");
        let file_id = super::ProcessFileId::new(11);
        let mut file_spec = spec(ProcessStdio::Null, ProcessStdio::File(file_id));
        file_spec.files.push(super::NativeProcessFile {
            id: file_id,
            path: unopened.clone(),
            mode: super::NativeProcessFileMode::Write,
        });
        assert!(matches!(
            spawn_native_graph(&[
                file_spec,
                spec(
                    ProcessStdio::Capture(super::ProcessCaptureId::new(12)),
                    ProcessStdio::Null,
                ),
            ]),
            Err(PlatformError::InvalidProcessRedirection)
        ));
        assert!(
            !unopened.exists(),
            "the complete graph plan must validate before file-open side effects"
        );

        let unopened_closed = directory.0.join("unopened-closed.log");
        let mut invalid_closed_spec = spec(ProcessStdio::Null, ProcessStdio::Pipe(first_pipe));
        invalid_closed_spec.files.push(super::NativeProcessFile {
            id: super::ProcessFileId::new(13),
            path: unopened_closed.clone(),
            mode: super::NativeProcessFileMode::Write,
        });
        assert!(matches!(
            spawn_native_graph_with_closed_pipe_ends(
                &[invalid_closed_spec],
                &[
                    ClosedProcessPipeEnd::Writer(first_pipe),
                    ClosedProcessPipeEnd::Reader(first_pipe),
                ],
            ),
            Err(PlatformError::InvalidProcessGraph)
        ));
        assert!(
            !unopened_closed.exists(),
            "closed-end conflicts must fail before file-open side effects"
        );

        let mut shared_writer = spec(ProcessStdio::Null, ProcessStdio::Pipe(first_pipe));
        shared_writer.stderr = ProcessStdio::Pipe(first_pipe);
        let graph = validate_process_graph(&[
            shared_writer,
            spec(ProcessStdio::Pipe(first_pipe), ProcessStdio::Null),
        ])
        .expect("one process may share one writer across stdout and stderr");
        assert_eq!(graph.spawn_order, vec![0, 1]);
    }

    #[test]
    fn parent_file_plans_validate_before_source_ordered_open_side_effects() {
        let directory = TestDirectory::new();
        let parent_id = super::ParentProcessFileId::new(1);
        let native_id = super::ProcessFileId::new(2);
        let missing = directory.0.join("missing-input");
        let native = NativeProcessSpec {
            executable: OsString::from("unreachable-process"),
            argv: Vec::new(),
            cwd: directory.0.clone(),
            environment: Vec::new(),
            clear_environment: false,
            files: vec![super::NativeProcessFile {
                id: native_id,
                path: missing,
                mode: super::NativeProcessFileMode::Read,
            }],
            stdin: ProcessStdio::File(native_id),
            stdout: ProcessStdio::Null,
            stderr: ProcessStdio::Null,
        };

        let invalid_path = directory.0.join("invalid-order.log");
        let invalid_parent = super::ParentProcessFile {
            id: parent_id,
            path: invalid_path.clone(),
            mode: super::NativeProcessFileMode::Write,
        };
        assert!(matches!(
            spawn_native_graph_with_parent_io(
                &[],
                &[],
                &[],
                std::slice::from_ref(&invalid_parent),
                &[],
            ),
            Err(PlatformError::InvalidProcessRedirection)
        ));
        assert!(
            !invalid_path.exists(),
            "an incomplete global order must fail before opening a parent file"
        );

        let duplicate_path = directory.0.join("duplicate-parent.log");
        let duplicate_parent = super::ParentProcessFile {
            id: parent_id,
            path: duplicate_path.clone(),
            mode: super::NativeProcessFileMode::Write,
        };
        assert!(matches!(
            spawn_native_graph_with_parent_io(
                &[],
                &[],
                &[],
                &[invalid_parent, duplicate_parent],
                &[ProcessGraphFile::Parent(parent_id)],
            ),
            Err(PlatformError::InvalidProcessRedirection)
        ));
        assert!(
            !duplicate_path.exists(),
            "duplicate parent file identifiers must fail before open"
        );

        let before_path = directory.0.join("opened-before-failure.log");
        let before_parent = super::ParentProcessFile {
            id: parent_id,
            path: before_path.clone(),
            mode: super::NativeProcessFileMode::Write,
        };
        assert!(matches!(
            spawn_native_graph_with_parent_io(
                std::slice::from_ref(&native),
                &[],
                &[],
                &[before_parent],
                &[
                    ProcessGraphFile::Parent(parent_id),
                    ProcessGraphFile::Process {
                        process_index: 0,
                        file: native_id,
                    },
                ],
            ),
            Err(PlatformError::ProcessRedirection { .. })
        ));
        assert!(
            before_path.exists(),
            "a parent file ordered before a failing native file must be created"
        );

        let after_path = directory.0.join("not-opened-after-failure.log");
        let after_parent = super::ParentProcessFile {
            id: parent_id,
            path: after_path.clone(),
            mode: super::NativeProcessFileMode::Write,
        };
        assert!(matches!(
            spawn_native_graph_with_parent_io(
                &[native],
                &[],
                &[],
                &[after_parent],
                &[
                    ProcessGraphFile::Process {
                        process_index: 0,
                        file: native_id,
                    },
                    ProcessGraphFile::Parent(parent_id),
                ],
            ),
            Err(PlatformError::ProcessRedirection { .. })
        ));
        assert!(
            !after_path.exists(),
            "a parent file ordered after a failing native file must stay unopened"
        );
    }

    #[tokio::test]
    async fn parent_owned_graph_files_expose_async_handles_once() {
        let directory = TestDirectory::new();
        let input_path = directory.0.join("input.bin");
        let output_path = directory.0.join("output.bin");
        fs::write(&input_path, b"parent-file-input").expect("write parent input");
        let input_id = super::ParentProcessFileId::new(3);
        let output_id = super::ParentProcessFileId::new(4);
        let mut graph = spawn_native_graph_with_parent_io(
            &[],
            &[],
            &[],
            &[
                super::ParentProcessFile {
                    id: input_id,
                    path: input_path,
                    mode: super::NativeProcessFileMode::Read,
                },
                super::ParentProcessFile {
                    id: output_id,
                    path: output_path.clone(),
                    mode: super::NativeProcessFileMode::Write,
                },
            ],
            &[
                ProcessGraphFile::Parent(input_id),
                ProcessGraphFile::Parent(output_id),
            ],
        )
        .expect("open parent-owned graph files");
        let mut input = graph.take_parent_file(input_id).expect("parent input file");
        let mut output = graph
            .take_parent_file(output_id)
            .expect("parent output file");
        assert!(graph.take_parent_file(input_id).is_none());
        assert!(graph.take_parent_file(output_id).is_none());
        assert!(graph.into_processes().is_empty());

        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .await
            .expect("read parent input file");
        output
            .write_all(&bytes)
            .await
            .expect("write parent output file");
        output.shutdown().await.expect("close parent output file");
        drop(output);
        assert_eq!(bytes, b"parent-file-input");
        assert_eq!(
            fs::read(output_path).expect("read parent output fixture"),
            b"parent-file-input"
        );
    }

    #[tokio::test]
    async fn native_pipe_graph_streams_without_exposing_internal_handles() {
        const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

        let directory = TestDirectory::new();
        let executable = directory.0.join(compile_process_tree_helper(&directory));
        let pipe = ProcessPipeId::new(7);
        let mut processes = spawn_native_graph(&[
            NativeProcessSpec {
                executable: executable.clone().into_os_string(),
                argv: vec![
                    OsString::from("produce"),
                    OsString::from(PAYLOAD_BYTES.to_string()),
                ],
                cwd: directory.0.clone(),
                environment: vec![],
                clear_environment: false,
                files: Vec::new(),
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Pipe(pipe),
                stderr: ProcessStdio::Null,
            },
            NativeProcessSpec {
                executable: executable.clone().into_os_string(),
                argv: vec![OsString::from("copy")],
                cwd: directory.0.clone(),
                environment: vec![],
                clear_environment: false,
                files: Vec::new(),
                stdin: ProcessStdio::Pipe(pipe),
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Null,
            },
        ])
        .expect("spawn native pipe graph");
        let mut consumer = processes.pop().expect("consumer process");
        let mut producer = processes.pop().expect("producer process");
        assert!(processes.is_empty());
        assert!(producer.take_stdin().is_none());
        assert!(producer.take_stdout().is_none());
        assert!(producer.take_stderr().is_none());
        assert!(consumer.take_stdin().is_none());
        let mut stdout = consumer.take_stdout().expect("parent-facing stdout");
        assert!(consumer.take_stderr().is_none());

        let reader = async move {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.expect("read pipeline");
            bytes
        };
        let (actual, producer_exit, consumer_exit) =
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::join!(reader, producer.wait(), consumer.wait())
            })
            .await
            .expect("native pipe graph completes without deadlock");
        assert!(producer_exit.expect("wait for producer").success);
        assert!(consumer_exit.expect("wait for consumer").success);

        let expected: Vec<u8> = (0..PAYLOAD_BYTES)
            .map(|index| u8::try_from(index % 251).expect("bounded byte"))
            .collect();
        assert_eq!(actual.len(), expected.len());
        assert!(actual == expected, "native pipe graph changed the payload");

        let parent_reader_pipe = ProcessPipeId::new(11);
        let mut parent_reader_graph = spawn_native_graph_with_parent_pipe_ends(
            &[NativeProcessSpec {
                executable: executable.clone().into_os_string(),
                argv: vec![
                    OsString::from("produce"),
                    OsString::from(PAYLOAD_BYTES.to_string()),
                ],
                cwd: directory.0.clone(),
                environment: vec![],
                clear_environment: false,
                files: Vec::new(),
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Pipe(parent_reader_pipe),
                stderr: ProcessStdio::Null,
            }],
            &[],
            &[ParentProcessPipeEnd::Reader(parent_reader_pipe)],
        )
        .expect("spawn child writer with a parent reader");
        let mut parent_reader = parent_reader_graph
            .take_pipe_reader(parent_reader_pipe)
            .expect("parent reader");
        assert!(
            parent_reader_graph
                .take_pipe_reader(parent_reader_pipe)
                .is_none()
        );
        assert!(
            parent_reader_graph
                .take_pipe_writer(parent_reader_pipe)
                .is_none()
        );
        let mut parent_reader_processes = parent_reader_graph.into_processes();
        let mut parent_reader_producer = parent_reader_processes.pop().expect("parent producer");
        assert!(parent_reader_processes.is_empty());
        let (parent_bytes, parent_reader_exit) =
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::join!(
                    async {
                        let mut bytes = Vec::new();
                        parent_reader
                            .read_to_end(&mut bytes)
                            .await
                            .expect("read parent endpoint");
                        bytes
                    },
                    parent_reader_producer.wait(),
                )
            })
            .await
            .expect("parent reader drains with backpressure");
        assert!(parent_reader_exit.expect("wait parent producer").success);
        assert_eq!(parent_bytes, expected);

        let parent_writer_pipe = ProcessPipeId::new(12);
        let mut parent_writer_graph = spawn_native_graph_with_parent_pipe_ends(
            &[NativeProcessSpec {
                executable: executable.clone().into_os_string(),
                argv: vec![OsString::from("copy")],
                cwd: directory.0.clone(),
                environment: vec![],
                clear_environment: false,
                files: Vec::new(),
                stdin: ProcessStdio::Pipe(parent_writer_pipe),
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Null,
            }],
            &[],
            &[ParentProcessPipeEnd::Writer(parent_writer_pipe)],
        )
        .expect("spawn child reader with a parent writer");
        let mut parent_writer = parent_writer_graph
            .take_pipe_writer(parent_writer_pipe)
            .expect("parent writer");
        let mut parent_writer_processes = parent_writer_graph.into_processes();
        let mut parent_writer_consumer = parent_writer_processes.pop().expect("parent consumer");
        assert!(parent_writer_processes.is_empty());
        let mut copied_stdout = parent_writer_consumer
            .take_stdout()
            .expect("copied parent output");
        let expected_for_writer = expected.clone();
        let ((), copied, parent_writer_exit) =
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::join!(
                    async move {
                        parent_writer
                            .write_all(&expected_for_writer)
                            .await
                            .expect("write parent endpoint");
                        parent_writer.shutdown().await.expect("close parent writer");
                    },
                    async {
                        let mut bytes = Vec::new();
                        copied_stdout
                            .read_to_end(&mut bytes)
                            .await
                            .expect("read copied parent input");
                        bytes
                    },
                    parent_writer_consumer.wait(),
                )
            })
            .await
            .expect("parent writer streams with backpressure");
        assert!(parent_writer_exit.expect("wait parent consumer").success);
        assert_eq!(copied, expected);

        let parent_task_pipe = ProcessPipeId::new(13);
        let mut parent_task_graph = spawn_native_graph_with_parent_pipe_ends(
            &[],
            &[],
            &[
                ParentProcessPipeEnd::Reader(parent_task_pipe),
                ParentProcessPipeEnd::Writer(parent_task_pipe),
            ],
        )
        .expect("create parent task pipe");
        let mut task_reader = parent_task_graph
            .take_pipe_reader(parent_task_pipe)
            .expect("task reader");
        let mut task_writer = parent_task_graph
            .take_pipe_writer(parent_task_pipe)
            .expect("task writer");
        assert!(parent_task_graph.into_processes().is_empty());
        let expected_for_task = expected.clone();
        let ((), task_bytes) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(
                async move {
                    task_writer
                        .write_all(&expected_for_task)
                        .await
                        .expect("write task pipe");
                    task_writer.shutdown().await.expect("close task writer");
                },
                async {
                    let mut bytes = Vec::new();
                    task_reader
                        .read_to_end(&mut bytes)
                        .await
                        .expect("read task pipe");
                    bytes
                },
            )
        })
        .await
        .expect("parent task pipe preserves backpressure");
        assert_eq!(task_bytes, expected);

        let eof_pipe = ProcessPipeId::new(8);
        let mut eof_processes = spawn_native_graph_with_closed_pipe_ends(
            &[NativeProcessSpec {
                executable: executable.clone().into_os_string(),
                argv: vec![OsString::from("copy")],
                cwd: directory.0.clone(),
                environment: vec![],
                clear_environment: false,
                files: Vec::new(),
                stdin: ProcessStdio::Pipe(eof_pipe),
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Null,
            }],
            &[ClosedProcessPipeEnd::Writer(eof_pipe)],
        )
        .expect("spawn reader with parent-closed writer");
        let mut eof_consumer = eof_processes.pop().expect("EOF consumer");
        assert!(eof_processes.is_empty());
        let mut eof_stdout = eof_consumer.take_stdout().expect("EOF output");
        let (eof_bytes, eof_exit) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(
                async {
                    let mut bytes = Vec::new();
                    eof_stdout
                        .read_to_end(&mut bytes)
                        .await
                        .expect("read EOF output");
                    bytes
                },
                eof_consumer.wait(),
            )
        })
        .await
        .expect("parent-closed writer delivers EOF");
        assert!(eof_bytes.is_empty());
        assert!(eof_exit.expect("wait for EOF consumer").success);

        let broken_pipe = ProcessPipeId::new(9);
        let mut broken_processes = spawn_native_graph_with_closed_pipe_ends(
            &[NativeProcessSpec {
                executable: executable.into_os_string(),
                argv: vec![
                    OsString::from("produce"),
                    OsString::from(PAYLOAD_BYTES.to_string()),
                ],
                cwd: directory.0.clone(),
                environment: vec![],
                clear_environment: false,
                files: Vec::new(),
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Pipe(broken_pipe),
                stderr: ProcessStdio::Null,
            }],
            &[ClosedProcessPipeEnd::Reader(broken_pipe)],
        )
        .expect("spawn writer with parent-closed reader");
        let mut broken_producer = broken_processes.pop().expect("broken-pipe producer");
        assert!(broken_processes.is_empty());
        let broken_exit = tokio::time::timeout(Duration::from_secs(30), broken_producer.wait())
            .await
            .expect("parent-closed reader unblocks the producer")
            .expect("wait for broken-pipe producer");
        assert!(!broken_exit.success);

        let unused_pipe = ProcessPipeId::new(10);
        let unused = spawn_native_graph_with_closed_pipe_ends(
            &[],
            &[
                ClosedProcessPipeEnd::Reader(unused_pipe),
                ClosedProcessPipeEnd::Writer(unused_pipe),
            ],
        )
        .expect("create and close an unused pipe");
        assert!(unused.is_empty());
    }

    #[tokio::test]
    async fn process_is_spawned_directly_with_piped_machine_output() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let mut process = workspace
            .spawn(&ProcessSpec {
                executable: "rustc".to_owned(),
                argv: vec!["--version".to_owned()],
                cwd: ".".to_owned(),
                environment: vec![],
                clear_environment: false,
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Piped,
            })
            .expect("spawn rustc");
        let mut stdout = process.take_stdout().expect("stdout");
        let mut stderr = process.take_stderr().expect("stderr");
        let (stdout, stderr, exit) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.expect("stdout");
                bytes
            },
            async {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.expect("stderr");
                bytes
            },
            process.wait(),
        );
        assert!(exit.expect("wait").success, "stderr={stderr:?}");
        assert!(stdout.starts_with(b"rustc "));
    }

    #[tokio::test]
    async fn terminating_a_process_terminates_its_descendants() {
        let directory = TestDirectory::new();
        let executable = compile_process_tree_helper(&directory);
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let mut process = workspace
            .spawn(&ProcessSpec {
                executable,
                argv: vec![
                    "parent".to_owned(),
                    "ready".to_owned(),
                    "escaped".to_owned(),
                ],
                cwd: ".".to_owned(),
                environment: vec![],
                clear_environment: false,
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Piped,
            })
            .expect("spawn process tree");

        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.0.join("ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant should start");
        process.terminate().await.expect("terminate process tree");
        tokio::time::sleep(Duration::from_millis(1_200)).await;

        assert!(
            !directory.0.join("escaped").exists(),
            "descendant survived process-tree termination"
        );
    }

    #[tokio::test]
    async fn native_process_job_supervises_ordered_exits_and_all_member_trees() {
        let directory = TestDirectory::new();
        let executable = directory.0.join(compile_process_tree_helper(&directory));
        let spec = |arguments: Vec<OsString>, stdin, stdout| NativeProcessSpec {
            executable: executable.clone().into_os_string(),
            argv: arguments,
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            files: Vec::new(),
            stdin,
            stdout,
            stderr: ProcessStdio::Null,
        };

        let pipe = ProcessPipeId::new(41);
        let graph = spawn_native_graph_with_parent_io(
            &[
                spec(
                    vec![OsString::from("exit"), OsString::from("3")],
                    ProcessStdio::Null,
                    ProcessStdio::Pipe(pipe),
                ),
                spec(
                    vec![OsString::from("exit"), OsString::from("7")],
                    ProcessStdio::Pipe(pipe),
                    ProcessStdio::Null,
                ),
            ],
            &[],
            &[],
            &[],
            &[],
        )
        .expect("spawn supervised exit graph");
        let mut job = graph.into_job();
        assert_eq!(job.len(), 2);
        assert!(!job.is_empty());
        assert!(job.process_id(0).is_some());
        assert!(job.process_id(1).is_some());
        let exits = tokio::time::timeout(Duration::from_secs(5), job.wait())
            .await
            .expect("supervised exits complete")
            .expect("wait for supervised exits");
        assert_eq!(
            exits.iter().map(|exit| exit.code).collect::<Vec<_>>(),
            vec![Some(3), Some(7)],
            "job exits must retain specification order"
        );

        let unclaimed_pipe = ProcessPipeId::new(42);
        let mut graph = spawn_native_graph_with_parent_pipe_ends(
            &[],
            &[],
            &[
                ParentProcessPipeEnd::Reader(unclaimed_pipe),
                ParentProcessPipeEnd::Writer(unclaimed_pipe),
            ],
        )
        .expect("construct parent-only graph");
        let mut reader = graph
            .take_pipe_reader(unclaimed_pipe)
            .expect("take parent-only reader");
        let job = graph.into_job();
        assert!(job.is_empty());
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), reader.read_to_end(&mut bytes))
            .await
            .expect("job conversion closes unclaimed writer")
            .expect("read parent-only EOF");
        assert!(bytes.is_empty());

        let mut job = spawn_native_graph_with_parent_io(
            &[
                spec(
                    vec![
                        OsString::from("parent"),
                        OsString::from("first-ready"),
                        OsString::from("first-escaped"),
                    ],
                    ProcessStdio::Null,
                    ProcessStdio::Null,
                ),
                spec(
                    vec![
                        OsString::from("parent"),
                        OsString::from("second-ready"),
                        OsString::from("second-escaped"),
                    ],
                    ProcessStdio::Null,
                    ProcessStdio::Null,
                ),
            ],
            &[],
            &[],
            &[],
            &[],
        )
        .expect("spawn supervised process trees")
        .into_job();

        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.0.join("first-ready").is_file()
                || !directory.0.join("second-ready").is_file()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both supervised process trees should start");
        tokio::time::timeout(Duration::from_secs(5), job.terminate_and_reap())
            .await
            .expect("job termination completes")
            .expect("terminate every supervised process tree");
        tokio::time::sleep(Duration::from_millis(1_200)).await;

        assert!(
            !directory.0.join("first-escaped").exists(),
            "first job descendant survived termination"
        );
        assert!(
            !directory.0.join("second-escaped").exists(),
            "second job descendant survived termination"
        );
    }

    #[tokio::test]
    async fn dropping_a_process_handle_terminates_its_descendants() {
        let directory = TestDirectory::new();
        let executable = compile_process_tree_helper(&directory);
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let process = workspace
            .spawn(&ProcessSpec {
                executable,
                argv: vec![
                    "parent".to_owned(),
                    "ready".to_owned(),
                    "escaped".to_owned(),
                ],
                cwd: ".".to_owned(),
                environment: vec![],
                clear_environment: false,
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Piped,
            })
            .expect("spawn process tree");

        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.0.join("ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant should start");
        drop(process);
        tokio::time::sleep(Duration::from_millis(1_200)).await;

        assert!(
            !directory.0.join("escaped").exists(),
            "descendant survived process-handle drop"
        );
    }
}
