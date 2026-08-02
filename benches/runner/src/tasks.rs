use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use super::{Measurement, measure};

const MANIFEST_BYTES: &[u8] = include_bytes!("../../tasks/v1/manifest.json");
const MANIFEST_TEXT: &str = include_str!("../../tasks/v1/manifest.json");
const LOCK_BYTES: &[u8] = include_bytes!("../../tasks/v1/lock.json");
const LOCK_TEXT: &str = include_str!("../../tasks/v1/lock.json");
const MAX_TASKS: usize = 128;
const MAX_FILES: usize = 1024;
const MAX_WORKSPACE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_MILLIS: u64 = 30_000;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
pub(super) struct TaskCorpusReport {
    pub(super) schema: u8,
    corpus: &'static str,
    corpus_sha256: String,
    lock_sha256: String,
    pub(super) platform: &'static str,
    tokenizers: [&'static str; 2],
    pub(super) tasks: Vec<TaskReport>,
    pub(super) gates: TaskGates,
}

#[derive(Debug, Serialize)]
pub(super) struct TaskReport {
    id: String,
    family: String,
    workspace: String,
    capabilities: Vec<String>,
    output_retention: String,
    pub(super) declared_initial_tree_sha256: String,
    pub(super) initial_tree_sha256: String,
    pub(super) expected_final_tree_sha256: String,
    pub(super) final_tree_sha256: String,
    pub(super) baseline: BaselineReport,
}

#[derive(Debug, Serialize)]
pub(super) struct BaselineReport {
    shell: String,
    command_sha256: String,
    tool_calls: usize,
    retries: usize,
    elapsed_ns: u128,
    pub(super) success: bool,
    objective: Measurement,
    command: Measurement,
    pub(super) stdout: Measurement,
    pub(super) stderr: Measurement,
    pub(super) total: Measurement,
    stdout_sha256: String,
    stderr_sha256: String,
}

#[derive(Debug, Serialize)]
pub(super) struct TaskGates {
    pub(super) manifest_valid: bool,
    outputs_match: bool,
    pub(super) all_baselines_success: bool,
    pub(super) all_final_states_match: bool,
    pub(super) passed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskManifest {
    schema: u8,
    tasks: Vec<TaskDefinition>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskDefinition {
    id: String,
    family: String,
    workspace: String,
    objective: String,
    capabilities: Vec<String>,
    output_retention: String,
    limits: TaskLimits,
    expected: TaskExpected,
    baselines: PlatformBaselines,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskLimits {
    millis: u64,
    output_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskExpected {
    stdout: String,
    stderr: String,
    files: Vec<ExpectedFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedFile {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformBaselines {
    linux: BaselineDefinition,
    macos: BaselineDefinition,
    windows: BaselineDefinition,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaselineDefinition {
    shell: String,
    script: String,
}

impl PlatformBaselines {
    fn current(&self) -> Result<&BaselineDefinition, io::Error> {
        match std::env::consts::OS {
            "linux" => Ok(&self.linux),
            "macos" => Ok(&self.macos),
            "windows" => Ok(&self.windows),
            platform => Err(io::Error::other(format!(
                "task baselines do not support {platform}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskCorpusLock {
    schema: u8,
    manifest_sha256: String,
    tasks: Vec<TaskLockEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskLockEntry {
    id: String,
    initial_tree_sha256: String,
    expected_final_tree_sha256: String,
}

struct RawTaskRun {
    initial_tree_sha256: String,
    final_tree_sha256: String,
    shell: String,
    command: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    elapsed_ns: u128,
}

struct ProcessOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    elapsed_ns: u128,
}

pub(super) async fn build_report() -> Result<TaskCorpusReport, Box<dyn Error>> {
    let manifest = load_manifest()?;
    let lock = load_lock(&manifest)?;
    let cl100k = tiktoken_rs::cl100k_base()?;
    let o200k = tiktoken_rs::o200k_base()?;
    let mut reports = Vec::with_capacity(manifest.tasks.len());

    for (task, declared) in manifest.tasks.iter().zip(&lock.tasks) {
        let run = execute_task(task).await?;
        if run.initial_tree_sha256 != declared.initial_tree_sha256 {
            return Err(format!("initial tree digest changed for task {}", task.id).into());
        }
        if run.final_tree_sha256 != declared.expected_final_tree_sha256 {
            return Err(format!("final tree digest changed for task {}", task.id).into());
        }
        let objective = measure(&task.objective, &cl100k, &o200k);
        let command = measure(&run.command, &cl100k, &o200k);
        let stdout_text = std::str::from_utf8(&run.stdout)?;
        let stderr_text = std::str::from_utf8(&run.stderr)?;
        let stdout = measure(stdout_text, &cl100k, &o200k);
        let stderr = measure(stderr_text, &cl100k, &o200k);
        let total = sum_measurements([&objective, &command, &stdout, &stderr]);
        reports.push(TaskReport {
            id: task.id.clone(),
            family: task.family.clone(),
            workspace: task.workspace.clone(),
            capabilities: task.capabilities.clone(),
            output_retention: task.output_retention.clone(),
            declared_initial_tree_sha256: declared.initial_tree_sha256.clone(),
            initial_tree_sha256: run.initial_tree_sha256,
            expected_final_tree_sha256: declared.expected_final_tree_sha256.clone(),
            final_tree_sha256: run.final_tree_sha256,
            baseline: BaselineReport {
                shell: run.shell,
                command_sha256: sha256_hex(run.command.as_bytes()),
                tool_calls: 1,
                retries: 0,
                elapsed_ns: run.elapsed_ns,
                success: true,
                objective,
                command,
                stdout,
                stderr,
                total,
                stdout_sha256: sha256_hex(&run.stdout),
                stderr_sha256: sha256_hex(&run.stderr),
            },
        });
    }

    Ok(TaskCorpusReport {
        schema: 1,
        corpus: "benches/tasks/v1/manifest.json",
        corpus_sha256: sha256_hex(MANIFEST_BYTES),
        lock_sha256: sha256_hex(LOCK_BYTES),
        platform: std::env::consts::OS,
        tokenizers: [
            "tiktoken-rs/0.12.0:cl100k_base",
            "tiktoken-rs/0.12.0:o200k_base",
        ],
        tasks: reports,
        gates: TaskGates {
            manifest_valid: true,
            outputs_match: true,
            all_baselines_success: true,
            all_final_states_match: true,
            passed: true,
        },
    })
}

pub(super) async fn encoded_lock() -> Result<Vec<u8>, Box<dyn Error>> {
    let manifest = load_manifest()?;
    let mut entries = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        let run = execute_task(task).await?;
        entries.push(TaskLockEntry {
            id: task.id.clone(),
            initial_tree_sha256: run.initial_tree_sha256,
            expected_final_tree_sha256: run.final_tree_sha256,
        });
    }
    let lock = TaskCorpusLock {
        schema: 1,
        manifest_sha256: sha256_hex(MANIFEST_BYTES),
        tasks: entries,
    };
    let mut encoded = serde_json::to_vec_pretty(&lock)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn load_manifest() -> Result<TaskManifest, Box<dyn Error>> {
    let manifest: TaskManifest = serde_json::from_str(MANIFEST_TEXT)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn load_lock(manifest: &TaskManifest) -> Result<TaskCorpusLock, Box<dyn Error>> {
    let lock: TaskCorpusLock = serde_json::from_str(LOCK_TEXT)?;
    if lock.schema != 1
        || lock.manifest_sha256 != sha256_hex(MANIFEST_BYTES)
        || lock.tasks.len() != manifest.tasks.len()
    {
        return Err(io::Error::other("task corpus lock is stale").into());
    }
    for (task, entry) in manifest.tasks.iter().zip(&lock.tasks) {
        if entry.id != task.id
            || !valid_sha256(&entry.initial_tree_sha256)
            || !valid_sha256(&entry.expected_final_tree_sha256)
        {
            return Err(io::Error::other("task corpus lock is invalid").into());
        }
    }
    Ok(lock)
}

fn validate_manifest(manifest: &TaskManifest) -> Result<(), io::Error> {
    if manifest.schema != 1 || manifest.tasks.is_empty() || manifest.tasks.len() > MAX_TASKS {
        return Err(io::Error::other("invalid task manifest size or schema"));
    }
    let mut ids = BTreeSet::new();
    for task in &manifest.tasks {
        if !valid_name(&task.id)
            || !valid_name(&task.family)
            || !valid_name(&task.workspace)
            || !ids.insert(task.id.as_str())
            || task.objective.is_empty()
            || task.objective.len() > MAX_TEXT_BYTES
            || task.capabilities.is_empty()
            || task.output_retention != "immediate"
            || task.limits.millis == 0
            || task.limits.millis > MAX_MILLIS
            || task.limits.output_bytes == 0
            || task.limits.output_bytes > MAX_OUTPUT_BYTES
            || task.expected.stdout.len() + task.expected.stderr.len() > task.limits.output_bytes
        {
            return Err(io::Error::other(format!(
                "invalid task definition: {}",
                task.id
            )));
        }
        let mut capabilities = BTreeSet::new();
        if task.capabilities.iter().any(|capability| {
            !matches!(
                capability.as_str(),
                "exec" | "read" | "list" | "search" | "patch" | "fs" | "snapshot" | "batch" | "ref"
            ) || !capabilities.insert(capability.as_str())
        }) {
            return Err(io::Error::other(format!(
                "invalid task capabilities: {}",
                task.id
            )));
        }
        validate_baseline(&task.baselines.linux, "sh")?;
        validate_baseline(&task.baselines.macos, "sh")?;
        validate_baseline(&task.baselines.windows, "powershell")?;
        let mut expected_paths = BTreeSet::new();
        for file in &task.expected.files {
            if !valid_logical_path(&file.path)
                || !expected_paths.insert(file.path.as_str())
                || file.content.len() > MAX_WORKSPACE_BYTES
            {
                return Err(io::Error::other(format!(
                    "invalid expected file for task {}",
                    task.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_baseline(baseline: &BaselineDefinition, shell: &str) -> Result<(), io::Error> {
    if baseline.shell != shell
        || baseline.script.is_empty()
        || baseline.script.len() > MAX_TEXT_BYTES
    {
        Err(io::Error::other("invalid native-shell baseline"))
    } else {
        Ok(())
    }
}

async fn execute_task(task: &TaskDefinition) -> Result<RawTaskRun, Box<dyn Error>> {
    let fixture = fixture_path(&task.workspace)?;
    let directory = TempDir::new()?;
    copy_workspace(&fixture, directory.path())?;
    let initial_tree_sha256 = tree_sha256(directory.path())?;
    let baseline = task.baselines.current()?;
    let process = run_baseline(directory.path(), baseline, task.limits).await?;
    if !process.success {
        return Err(io::Error::other(format!("baseline failed for task {}", task.id)).into());
    }
    if process.stdout.len().saturating_add(process.stderr.len()) > task.limits.output_bytes {
        return Err(
            io::Error::other(format!("baseline output exceeded task limit: {}", task.id)).into(),
        );
    }
    if normalize_output(&process.stdout)? != task.expected.stdout
        || normalize_output(&process.stderr)? != task.expected.stderr
    {
        return Err(
            io::Error::other(format!("baseline output changed for task {}", task.id)).into(),
        );
    }
    for expected in &task.expected.files {
        let path = confined_path(directory.path(), &expected.path)?;
        if fs::read(&path)? != expected.content.as_bytes() {
            return Err(io::Error::other(format!(
                "baseline final file changed for task {}: {}",
                task.id, expected.path
            ))
            .into());
        }
    }
    let final_tree_sha256 = tree_sha256(directory.path())?;
    Ok(RawTaskRun {
        initial_tree_sha256,
        final_tree_sha256,
        shell: baseline.shell.clone(),
        command: format!("{} -c {}", baseline.shell, baseline.script),
        stdout: process.stdout,
        stderr: process.stderr,
        elapsed_ns: process.elapsed_ns,
    })
}

async fn run_baseline(
    workspace: &Path,
    baseline: &BaselineDefinition,
    limits: TaskLimits,
) -> Result<ProcessOutput, Box<dyn Error>> {
    let mut command = if baseline.shell == "sh" {
        let mut command = Command::new("sh");
        command.args(["-eu", "-c", &baseline.script]);
        command
    } else {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!("$ErrorActionPreference = 'Stop'; {}", baseline.script),
        ]);
        command
    };
    command
        .current_dir(workspace)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let started = Instant::now();
    let mut child = command.spawn()?;
    let streams = (child.stdout.take(), child.stderr.take());
    let (Some(stdout), Some(stderr)) = streams else {
        terminate(&mut child).await;
        return Err(io::Error::other("baseline child pipes were not configured").into());
    };
    let mut stdout_task = tokio::spawn(read_all(stdout));
    let mut stderr_task = tokio::spawn(read_all(stderr));
    let deadline = Duration::from_millis(limits.millis);
    let status = match tokio::time::timeout(deadline, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            terminate(&mut child).await;
            finish_reader(&mut stdout_task).await;
            finish_reader(&mut stderr_task).await;
            return Err(io::Error::other("native-shell baseline exceeded its deadline").into());
        }
    };
    let stdout = reader_result(&mut stdout_task, "stdout").await;
    let stderr = reader_result(&mut stderr_task, "stderr").await;
    let stdout = stdout?;
    let stderr = stderr?;
    Ok(ProcessOutput {
        success: status.success(),
        stdout,
        stderr,
        elapsed_ns: started.elapsed().as_nanos().max(1),
    })
}

async fn read_all(mut reader: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, io::Error> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output).await?;
    Ok(output)
}

async fn reader_result(
    task: &mut JoinHandle<Result<Vec<u8>, io::Error>>,
    stream: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    match tokio::time::timeout(CLEANUP_TIMEOUT, &mut *task).await {
        Ok(result) => Ok(result??),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(io::Error::other(format!("baseline {stream} drain exceeded its bound")).into())
        }
    }
}

async fn finish_reader(task: &mut JoinHandle<Result<Vec<u8>, io::Error>>) {
    if tokio::time::timeout(CLEANUP_TIMEOUT, &mut *task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

async fn terminate(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(CLEANUP_TIMEOUT, child.wait()).await;
}

fn fixture_path(workspace: &str) -> Result<PathBuf, Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tasks/v1/workspaces")
        .canonicalize()?;
    let path = root.join(workspace).canonicalize()?;
    if !path.starts_with(&root) || !path.is_dir() {
        return Err(io::Error::other("task workspace escaped the fixture root").into());
    }
    Ok(path)
}

fn copy_workspace(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let files = workspace_files(source)?;
    for (logical, source_path) in files {
        let destination_path = confined_path(destination, &logical)?;
        if let Some(parent) = destination_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source_path, destination_path)?;
    }
    Ok(())
}

fn tree_sha256(root: &Path) -> Result<String, Box<dyn Error>> {
    let files = workspace_files(root)?;
    let mut hasher = Sha256::new();
    hasher.update(b"ash-task-tree-v1\0");
    let mut total_bytes = 0_usize;
    for (logical, path) in files {
        let content = fs::read(path)?;
        total_bytes = total_bytes
            .checked_add(content.len())
            .ok_or_else(|| io::Error::other("task workspace byte count overflow"))?;
        if total_bytes > MAX_WORKSPACE_BYTES {
            return Err(io::Error::other("task workspace exceeds its byte ceiling").into());
        }
        hasher.update(u64::try_from(logical.len())?.to_le_bytes());
        hasher.update(logical.as_bytes());
        hasher.update(u64::try_from(content.len())?.to_le_bytes());
        hasher.update(&content);
    }
    Ok(hex(&hasher.finalize()))
}

fn workspace_files(root: &Path) -> Result<Vec<(String, PathBuf)>, Box<dyn Error>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("task workspaces cannot contain links").into());
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path.strip_prefix(root)?;
                let logical = logical_path(relative)?;
                files.push((logical, path));
                if files.len() > MAX_FILES {
                    return Err(io::Error::other("task workspace exceeds its file ceiling").into());
                }
            } else {
                return Err(io::Error::other("task workspace contains a special file").into());
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn logical_path(path: &Path) -> Result<String, io::Error> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| io::Error::other("task path is not UTF-8"))?,
            ),
            _ => return Err(io::Error::other("task path is not canonical")),
        }
    }
    let logical = parts.join("/");
    if valid_logical_path(&logical) {
        Ok(logical)
    } else {
        Err(io::Error::other("task path is invalid"))
    }
}

fn confined_path(root: &Path, logical: &str) -> Result<PathBuf, io::Error> {
    if !valid_logical_path(logical) {
        return Err(io::Error::other("invalid task logical path"));
    }
    let mut path = root.to_path_buf();
    for part in logical.split('/') {
        path.push(part);
    }
    Ok(path)
}

fn valid_logical_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn normalize_output(bytes: &[u8]) -> Result<String, Box<dyn Error>> {
    let normalized = std::str::from_utf8(bytes)?.replace("\r\n", "\n");
    if normalized.contains('\r') {
        Err(io::Error::other("baseline output contains a bare carriage return").into())
    } else {
        Ok(normalized)
    }
}

fn sum_measurements<const N: usize>(values: [&Measurement; N]) -> Measurement {
    Measurement {
        bytes: values.iter().map(|value| value.bytes).sum(),
        cl100k_tokens: values.iter().map(|value| value.cl100k_tokens).sum(),
        o200k_tokens: values.iter().map(|value| value.o200k_tokens).sum(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{
        ExpectedFile, MANIFEST_TEXT, TaskManifest, normalize_output, tree_sha256, validate_manifest,
    };

    fn manifest() -> TaskManifest {
        serde_json::from_str(MANIFEST_TEXT).expect("manifest")
    }

    #[test]
    fn strict_manifest_rejects_duplicate_ids_and_path_escape() {
        let mut duplicate = manifest();
        duplicate.tasks[1].id = duplicate.tasks[0].id.clone();
        assert!(validate_manifest(&duplicate).is_err());

        let mut escaped = manifest();
        escaped.tasks[0].expected.files.push(ExpectedFile {
            path: "../outside".to_owned(),
            content: "invalid".to_owned(),
        });
        assert!(validate_manifest(&escaped).is_err());
    }

    #[test]
    fn tree_digest_is_order_independent_and_content_bound() {
        let first = TempDir::new().expect("first tree");
        let second = TempDir::new().expect("second tree");
        fs::create_dir_all(first.path().join("nested")).expect("first directory");
        fs::create_dir_all(second.path().join("nested")).expect("second directory");
        fs::write(first.path().join("z.txt"), b"z\n").expect("first z");
        fs::write(first.path().join("nested/a.txt"), b"a\n").expect("first a");
        fs::write(second.path().join("nested/a.txt"), b"a\n").expect("second a");
        fs::write(second.path().join("z.txt"), b"z\n").expect("second z");
        assert_eq!(
            tree_sha256(first.path()).expect("first digest"),
            tree_sha256(second.path()).expect("second digest")
        );

        fs::write(second.path().join("z.txt"), b"changed\n").expect("changed z");
        assert_ne!(
            tree_sha256(first.path()).expect("first digest"),
            tree_sha256(second.path()).expect("changed digest")
        );
    }

    #[test]
    fn output_normalization_accepts_crlf_but_rejects_bare_carriage_returns() {
        assert_eq!(normalize_output(b"a\r\nb\r\n").expect("CRLF"), "a\nb\n");
        assert!(normalize_output(b"a\rb\n").is_err());
    }
}
