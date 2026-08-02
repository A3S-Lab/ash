use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use ash_cli::ExecutionSession;
use ash_engine::Parallelism;
use ash_protocol::Capability;
use ash_protocol::request::{
    Arguments, BatchArgs, BatchNode, Budget, FsAction, FsActionKind, FsArgs, ListArgs, PatchArgs,
    PatchContent, PatchEdit, ReadArgs, ReadMode, Request, SearchArgs,
};
use ash_protocol::response::{FinalResponse, ResultData, SearchMatch, Status};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use super::{Measurement, measure};

mod agent;

pub(super) use agent::{
    build_agent_report, capture_openai_agent_trace, validate_agent_trace,
    validate_agent_trace_audit,
};

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
    evidence_kind: &'static str,
    agent_results: bool,
    native_shell_accounting: &'static str,
    ash_accounting: &'static str,
    ash_session_bootstrap_tokens_included: bool,
    tokenizers: [&'static str; 2],
    pub(super) tasks: Vec<TaskReport>,
    summary: TaskSummary,
    pub(super) gates: TaskGates,
}

#[derive(Debug, Serialize)]
struct TaskSummary {
    tasks: usize,
    native_shell_tool_calls: usize,
    ash_tool_calls: usize,
    native_shell_total: Measurement,
    ash_total: Measurement,
    ash_vs_native_shell_bytes_percent: usize,
    ash_vs_native_shell_cl100k_percent: usize,
    ash_vs_native_shell_o200k_percent: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct TaskReport {
    id: String,
    family: String,
    workspace: String,
    capabilities: Vec<String>,
    output_retention: String,
    objective: Measurement,
    pub(super) declared_initial_tree_sha256: String,
    pub(super) expected_final_tree_sha256: String,
    pub(super) native_shell: NativeShellReport,
    pub(super) ash: AshReport,
}

#[derive(Debug, Serialize)]
pub(super) struct NativeShellReport {
    shell: String,
    command_sha256: String,
    pub(super) initial_tree_sha256: String,
    pub(super) final_tree_sha256: String,
    tool_calls: usize,
    retries: usize,
    elapsed_ns: u128,
    pub(super) success: bool,
    command: Measurement,
    pub(super) stdout: Measurement,
    pub(super) stderr: Measurement,
    pub(super) total: Measurement,
    stdout_sha256: String,
    stderr_sha256: String,
}

#[derive(Debug, Serialize)]
pub(super) struct AshReport {
    protocol: &'static str,
    plan_sha256: String,
    pub(super) initial_tree_sha256: String,
    pub(super) final_tree_sha256: String,
    tool_calls: usize,
    retries: usize,
    elapsed_ns: u128,
    pub(super) success: bool,
    steps: Vec<AshStepReport>,
    requests: Measurement,
    responses: Measurement,
    pub(super) total: Measurement,
    request_transcript_sha256: String,
    response_transcript_sha256: String,
    semantic_stdout_sha256: String,
    semantic_stderr_sha256: String,
}

#[derive(Debug, Serialize)]
struct AshStepReport {
    index: usize,
    operation: String,
    request: Measurement,
    response: Measurement,
    request_sha256: String,
    response_sha256: String,
}

#[derive(Debug, Serialize)]
pub(super) struct TaskGates {
    pub(super) manifest_valid: bool,
    outputs_match: bool,
    pub(super) all_native_shell_success: bool,
    pub(super) all_ash_success: bool,
    pub(super) all_initial_states_match: bool,
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
    ash: AshPlanDefinition,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AshPlanDefinition {
    steps: Vec<AshStepDefinition>,
    answer: AshAnswerDefinition,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum AshStepDefinition {
    List {
        paths: Vec<String>,
        depth: u16,
        flags: u32,
    },
    Search {
        query: String,
        paths: Vec<String>,
        flags: u32,
    },
    Read {
        paths: Vec<String>,
        mode: AshReadMode,
        offset: u64,
        length: u64,
    },
    Patch {
        paths: Vec<String>,
        digests: Vec<AshDigestSource>,
        edits: Vec<AshPatchEdit>,
        flags: u32,
    },
    Fs {
        actions: Vec<AshFsAction>,
    },
    Batch {
        nodes: Vec<AshBatchNode>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum AshReadMode {
    Bytes,
    Lines,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AshDigestSource {
    step: usize,
    path: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AshPatchEdit {
    file: u16,
    offset: u64,
    delete_bytes: u64,
    value: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AshFsAction {
    id: u64,
    kind: AshFsKind,
    path: String,
    destination: Option<String>,
    digest: Option<AshDigestSource>,
    value: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum AshFsKind {
    Create,
    Copy,
    Move,
    Remove,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AshBatchNode {
    id: u64,
    dependencies: Vec<u64>,
    action: AshLeafDefinition,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum AshLeafDefinition {
    List {
        paths: Vec<String>,
        depth: u16,
        flags: u32,
    },
    Search {
        query: String,
        paths: Vec<String>,
        flags: u32,
    },
    Read {
        paths: Vec<String>,
        mode: AshReadMode,
        offset: u64,
        length: u64,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum AshAnswerDefinition {
    ListPaths { step: usize },
    SearchPaths { step: usize },
    ErrorCodeCounts { step: usize },
    ReadText { step: usize },
    None,
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

struct RawNativeRun {
    initial_tree_sha256: String,
    final_tree_sha256: String,
    shell: String,
    command: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    elapsed_ns: u128,
}

struct RawAshRun {
    initial_tree_sha256: String,
    final_tree_sha256: String,
    steps: Vec<RawAshStep>,
    semantic_stdout: String,
    semantic_stderr: String,
    elapsed_ns: u128,
}

struct RawAshStep {
    operation: String,
    request: String,
    response: String,
}

struct SearchEvidence<'a> {
    matches: &'a [SearchMatch],
    paths: BTreeMap<u64, &'a str>,
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
    let mut native_shell_corpus_total = Measurement::default();
    let mut ash_corpus_total = Measurement::default();
    let mut native_shell_tool_calls = 0_usize;
    let mut ash_tool_calls = 0_usize;

    for (task, declared) in manifest.tasks.iter().zip(&lock.tasks) {
        let native = execute_native_task(task).await?;
        let ash = execute_ash_task(task).await?;
        if native.initial_tree_sha256 != declared.initial_tree_sha256
            || ash.initial_tree_sha256 != declared.initial_tree_sha256
            || native.initial_tree_sha256 != ash.initial_tree_sha256
        {
            return Err(format!("initial tree digest changed for task {}", task.id).into());
        }
        if native.final_tree_sha256 != declared.expected_final_tree_sha256
            || ash.final_tree_sha256 != declared.expected_final_tree_sha256
            || native.final_tree_sha256 != ash.final_tree_sha256
        {
            return Err(format!("final tree digest changed for task {}", task.id).into());
        }
        let objective = measure(&task.objective, &cl100k, &o200k);
        let command = measure(&native.command, &cl100k, &o200k);
        let stdout_text = std::str::from_utf8(&native.stdout)?;
        let stderr_text = std::str::from_utf8(&native.stderr)?;
        let stdout = measure(stdout_text, &cl100k, &o200k);
        let stderr = measure(stderr_text, &cl100k, &o200k);
        let native_total = sum_measurements([&objective, &command, &stdout, &stderr]);
        let requests = sum_text_measurements(
            ash.steps.iter().map(|step| step.request.as_str()),
            &cl100k,
            &o200k,
        );
        let responses = sum_text_measurements(
            ash.steps.iter().map(|step| step.response.as_str()),
            &cl100k,
            &o200k,
        );
        let ash_total = sum_measurements([&objective, &requests, &responses]);
        add_measurement(&mut native_shell_corpus_total, &native_total);
        add_measurement(&mut ash_corpus_total, &ash_total);
        native_shell_tool_calls += 1;
        ash_tool_calls += ash.steps.len();
        let step_reports = ash
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| AshStepReport {
                index,
                operation: step.operation.clone(),
                request: measure(&step.request, &cl100k, &o200k),
                response: measure(&step.response, &cl100k, &o200k),
                request_sha256: sha256_hex(step.request.as_bytes()),
                response_sha256: sha256_hex(step.response.as_bytes()),
            })
            .collect();
        let request_transcript_sha256 =
            transcript_sha256(ash.steps.iter().map(|step| step.request.as_bytes()));
        let response_transcript_sha256 =
            transcript_sha256(ash.steps.iter().map(|step| step.response.as_bytes()));
        let plan_sha256 = sha256_hex(&serde_json::to_vec(&task.ash)?);
        reports.push(TaskReport {
            id: task.id.clone(),
            family: task.family.clone(),
            workspace: task.workspace.clone(),
            capabilities: task.capabilities.clone(),
            output_retention: task.output_retention.clone(),
            objective,
            declared_initial_tree_sha256: declared.initial_tree_sha256.clone(),
            expected_final_tree_sha256: declared.expected_final_tree_sha256.clone(),
            native_shell: NativeShellReport {
                shell: native.shell,
                command_sha256: sha256_hex(native.command.as_bytes()),
                initial_tree_sha256: native.initial_tree_sha256,
                final_tree_sha256: native.final_tree_sha256,
                tool_calls: 1,
                retries: 0,
                elapsed_ns: native.elapsed_ns,
                success: true,
                command,
                stdout,
                stderr,
                total: native_total,
                stdout_sha256: sha256_hex(&native.stdout),
                stderr_sha256: sha256_hex(&native.stderr),
            },
            ash: AshReport {
                protocol: "ASH/1+ASON/1",
                plan_sha256,
                initial_tree_sha256: ash.initial_tree_sha256,
                final_tree_sha256: ash.final_tree_sha256,
                tool_calls: ash.steps.len(),
                retries: 0,
                elapsed_ns: ash.elapsed_ns,
                success: true,
                steps: step_reports,
                requests,
                responses,
                total: ash_total,
                request_transcript_sha256,
                response_transcript_sha256,
                semantic_stdout_sha256: sha256_hex(ash.semantic_stdout.as_bytes()),
                semantic_stderr_sha256: sha256_hex(ash.semantic_stderr.as_bytes()),
            },
        });
    }

    Ok(TaskCorpusReport {
        schema: 2,
        corpus: "benches/tasks/v1/manifest.json",
        corpus_sha256: sha256_hex(MANIFEST_BYTES),
        lock_sha256: sha256_hex(LOCK_BYTES),
        platform: std::env::consts::OS,
        evidence_kind: "deterministic-tool-plan",
        agent_results: false,
        native_shell_accounting: "objective+command+stdout+stderr",
        ash_accounting: "objective+canonical-requests+canonical-responses",
        ash_session_bootstrap_tokens_included: false,
        tokenizers: [
            "tiktoken-rs/0.12.0:cl100k_base",
            "tiktoken-rs/0.12.0:o200k_base",
        ],
        tasks: reports,
        summary: TaskSummary {
            tasks: manifest.tasks.len(),
            native_shell_tool_calls,
            ash_tool_calls,
            ash_vs_native_shell_bytes_percent: ratio_percent(
                ash_corpus_total.bytes,
                native_shell_corpus_total.bytes,
            ),
            ash_vs_native_shell_cl100k_percent: ratio_percent(
                ash_corpus_total.cl100k_tokens,
                native_shell_corpus_total.cl100k_tokens,
            ),
            ash_vs_native_shell_o200k_percent: ratio_percent(
                ash_corpus_total.o200k_tokens,
                native_shell_corpus_total.o200k_tokens,
            ),
            native_shell_total: native_shell_corpus_total,
            ash_total: ash_corpus_total,
        },
        gates: TaskGates {
            manifest_valid: true,
            outputs_match: true,
            all_native_shell_success: true,
            all_ash_success: true,
            all_initial_states_match: true,
            all_final_states_match: true,
            passed: true,
        },
    })
}

pub(super) async fn encoded_lock() -> Result<Vec<u8>, Box<dyn Error>> {
    let manifest = load_manifest()?;
    let mut entries = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        let native = execute_native_task(task).await?;
        let ash = execute_ash_task(task).await?;
        if native.initial_tree_sha256 != ash.initial_tree_sha256
            || native.final_tree_sha256 != ash.final_tree_sha256
        {
            return Err(format!("ASH/native-shell state mismatch for task {}", task.id).into());
        }
        entries.push(TaskLockEntry {
            id: task.id.clone(),
            initial_tree_sha256: native.initial_tree_sha256,
            expected_final_tree_sha256: native.final_tree_sha256,
        });
    }
    let lock = TaskCorpusLock {
        schema: 2,
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
    if lock.schema != 2
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
    if manifest.schema != 2 || manifest.tasks.is_empty() || manifest.tasks.len() > MAX_TASKS {
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
        validate_ash_plan(task)?;
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

fn validate_ash_plan(task: &TaskDefinition) -> Result<(), io::Error> {
    if task.ash.steps.is_empty()
        || task.ash.steps.len() > 64
        || serde_json::to_vec(&task.ash)
            .map_err(io::Error::other)?
            .len()
            > MAX_TEXT_BYTES
    {
        return Err(invalid_ash_plan(&task.id));
    }
    let capabilities: BTreeSet<_> = task.capabilities.iter().map(String::as_str).collect();
    for (index, step) in task.ash.steps.iter().enumerate() {
        if !capabilities.contains(step.operation()) {
            return Err(invalid_ash_plan(&task.id));
        }
        let valid = match step {
            AshStepDefinition::List {
                paths,
                depth,
                flags,
            } => ListArgs::new(paths.clone(), *depth, *flags).is_ok(),
            AshStepDefinition::Search {
                query,
                paths,
                flags,
            } => SearchArgs::new(query, paths.clone(), *flags).is_ok(),
            AshStepDefinition::Read {
                paths,
                mode,
                offset,
                length,
            } => ReadArgs::new(paths.clone(), mode.protocol(), *offset, *length).is_ok(),
            AshStepDefinition::Patch {
                paths,
                digests,
                edits,
                flags,
            } => {
                let sources_valid = digests.len() == paths.len()
                    && digests.iter().zip(paths).all(|(source, path)| {
                        source.step < index
                            && &source.path == path
                            && valid_protocol_path(&source.path)
                            && matches!(
                                &task.ash.steps[source.step],
                                AshStepDefinition::Read { paths, .. }
                                    if paths.iter().any(|path| path == &source.path)
                            )
                    });
                let edits = edits
                    .iter()
                    .map(AshPatchEdit::protocol)
                    .collect::<Result<Vec<_>, _>>();
                sources_valid
                    && edits.is_ok()
                    && PatchArgs::new(
                        paths.clone(),
                        vec!["0".repeat(64); paths.len()],
                        edits.unwrap_or_default(),
                        *flags,
                    )
                    .is_ok()
            }
            AshStepDefinition::Fs { actions } => {
                let sources_valid = actions.iter().all(|action| {
                    action.digest.as_ref().is_none_or(|source| {
                        source.step < index
                            && source.path == action.path
                            && valid_protocol_path(&source.path)
                            && matches!(
                                &task.ash.steps[source.step],
                                AshStepDefinition::Read { paths, .. }
                                    if paths.iter().any(|path| path == &source.path)
                            )
                    })
                });
                let actions = actions
                    .iter()
                    .map(|action| action.protocol(Some("0".repeat(64))))
                    .collect::<Result<Vec<_>, _>>();
                sources_valid && actions.is_ok() && FsArgs::new(actions.unwrap_or_default()).is_ok()
            }
            AshStepDefinition::Batch { nodes } => {
                let leaves_declared = nodes
                    .iter()
                    .all(|node| capabilities.contains(node.action.operation()));
                let nodes = nodes
                    .iter()
                    .map(AshBatchNode::protocol)
                    .collect::<Result<Vec<_>, _>>();
                leaves_declared
                    && nodes.is_ok()
                    && BatchArgs::new(nodes.unwrap_or_default()).is_ok()
            }
        };
        if !valid {
            return Err(invalid_ash_plan(&task.id));
        }
    }
    let answer_valid = match &task.ash.answer {
        AshAnswerDefinition::ListPaths { step } => {
            matches!(
                task.ash.steps.get(*step),
                Some(AshStepDefinition::List { .. })
            )
        }
        AshAnswerDefinition::SearchPaths { step }
        | AshAnswerDefinition::ErrorCodeCounts { step } => {
            matches!(
                task.ash.steps.get(*step),
                Some(AshStepDefinition::Search { .. })
            )
        }
        AshAnswerDefinition::ReadText { step } => {
            matches!(
                task.ash.steps.get(*step),
                Some(AshStepDefinition::Read { .. })
            )
        }
        AshAnswerDefinition::None => true,
    };
    if !answer_valid {
        return Err(invalid_ash_plan(&task.id));
    }
    Ok(())
}

fn invalid_ash_plan(task: &str) -> io::Error {
    io::Error::other(format!("invalid ASH task plan: {task}"))
}

impl AshStepDefinition {
    const fn operation(&self) -> &'static str {
        match self {
            Self::List { .. } => "list",
            Self::Search { .. } => "search",
            Self::Read { .. } => "read",
            Self::Patch { .. } => "patch",
            Self::Fs { .. } => "fs",
            Self::Batch { .. } => "batch",
        }
    }

    fn capability_mask(&self) -> u64 {
        match self {
            Self::List { .. } | Self::Search { .. } | Self::Read { .. } => {
                Capability::WorkspaceRead.mask()
            }
            Self::Patch { .. } | Self::Fs { .. } => Capability::WorkspaceWrite.mask(),
            Self::Batch { nodes } => nodes
                .iter()
                .fold(0, |mask, node| mask | node.action.capability_mask()),
        }
    }
}

impl AshReadMode {
    const fn protocol(self) -> ReadMode {
        match self {
            Self::Bytes => ReadMode::Bytes,
            Self::Lines => ReadMode::Lines,
        }
    }
}

impl AshPatchEdit {
    fn protocol(&self) -> Result<PatchEdit, ash_protocol::request::RequestError> {
        PatchEdit::new(
            self.file,
            self.offset,
            self.delete_bytes,
            PatchContent::Inline(self.value.clone()),
        )
    }
}

impl AshFsKind {
    const fn protocol(self) -> FsActionKind {
        match self {
            Self::Create => FsActionKind::Create,
            Self::Copy => FsActionKind::Copy,
            Self::Move => FsActionKind::Move,
            Self::Remove => FsActionKind::Remove,
        }
    }
}

impl AshFsAction {
    fn protocol(
        &self,
        resolved_digest: Option<String>,
    ) -> Result<FsAction, ash_protocol::request::RequestError> {
        FsAction::new(
            self.id,
            self.kind.protocol(),
            &self.path,
            self.destination.clone(),
            self.digest.as_ref().and(resolved_digest),
            self.value.clone().map(PatchContent::Inline),
        )
    }
}

impl AshLeafDefinition {
    const fn operation(&self) -> &'static str {
        match self {
            Self::List { .. } => "list",
            Self::Search { .. } => "search",
            Self::Read { .. } => "read",
        }
    }

    const fn capability_mask(&self) -> u64 {
        Capability::WorkspaceRead.mask()
    }

    fn protocol(&self) -> Result<Arguments, ash_protocol::request::RequestError> {
        match self {
            Self::List {
                paths,
                depth,
                flags,
            } => Ok(Arguments::List(ListArgs::new(
                paths.clone(),
                *depth,
                *flags,
            )?)),
            Self::Search {
                query,
                paths,
                flags,
            } => Ok(Arguments::Search(SearchArgs::new(
                query,
                paths.clone(),
                *flags,
            )?)),
            Self::Read {
                paths,
                mode,
                offset,
                length,
            } => Ok(Arguments::Read(ReadArgs::new(
                paths.clone(),
                mode.protocol(),
                *offset,
                *length,
            )?)),
        }
    }
}

impl AshBatchNode {
    fn protocol(&self) -> Result<BatchNode, ash_protocol::request::RequestError> {
        BatchNode::new(self.id, self.dependencies.clone(), self.action.protocol()?)
    }
}

async fn execute_native_task(task: &TaskDefinition) -> Result<RawNativeRun, Box<dyn Error>> {
    let fixture = fixture_path(&task.workspace)?;
    let directory = TempDir::new()?;
    copy_workspace(&fixture, directory.path())?;
    let initial_tree_sha256 = tree_sha256(directory.path())?;
    let baseline = task.baselines.current()?;
    let process = run_baseline(directory.path(), baseline, task.limits)
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "native-shell baseline failed for task {}: {error}",
                task.id
            ))
        })?;
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
    Ok(RawNativeRun {
        initial_tree_sha256,
        final_tree_sha256,
        shell: baseline.shell.clone(),
        command: format!("{} -c {}", baseline.shell, baseline.script),
        stdout: process.stdout,
        stderr: process.stderr,
        elapsed_ns: process.elapsed_ns,
    })
}

async fn execute_ash_task(task: &TaskDefinition) -> Result<RawAshRun, Box<dyn Error>> {
    let fixture = fixture_path(&task.workspace)?;
    let directory = TempDir::new()?;
    copy_workspace(&fixture, directory.path())?;
    let initial_tree_sha256 = tree_sha256(directory.path())?;
    let workspace = directory
        .path()
        .to_str()
        .ok_or_else(|| io::Error::other("task workspace path is not UTF-8"))?;
    let capability_mask = task
        .ash
        .steps
        .iter()
        .fold(0, |mask, step| mask | step.capability_mask());
    let started = Instant::now();
    let session = ExecutionSession::open(
        1,
        workspace,
        u64::try_from(task.limits.output_bytes)?,
        Parallelism::detected(),
        capability_mask,
    )?;
    let execution = execute_ash_steps(task, &session, started).await;
    let close = session.close();
    let (steps, responses) = execution?;
    close?;
    let elapsed_ns = started.elapsed().as_nanos().max(1);
    let semantic_stdout = ash_semantic_output(&task.ash.answer, &responses)?;
    let semantic_stderr = String::new();
    if semantic_stdout != task.expected.stdout || semantic_stderr != task.expected.stderr {
        return Err(
            io::Error::other(format!("ASH semantic output changed for task {}", task.id)).into(),
        );
    }
    verify_expected_files(directory.path(), task, "ASH")?;
    let final_tree_sha256 = tree_sha256(directory.path())?;
    Ok(RawAshRun {
        initial_tree_sha256,
        final_tree_sha256,
        steps,
        semantic_stdout,
        semantic_stderr,
        elapsed_ns,
    })
}

async fn execute_ash_steps(
    task: &TaskDefinition,
    session: &ExecutionSession,
    started: Instant,
) -> Result<(Vec<RawAshStep>, Vec<FinalResponse>), Box<dyn Error>> {
    let mut transcript = Vec::with_capacity(task.ash.steps.len());
    let mut responses = Vec::with_capacity(task.ash.steps.len());
    let mut response_bytes = 0_usize;
    for (index, step) in task.ash.steps.iter().enumerate() {
        let elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let remaining_millis = task
            .limits
            .millis
            .checked_sub(elapsed_millis)
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| io::Error::other("ASH task plan exceeded its deadline"))?;
        let request = ash_request(u64::try_from(index + 1)?, step, &responses, task.limits)?;
        let document = request.encode()?;
        let request_text = document.encode();
        let decoded = Request::decode(&document)?;
        let response = tokio::time::timeout(
            Duration::from_millis(remaining_millis),
            session.execute(&decoded),
        )
        .await
        .map_err(|_| io::Error::other("ASH task operation exceeded its deadline"))??;
        if response.status() != Status::Success {
            return Err(io::Error::other(format!(
                "ASH task operation {} failed with status {}",
                index,
                response.status().code()
            ))
            .into());
        }
        let response_text = response.encode()?.encode();
        response_bytes = response_bytes
            .checked_add(response_text.len())
            .ok_or_else(|| io::Error::other("ASH task response byte count overflow"))?;
        if response_bytes > task.limits.output_bytes {
            return Err(io::Error::other("ASH task responses exceeded the output limit").into());
        }
        transcript.push(RawAshStep {
            operation: step.operation().to_owned(),
            request: request_text,
            response: response_text,
        });
        responses.push(response);
    }
    Ok((transcript, responses))
}

fn ash_request(
    id: u64,
    step: &AshStepDefinition,
    responses: &[FinalResponse],
    limits: TaskLimits,
) -> Result<Request, Box<dyn Error>> {
    let arguments = match step {
        AshStepDefinition::List {
            paths,
            depth,
            flags,
        } => Arguments::List(ListArgs::new(paths.clone(), *depth, *flags)?),
        AshStepDefinition::Search {
            query,
            paths,
            flags,
        } => Arguments::Search(SearchArgs::new(query, paths.clone(), *flags)?),
        AshStepDefinition::Read {
            paths,
            mode,
            offset,
            length,
        } => Arguments::Read(ReadArgs::new(
            paths.clone(),
            mode.protocol(),
            *offset,
            *length,
        )?),
        AshStepDefinition::Patch {
            paths,
            digests,
            edits,
            flags,
        } => {
            let digests = digests
                .iter()
                .map(|source| read_digest(responses, source))
                .collect::<Result<Vec<_>, _>>()?;
            let edits = edits
                .iter()
                .map(AshPatchEdit::protocol)
                .collect::<Result<Vec<_>, _>>()?;
            Arguments::Patch(PatchArgs::new(paths.clone(), digests, edits, *flags)?)
        }
        AshStepDefinition::Fs { actions } => {
            let actions = actions
                .iter()
                .map(|action| {
                    let digest = action
                        .digest
                        .as_ref()
                        .map(|source| read_digest(responses, source))
                        .transpose()?;
                    Ok::<_, Box<dyn Error>>(action.protocol(digest)?)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Arguments::Fs(FsArgs::new(actions)?)
        }
        AshStepDefinition::Batch { nodes } => Arguments::Batch(BatchArgs::new(
            nodes
                .iter()
                .map(AshBatchNode::protocol)
                .collect::<Result<Vec<_>, _>>()?,
        )?),
    };
    let capacity = u32::try_from(limits.output_bytes)?;
    Ok(Request::new(
        id,
        arguments,
        Budget::new(capacity, capacity, limits.millis)?,
    )?)
}

fn read_digest(
    responses: &[FinalResponse],
    source: &AshDigestSource,
) -> Result<String, Box<dyn Error>> {
    let response = responses
        .get(source.step)
        .ok_or_else(|| io::Error::other("ASH digest source step is unavailable"))?;
    let paths = response_path_map(response)?;
    let ResultData::Read(results) = response
        .data()
        .ok_or_else(|| io::Error::other("ASH digest source has no response data"))?
    else {
        return Err(io::Error::other("ASH digest source is not a read response").into());
    };
    results
        .iter()
        .find(|result| {
            paths
                .get(&result.path)
                .is_some_and(|path| *path == source.path)
        })
        .map(|result| result.digest.clone())
        .ok_or_else(|| io::Error::other("ASH digest source path is unavailable").into())
}

fn ash_semantic_output(
    answer: &AshAnswerDefinition,
    responses: &[FinalResponse],
) -> Result<String, Box<dyn Error>> {
    match answer {
        AshAnswerDefinition::ListPaths { step } => {
            let response = responses
                .get(*step)
                .ok_or_else(|| io::Error::other("ASH list answer step is unavailable"))?;
            let paths = response_path_map(response)?;
            let ResultData::List(entries) = response
                .data()
                .ok_or_else(|| io::Error::other("ASH list answer has no response data"))?
            else {
                return Err(io::Error::other("ASH list answer is not a list response").into());
            };
            entries
                .iter()
                .map(|entry| {
                    paths
                        .get(&entry.path)
                        .map(|path| format!("{path}\n"))
                        .ok_or_else(|| io::Error::other("ASH list path mapping is missing"))
                })
                .collect::<Result<String, _>>()
                .map_err(Into::into)
        }
        AshAnswerDefinition::SearchPaths { step } => {
            let evidence = search_response(responses, *step)?;
            let unique = evidence
                .matches
                .iter()
                .map(|matched| {
                    evidence
                        .paths
                        .get(&matched.path)
                        .copied()
                        .ok_or_else(|| io::Error::other("ASH search path mapping is missing"))
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            Ok(unique.into_iter().map(|path| format!("{path}\n")).collect())
        }
        AshAnswerDefinition::ErrorCodeCounts { step } => {
            let evidence = search_response(responses, *step)?;
            let mut counts = BTreeMap::<&str, usize>::new();
            for matched in evidence.matches {
                let code = diagnostic_code(&matched.text)
                    .ok_or_else(|| io::Error::other("ASH search returned an invalid error code"))?;
                *counts.entry(code).or_default() += 1;
            }
            Ok(counts
                .into_iter()
                .map(|(code, count)| format!("{code} {count}\n"))
                .collect())
        }
        AshAnswerDefinition::ReadText { step } => {
            let response = responses
                .get(*step)
                .ok_or_else(|| io::Error::other("ASH read answer step is unavailable"))?;
            let ResultData::Read(results) = response
                .data()
                .ok_or_else(|| io::Error::other("ASH read answer has no response data"))?
            else {
                return Err(io::Error::other("ASH read answer is not a read response").into());
            };
            results
                .iter()
                .map(|result| {
                    result
                        .text
                        .as_deref()
                        .ok_or_else(|| io::Error::other("ASH read answer was retained"))
                })
                .collect::<Result<String, _>>()
                .map_err(Into::into)
        }
        AshAnswerDefinition::None => Ok(String::new()),
    }
}

fn search_response(
    responses: &[FinalResponse],
    step: usize,
) -> Result<SearchEvidence<'_>, Box<dyn Error>> {
    let response = responses
        .get(step)
        .ok_or_else(|| io::Error::other("ASH search answer step is unavailable"))?;
    let paths = response_path_map(response)?;
    let ResultData::Search(matches) = response
        .data()
        .ok_or_else(|| io::Error::other("ASH search answer has no response data"))?
    else {
        return Err(io::Error::other("ASH search answer is not a search response").into());
    };
    Ok(SearchEvidence { matches, paths })
}

fn response_path_map(response: &FinalResponse) -> Result<BTreeMap<u64, &str>, io::Error> {
    let mut paths = BTreeMap::new();
    for mapping in response.paths() {
        if mapping.id == 0 || paths.insert(mapping.id, mapping.value.as_str()).is_some() {
            return Err(io::Error::other("ASH response has invalid path mappings"));
        }
    }
    Ok(paths)
}

fn diagnostic_code(line: &str) -> Option<&str> {
    let (code, _) = line.strip_prefix("error ")?.split_once(':')?;
    (code.len() == 4
        && code.starts_with('E')
        && code[1..].bytes().all(|byte| byte.is_ascii_digit()))
    .then_some(code)
}

fn verify_expected_files(
    workspace: &Path,
    task: &TaskDefinition,
    executor: &str,
) -> Result<(), Box<dyn Error>> {
    for expected in &task.expected.files {
        let path = confined_path(workspace, &expected.path)?;
        if fs::read(&path)? != expected.content.as_bytes() {
            return Err(io::Error::other(format!(
                "{executor} final file changed for task {}: {}",
                task.id, expected.path
            ))
            .into());
        }
    }
    Ok(())
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
            if directory == root && entry.file_name() == ".ash" {
                continue;
            }
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

fn valid_protocol_path(path: &str) -> bool {
    path == "." || valid_logical_path(path)
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

fn add_measurement(total: &mut Measurement, value: &Measurement) {
    total.bytes += value.bytes;
    total.cl100k_tokens += value.cl100k_tokens;
    total.o200k_tokens += value.o200k_tokens;
}

fn ratio_percent(value: usize, baseline: usize) -> usize {
    if baseline == 0 {
        usize::MAX
    } else {
        value.saturating_mul(100).div_ceil(baseline)
    }
}

fn sum_text_measurements<'a>(
    values: impl Iterator<Item = &'a str>,
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Measurement {
    values.fold(Measurement::default(), |mut total, value| {
        let value = measure(value, cl100k, o200k);
        total.bytes += value.bytes;
        total.cl100k_tokens += value.cl100k_tokens;
        total.o200k_tokens += value.o200k_tokens;
        total
    })
}

fn transcript_sha256<'a>(values: impl Iterator<Item = &'a [u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ash-task-transcript-v1\0");
    for value in values {
        hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(value);
    }
    hex(&hasher.finalize())
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
        AshStepDefinition, ExpectedFile, MANIFEST_TEXT, TaskManifest, normalize_output,
        tree_sha256, validate_manifest,
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

        let mut forward_digest = manifest();
        let AshStepDefinition::Patch { digests, .. } = &mut forward_digest.tasks[2].ash.steps[1]
        else {
            panic!("mutation plan must end in patch");
        };
        digests[0].step = 1;
        assert!(validate_manifest(&forward_digest).is_err());
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

        fs::create_dir_all(first.path().join(".ash")).expect("private state directory");
        fs::write(first.path().join(".ash/transaction.lock"), b"private").expect("private state");
        assert_eq!(
            tree_sha256(first.path()).expect("private-state digest"),
            tree_sha256(second.path()).expect("visible digest")
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
