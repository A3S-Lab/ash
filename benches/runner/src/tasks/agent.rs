use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use ash_cli::ExecutionSession;
use ash_engine::Parallelism;
use ash_protocol::ason::{Limits, decode_with_limits};
use ash_protocol::request::{Arguments, Request};
use ash_protocol::response::Status;
use ash_protocol::{Capability, Operation};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::task::JoinHandle;

use super::{
    CLEANUP_TIMEOUT, LOCK_BYTES, MANIFEST_BYTES, MAX_TEXT_BYTES, TaskCorpusLock, TaskDefinition,
    TaskManifest, add_measurement, confined_path, copy_workspace, fixture_path, load_lock,
    load_manifest, ratio_percent, sha256_hex, sum_measurements, sum_text_measurements, terminate,
    tree_sha256, valid_name, valid_sha256,
};
use crate::{Measurement, measure};

mod openai;

pub(crate) use openai::capture_openai_agent_trace;

const TRACE_SCHEMA: u8 = 1;
const REPORT_SCHEMA: u8 = 1;
const MAX_TRACE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_AUDIT_FIELD_BYTES: usize = 4 * 1024 * 1024;
const MAX_REPETITIONS: u16 = 32;
const MAX_ATTEMPTS: usize = 64;
const MAX_MODEL_ELAPSED_MILLIS: u64 = 3_600_000;
const MAX_PROVIDER_TOKENS: u64 = 1_000_000_000;
const MAX_TASK_VISIBLE_BYTES: usize = 16 * 1024 * 1024;
const INVALID_REQUEST_RESULT: &str = "e:invalid-request\n";
const POLICY_REJECTED_RESULT: &str = "e:policy-rejected\n";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentTrace {
    schema: u8,
    evidence_kind: String,
    experiment_id: String,
    captured_at_utc: String,
    manifest_sha256: String,
    lock_sha256: String,
    driver: DriverMetadata,
    model: ModelMetadata,
    platform: String,
    architecture: String,
    repetitions: u16,
    primers: PrimerSet,
    audit_sha256: String,
    runs: Vec<AgentRunTrace>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DriverMetadata {
    name: String,
    version: String,
    source_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModelMetadata {
    provider: String,
    id: String,
    revision: String,
    context_tokens: u64,
    max_output_tokens: u64,
    temperature: String,
    top_p: String,
    reasoning_effort: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrimerSet {
    shared: String,
    ash: String,
    native_shell: String,
    ash_tools: String,
    native_shell_tools: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AgentArm {
    Ash,
    NativeShell,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentRunTrace {
    arm: AgentArm,
    repetition: u16,
    seed: u64,
    tasks: Vec<AgentTaskTrace>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentTaskTrace {
    id: String,
    prompt: String,
    attempts: Vec<AgentAttemptTrace>,
    final_stdout: String,
    final_stderr: String,
    finish_elapsed_millis: u64,
    finish_request_sha256: String,
    finish_response_sha256: String,
    usage: ProviderUsage,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AttemptKind {
    Request,
    InvalidRequest,
    PolicyRejected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentAttemptTrace {
    kind: AttemptKind,
    model_output: String,
    tool_result_sha256: String,
    model_elapsed_millis: u64,
    provider_request_sha256: String,
    provider_response_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    visible_output_tokens: u64,
    hidden_reasoning_tokens: Option<u64>,
    raw_usage_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentAuditRecord {
    schema: u8,
    provider: String,
    arm: AgentArm,
    repetition: u16,
    seed: u64,
    task_id: String,
    turn: usize,
    phase: String,
    request_sha256: String,
    response_sha256: String,
    request_json: String,
    response_json: String,
}

impl ProviderUsage {
    fn visible_total(&self) -> Result<u64, io::Error> {
        self.input_tokens
            .checked_add(self.visible_output_tokens)
            .ok_or_else(|| io::Error::other("provider Token total overflow"))
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentCorpusReport {
    schema: u8,
    evidence_kind: &'static str,
    agent_results: bool,
    provenance: &'static str,
    provider_attestation_verified: bool,
    audit_verified: bool,
    trace_sha256: String,
    audit_sha256: String,
    experiment_id: String,
    captured_at_utc: String,
    corpus: &'static str,
    manifest_sha256: String,
    lock_sha256: String,
    platform: String,
    architecture: String,
    driver: DriverMetadata,
    model: ModelMetadata,
    repetitions: u16,
    provider_accounting: &'static str,
    normalized_accounting: &'static str,
    hidden_reasoning_tokens_included: bool,
    tokenizers: [&'static str; 2],
    primers: PrimerReport,
    runs: Vec<AgentRunReport>,
    summary: AgentSummary,
    gates: AgentGates,
}

#[derive(Debug, Serialize)]
struct PrimerReport {
    shared: TextEvidence,
    ash: TextEvidence,
    native_shell: TextEvidence,
    ash_tools: TextEvidence,
    native_shell_tools: TextEvidence,
}

#[derive(Debug, Serialize)]
struct TextEvidence {
    sha256: String,
    measurement: Measurement,
}

#[derive(Debug, Serialize)]
struct AgentRunReport {
    arm: AgentArm,
    repetition: u16,
    seed: u64,
    task_order: Vec<String>,
    tasks: Vec<AgentTaskReport>,
    provider_usage: ProviderUsageTotal,
    normalized_input: Measurement,
    normalized_output: Measurement,
    normalized_total: Measurement,
    tool_calls: usize,
    executed_operations: usize,
    failed_attempts: usize,
    retries: usize,
    model_elapsed_millis: u64,
    replay_elapsed_ns: u128,
    successful_tasks: usize,
    success: bool,
}

#[derive(Debug, Serialize)]
struct AgentTaskReport {
    id: String,
    family: String,
    declared_initial_tree_sha256: String,
    actual_initial_tree_sha256: String,
    expected_final_tree_sha256: String,
    actual_final_tree_sha256: String,
    attempts: Vec<AgentAttemptReport>,
    tool_calls: usize,
    executed_operations: usize,
    failed_attempts: usize,
    retries: usize,
    provider_usage: ProviderUsage,
    provider_visible_tokens: u64,
    normalized_input: Measurement,
    normalized_output: Measurement,
    normalized_total: Measurement,
    model_elapsed_millis: u64,
    replay_elapsed_ns: u128,
    final_stdout_sha256: String,
    final_stderr_sha256: String,
    finish_request_sha256: String,
    finish_response_sha256: String,
    semantic_output_match: bool,
    expected_files_match: bool,
    final_tree_match: bool,
    success: bool,
}

#[derive(Debug, Serialize)]
struct AgentAttemptReport {
    index: usize,
    kind: AttemptKind,
    operation: Option<&'static str>,
    outcome: String,
    model_output: Measurement,
    tool_result: Measurement,
    model_output_sha256: String,
    tool_result_sha256: String,
    provider_request_sha256: String,
    provider_response_sha256: String,
    raw_stdout_sha256: Option<String>,
    raw_stderr_sha256: Option<String>,
    model_elapsed_millis: u64,
    replay_elapsed_ns: u128,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ProviderUsageTotal {
    input_tokens: u64,
    cached_input_tokens: u64,
    visible_output_tokens: u64,
    hidden_reasoning_tokens: Option<u64>,
    visible_total_tokens: u64,
}

#[derive(Debug, Serialize)]
struct AgentSummary {
    ash: AgentArmSummary,
    native_shell: AgentArmSummary,
    comparison: AgentComparison,
}

#[derive(Debug, Serialize)]
struct AgentArmSummary {
    runs: usize,
    tasks: usize,
    successful_tasks: usize,
    success_basis_points: usize,
    tool_calls: usize,
    executed_operations: usize,
    failed_attempts: usize,
    retries: usize,
    provider_usage: ProviderUsageTotal,
    median_provider_visible_tokens_per_successful_task: Option<u64>,
    normalized_total: Measurement,
    median_normalized_cl100k_tokens_per_successful_task: Option<usize>,
    median_normalized_o200k_tokens_per_successful_task: Option<usize>,
    model_elapsed_millis: u64,
    replay_elapsed_ns: u128,
}

#[derive(Debug, Serialize)]
struct AgentComparison {
    valid: bool,
    success_rate_gap_basis_points: usize,
    ash_vs_native_shell_median_provider_visible_tokens_percent: Option<usize>,
    ash_vs_native_shell_normalized_cl100k_tokens_percent: Option<usize>,
    ash_vs_native_shell_normalized_o200k_tokens_percent: Option<usize>,
}

#[derive(Debug, Serialize)]
struct AgentGates {
    strict_trace_valid: bool,
    corpus_lock_match: bool,
    paired_runs: bool,
    tool_result_hashes_match: bool,
    all_tasks_success: bool,
    comparable_success: bool,
    passed: bool,
}

struct ReplayedTask {
    report: AgentTaskReport,
}

struct NativeProcessOutput {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    output_exceeded: bool,
    elapsed_ns: u128,
}

struct CapturedStream {
    bytes: Vec<u8>,
    total_bytes: usize,
}

pub(crate) fn validate_agent_trace(path: &Path) -> Result<(), Box<dyn Error>> {
    let manifest = load_manifest()?;
    let lock = load_lock(&manifest)?;
    let _ = read_trace(path, &manifest, &lock)?;
    Ok(())
}

pub(crate) fn validate_agent_trace_audit(
    trace_path: &Path,
    audit_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let manifest = load_manifest()?;
    let lock = load_lock(&manifest)?;
    let (trace, _) = read_trace(trace_path, &manifest, &lock)?;
    let metadata = fs::metadata(audit_path)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_TRACE_BYTES {
        return Err(io::Error::other("agent audit size is invalid").into());
    }
    let bytes = fs::read(audit_path)?;
    if sha256_hex(&bytes) != trace.audit_sha256 || bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("agent audit digest or framing differs").into());
    }
    let mut records = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let payload = line
            .strip_suffix(b"\n")
            .ok_or_else(|| io::Error::other("agent audit line is not LF framed"))?;
        if payload.is_empty() || payload.contains(&b'\r') {
            return Err(io::Error::other("agent audit line is not canonical").into());
        }
        let record: AgentAuditRecord = serde_json::from_slice(payload)?;
        if serde_json::to_vec(&record)? != payload {
            return Err(io::Error::other("agent audit JSON is not canonical").into());
        }
        validate_audit_record(&record, &trace.model.provider)?;
        records.push(record);
    }
    let mut index = 0_usize;
    for run in &trace.runs {
        for task in &run.tasks {
            for (turn, attempt) in task.attempts.iter().enumerate() {
                let record = records
                    .get(index)
                    .ok_or_else(|| io::Error::other("agent audit record is missing"))?;
                if !audit_record_matches(
                    record,
                    run,
                    task,
                    turn,
                    "action",
                    &attempt.provider_request_sha256,
                    &attempt.provider_response_sha256,
                ) {
                    return Err(
                        io::Error::other("agent audit does not match the trace matrix").into(),
                    );
                }
                index += 1;
            }
            let record = records
                .get(index)
                .ok_or_else(|| io::Error::other("agent audit finish record is missing"))?;
            if !audit_record_matches(
                record,
                run,
                task,
                task.attempts.len(),
                "finish",
                &task.finish_request_sha256,
                &task.finish_response_sha256,
            ) {
                return Err(io::Error::other("agent audit finish does not match trace").into());
            }
            index += 1;
        }
    }
    if index != records.len() {
        return Err(io::Error::other("agent audit has surplus records").into());
    }
    Ok(())
}

fn validate_audit_record(record: &AgentAuditRecord, provider: &str) -> Result<(), Box<dyn Error>> {
    if record.schema != 1
        || record.provider != provider
        || !valid_name(&record.task_id)
        || !matches!(record.phase.as_str(), "action" | "finish")
        || !valid_sha256(&record.request_sha256)
        || !valid_sha256(&record.response_sha256)
        || record.request_json.is_empty()
        || record.response_json.is_empty()
        || record.request_json.len() > MAX_AUDIT_FIELD_BYTES
        || record.response_json.len() > MAX_AUDIT_FIELD_BYTES
        || sha256_hex(record.request_json.as_bytes()) != record.request_sha256
        || sha256_hex(record.response_json.as_bytes()) != record.response_sha256
    {
        return Err(io::Error::other("agent audit record is invalid").into());
    }
    let _: serde_json::Value = serde_json::from_str(&record.request_json)?;
    let _: serde_json::Value = serde_json::from_str(&record.response_json)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn audit_record_matches(
    record: &AgentAuditRecord,
    run: &AgentRunTrace,
    task: &AgentTaskTrace,
    turn: usize,
    phase: &str,
    request_sha256: &str,
    response_sha256: &str,
) -> bool {
    record.arm == run.arm
        && record.repetition == run.repetition
        && record.seed == run.seed
        && record.task_id == task.id
        && record.turn == turn
        && record.phase == phase
        && record.request_sha256 == request_sha256
        && record.response_sha256 == response_sha256
}

pub(crate) async fn build_agent_report(
    path: &Path,
    audit_path: &Path,
) -> Result<AgentCorpusReport, Box<dyn Error>> {
    let manifest = load_manifest()?;
    let lock = load_lock(&manifest)?;
    validate_agent_trace_audit(path, audit_path)?;
    let (trace, trace_bytes) = read_trace(path, &manifest, &lock)?;
    let cl100k = tiktoken_rs::cl100k_base()?;
    let o200k = tiktoken_rs::o200k_base()?;
    let primers = PrimerReport {
        shared: text_evidence(&trace.primers.shared, &cl100k, &o200k),
        ash: text_evidence(&trace.primers.ash, &cl100k, &o200k),
        native_shell: text_evidence(&trace.primers.native_shell, &cl100k, &o200k),
        ash_tools: text_evidence(&trace.primers.ash_tools, &cl100k, &o200k),
        native_shell_tools: text_evidence(&trace.primers.native_shell_tools, &cl100k, &o200k),
    };
    let tasks_by_id = manifest
        .tasks
        .iter()
        .zip(&lock.tasks)
        .map(|(task, locked)| (task.id.as_str(), (task, locked)))
        .collect::<BTreeMap<_, _>>();
    let mut runs = Vec::with_capacity(trace.runs.len());
    for run in &trace.runs {
        let replay_started = Instant::now();
        let (arm_primer, arm_tools) = match run.arm {
            AgentArm::Ash => (&trace.primers.ash, &trace.primers.ash_tools),
            AgentArm::NativeShell => (
                &trace.primers.native_shell,
                &trace.primers.native_shell_tools,
            ),
        };
        let mut normalized_input = sum_text_measurements(
            [
                trace.primers.shared.as_str(),
                arm_primer.as_str(),
                arm_tools.as_str(),
            ]
            .into_iter(),
            &cl100k,
            &o200k,
        );
        let mut normalized_output = Measurement::default();
        let mut provider_usage = ProviderUsageTotal {
            hidden_reasoning_tokens: Some(0),
            ..ProviderUsageTotal::default()
        };
        let mut task_reports = Vec::with_capacity(run.tasks.len());
        let mut tool_calls = 0_usize;
        let mut executed_operations = 0_usize;
        let mut failed_attempts = 0_usize;
        let mut retries = 0_usize;
        let mut model_elapsed_millis = 0_u64;
        let mut successful_tasks = 0_usize;
        for task_trace in &run.tasks {
            let (task, locked) = tasks_by_id
                .get(task_trace.id.as_str())
                .copied()
                .ok_or_else(|| io::Error::other("agent trace references an unknown task"))?;
            let replayed = match run.arm {
                AgentArm::Ash => replay_ash_task(task, locked, task_trace, &cl100k, &o200k).await?,
                AgentArm::NativeShell => {
                    replay_native_task(task, locked, task_trace, &cl100k, &o200k).await?
                }
            };
            add_measurement(&mut normalized_input, &replayed.report.normalized_input);
            add_measurement(&mut normalized_output, &replayed.report.normalized_output);
            add_provider_usage(&mut provider_usage, &replayed.report.provider_usage)?;
            tool_calls += replayed.report.tool_calls;
            executed_operations += replayed.report.executed_operations;
            failed_attempts += replayed.report.failed_attempts;
            retries += replayed.report.retries;
            model_elapsed_millis = model_elapsed_millis
                .checked_add(replayed.report.model_elapsed_millis)
                .ok_or_else(|| io::Error::other("agent elapsed time overflow"))?;
            successful_tasks += usize::from(replayed.report.success);
            task_reports.push(replayed.report);
        }
        let normalized_total = sum_measurements([&normalized_input, &normalized_output]);
        runs.push(AgentRunReport {
            arm: run.arm,
            repetition: run.repetition,
            seed: run.seed,
            task_order: run.tasks.iter().map(|task| task.id.clone()).collect(),
            tasks: task_reports,
            provider_usage,
            normalized_input,
            normalized_output,
            normalized_total,
            tool_calls,
            executed_operations,
            failed_attempts,
            retries,
            model_elapsed_millis,
            replay_elapsed_ns: replay_started.elapsed().as_nanos().max(1),
            successful_tasks,
            success: successful_tasks == run.tasks.len(),
        });
    }
    let summary = build_summary(&runs)?;
    let all_tasks_success = runs.iter().all(|run| run.success);
    let comparable_success = summary.comparison.valid;
    Ok(AgentCorpusReport {
        schema: REPORT_SCHEMA,
        evidence_kind: "model-selected-trace-replay",
        agent_results: true,
        provenance: "external-self-attested-trace",
        provider_attestation_verified: false,
        audit_verified: true,
        trace_sha256: sha256_hex(&trace_bytes),
        audit_sha256: trace.audit_sha256,
        experiment_id: trace.experiment_id,
        captured_at_utc: trace.captured_at_utc,
        corpus: "benches/tasks/v1/manifest.json",
        manifest_sha256: trace.manifest_sha256,
        lock_sha256: trace.lock_sha256,
        platform: trace.platform,
        architecture: trace.architecture,
        driver: trace.driver,
        model: trace.model,
        repetitions: trace.repetitions,
        provider_accounting: "provider-input+visible-model-output; cached input remains included; hidden reasoning excluded",
        normalized_accounting: "shared-primer+arm-primer+tool-schema+task-prompts+tool-results+model-requests+final-output",
        hidden_reasoning_tokens_included: false,
        tokenizers: [
            "tiktoken-rs/0.12.0:cl100k_base",
            "tiktoken-rs/0.12.0:o200k_base",
        ],
        primers,
        runs,
        summary,
        gates: AgentGates {
            strict_trace_valid: true,
            corpus_lock_match: true,
            paired_runs: true,
            tool_result_hashes_match: true,
            all_tasks_success,
            comparable_success,
            passed: all_tasks_success && comparable_success,
        },
    })
}

fn read_trace(
    path: &Path,
    manifest: &TaskManifest,
    lock: &TaskCorpusLock,
) -> Result<(AgentTrace, Vec<u8>), Box<dyn Error>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_TRACE_BYTES {
        return Err(io::Error::other("agent trace size is invalid").into());
    }
    let bytes = fs::read(path)?;
    if u64::try_from(bytes.len())? > MAX_TRACE_BYTES {
        return Err(io::Error::other("agent trace exceeds its byte ceiling").into());
    }
    let trace: AgentTrace = serde_json::from_slice(&bytes)?;
    validate_trace(&trace, manifest, lock)?;
    Ok((trace, bytes))
}

fn validate_trace(
    trace: &AgentTrace,
    manifest: &TaskManifest,
    lock: &TaskCorpusLock,
) -> Result<(), io::Error> {
    if trace.schema != TRACE_SCHEMA
        || trace.evidence_kind != "model-selected-trace"
        || !valid_name(&trace.experiment_id)
        || !valid_timestamp(&trace.captured_at_utc)
        || trace.manifest_sha256 != sha256_hex(MANIFEST_BYTES)
        || trace.lock_sha256 != sha256_hex(LOCK_BYTES)
        || trace.platform != std::env::consts::OS
        || trace.architecture != std::env::consts::ARCH
        || trace.repetitions == 0
        || trace.repetitions > MAX_REPETITIONS
        || trace.runs.len() != usize::from(trace.repetitions) * 2
    {
        return Err(io::Error::other("agent trace header is invalid"));
    }
    validate_metadata(&trace.driver.name)?;
    validate_metadata(&trace.driver.version)?;
    if !valid_sha256(&trace.driver.source_sha256) {
        return Err(io::Error::other("agent driver digest is invalid"));
    }
    validate_metadata(&trace.model.provider)?;
    validate_metadata(&trace.model.id)?;
    validate_metadata(&trace.model.revision)?;
    validate_metadata(&trace.model.temperature)?;
    validate_metadata(&trace.model.top_p)?;
    validate_metadata(&trace.model.reasoning_effort)?;
    if trace.model.context_tokens == 0
        || trace.model.context_tokens > MAX_PROVIDER_TOKENS
        || trace.model.max_output_tokens == 0
        || trace.model.max_output_tokens > trace.model.context_tokens
    {
        return Err(io::Error::other("agent model limits are invalid"));
    }
    validate_visible_text(&trace.primers.shared, false)?;
    validate_visible_text(&trace.primers.ash, false)?;
    validate_visible_text(&trace.primers.native_shell, false)?;
    validate_visible_text(&trace.primers.ash_tools, false)?;
    validate_visible_text(&trace.primers.native_shell_tools, false)?;
    if !valid_sha256(&trace.audit_sha256) {
        return Err(io::Error::other("agent audit digest is invalid"));
    }

    let declared_tasks = manifest
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<BTreeMap<_, _>>();
    let mut runs = BTreeMap::new();
    for run in &trace.runs {
        if run.repetition >= trace.repetitions
            || runs.insert((run.repetition, run.arm), run).is_some()
            || run.tasks.len() != manifest.tasks.len()
        {
            return Err(io::Error::other("agent run matrix is invalid"));
        }
        let mut ids = BTreeSet::new();
        for task in &run.tasks {
            let Some(definition) = declared_tasks.get(task.id.as_str()).copied() else {
                return Err(io::Error::other("agent run task set is invalid"));
            };
            if !ids.insert(task.id.as_str()) {
                return Err(io::Error::other("agent run task set is invalid"));
            }
            validate_task_trace(task, definition, run.arm)?;
        }
    }
    for repetition in 0..trace.repetitions {
        let ash = runs
            .get(&(repetition, AgentArm::Ash))
            .ok_or_else(|| io::Error::other("agent ASH run is missing"))?;
        let native = runs
            .get(&(repetition, AgentArm::NativeShell))
            .ok_or_else(|| io::Error::other("agent native-shell run is missing"))?;
        if ash.seed != native.seed
            || !ash
                .tasks
                .iter()
                .zip(&native.tasks)
                .all(|(left, right)| left.id == right.id)
        {
            return Err(io::Error::other("agent paired task order or seed differs"));
        }
    }
    let expected_order = (0..trace.repetitions).flat_map(|repetition| {
        [AgentArm::Ash, AgentArm::NativeShell]
            .into_iter()
            .map(move |arm| (repetition, arm))
    });
    if !trace
        .runs
        .iter()
        .map(|run| (run.repetition, run.arm))
        .eq(expected_order)
    {
        return Err(io::Error::other(
            "agent runs are not in canonical pair order",
        ));
    }
    if lock.tasks.len() != manifest.tasks.len() {
        return Err(io::Error::other("task lock and manifest differ"));
    }
    Ok(())
}

fn validate_task_trace(
    task: &AgentTaskTrace,
    definition: &TaskDefinition,
    arm: AgentArm,
) -> Result<(), io::Error> {
    if task.prompt != agent_task_prompt(definition)
        || task.attempts.is_empty()
        || task.attempts.len() > MAX_ATTEMPTS
        || task.finish_elapsed_millis > MAX_MODEL_ELAPSED_MILLIS
    {
        return Err(io::Error::other(
            "agent task attempt count or duration is invalid",
        ));
    }
    validate_visible_text(&task.final_stdout, true)?;
    validate_visible_text(&task.final_stderr, true)?;
    validate_visible_text(&task.prompt, false)?;
    if !valid_sha256(&task.finish_request_sha256) || !valid_sha256(&task.finish_response_sha256) {
        return Err(io::Error::other("agent finish exchange digest is invalid"));
    }
    if task
        .final_stdout
        .len()
        .saturating_add(task.final_stderr.len())
        > definition.limits.output_bytes
    {
        return Err(io::Error::other(
            "agent final output exceeds its byte ceiling",
        ));
    }
    validate_usage(&task.usage)?;
    for attempt in &task.attempts {
        validate_visible_text(&attempt.model_output, false)?;
        if !valid_sha256(&attempt.tool_result_sha256)
            || !valid_sha256(&attempt.provider_request_sha256)
            || !valid_sha256(&attempt.provider_response_sha256)
            || attempt.model_elapsed_millis > MAX_MODEL_ELAPSED_MILLIS
            || (arm == AgentArm::NativeShell && attempt.kind != AttemptKind::Request)
        {
            return Err(io::Error::other("agent attempt is invalid"));
        }
    }
    Ok(())
}

fn agent_task_prompt(task: &TaskDefinition) -> String {
    format!(
        "id:{}\nobjective:{}\ncapabilities:[{}]\nlimits{{ms,out}}:{},{}\n",
        task.id,
        task.objective,
        task.capabilities.join(","),
        task.limits.millis,
        task.limits.output_bytes,
    )
}

fn validate_usage(usage: &ProviderUsage) -> Result<(), io::Error> {
    if usage.input_tokens == 0
        || usage.input_tokens > MAX_PROVIDER_TOKENS
        || usage.visible_output_tokens == 0
        || usage.visible_output_tokens > MAX_PROVIDER_TOKENS
        || usage.cached_input_tokens > usage.input_tokens
        || usage
            .hidden_reasoning_tokens
            .is_some_and(|tokens| tokens > MAX_PROVIDER_TOKENS)
        || !valid_sha256(&usage.raw_usage_sha256)
    {
        return Err(io::Error::other("provider usage evidence is invalid"));
    }
    let _ = usage.visible_total()?;
    Ok(())
}

fn validate_metadata(value: &str) -> Result<(), io::Error> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(io::Error::other("agent metadata text is invalid"))
    } else {
        Ok(())
    }
}

fn validate_visible_text(value: &str, empty_allowed: bool) -> Result<(), io::Error> {
    if (!empty_allowed && value.is_empty())
        || value.len() > MAX_TEXT_BYTES
        || value.contains('\r')
        || value.contains('\0')
    {
        Err(io::Error::other("agent-visible text is invalid"))
    } else {
        Ok(())
    }
}

fn valid_timestamp(value: &str) -> bool {
    if value.len() < 20 || value.len() > 64 || !value.ends_with('Z') || !value.is_ascii() {
        return false;
    }
    let core = &value[..value.len() - 1];
    let (base, fraction) = core
        .split_once('.')
        .map_or((core, None), |(base, fraction)| (base, Some(fraction)));
    if base.len() != 19
        || base.as_bytes()[4] != b'-'
        || base.as_bytes()[7] != b'-'
        || base.as_bytes()[10] != b'T'
        || base.as_bytes()[13] != b':'
        || base.as_bytes()[16] != b':'
        || fraction.is_some_and(|digits| {
            digits.is_empty()
                || digits.len() > 9
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return false;
    }
    let digits =
        |range: std::ops::Range<usize>| base.get(range).and_then(|part| part.parse::<u32>().ok());
    matches!(digits(0..4), Some(1..=9999))
        && matches!(digits(5..7), Some(1..=12))
        && matches!(digits(8..10), Some(1..=31))
        && matches!(digits(11..13), Some(0..=23))
        && matches!(digits(14..16), Some(0..=59))
        && matches!(digits(17..19), Some(0..=60))
}

async fn replay_ash_task(
    task: &TaskDefinition,
    locked: &super::TaskLockEntry,
    trace: &AgentTaskTrace,
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Result<ReplayedTask, Box<dyn Error>> {
    let fixture = fixture_path(&task.workspace)?;
    let directory = TempDir::new()?;
    copy_workspace(&fixture, directory.path())?;
    let initial_tree_sha256 = tree_sha256(directory.path())?;
    if initial_tree_sha256 != locked.initial_tree_sha256 {
        return Err(io::Error::other(format!(
            "agent ASH initial tree changed for task {}",
            task.id
        ))
        .into());
    }
    let workspace = directory
        .path()
        .to_str()
        .ok_or_else(|| io::Error::other("agent task workspace path is not UTF-8"))?;
    let session = ExecutionSession::open(
        1,
        workspace,
        u64::try_from(task.limits.output_bytes)?,
        Parallelism::detected(),
        task_capability_mask(task),
    )?;
    let started = Instant::now();
    let execution = replay_ash_attempts(task, trace, &session, started, cl100k, o200k).await;
    let close = session.close();
    let (
        attempts,
        normalized_input,
        mut normalized_output,
        executed_operations,
        failed_attempts,
        retries,
    ) = execution?;
    close?;
    add_measurement(
        &mut normalized_output,
        &sum_text_measurements(
            [trace.final_stdout.as_str(), trace.final_stderr.as_str()].into_iter(),
            cl100k,
            o200k,
        ),
    );
    let final_tree_sha256 = tree_sha256(directory.path())?;
    finish_task_report(
        task,
        locked,
        trace,
        attempts,
        normalized_input,
        normalized_output,
        executed_operations,
        failed_attempts,
        retries,
        initial_tree_sha256,
        final_tree_sha256,
        directory.path(),
        started.elapsed().as_nanos().max(1),
    )
}

async fn replay_ash_attempts(
    task: &TaskDefinition,
    trace: &AgentTaskTrace,
    session: &ExecutionSession,
    started: Instant,
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Result<
    (
        Vec<AgentAttemptReport>,
        Measurement,
        Measurement,
        usize,
        usize,
        usize,
    ),
    Box<dyn Error>,
> {
    let mut reports = Vec::with_capacity(trace.attempts.len());
    let mut normalized_input = measure(&trace.prompt, cl100k, o200k);
    let mut normalized_output = Measurement::default();
    let mut visible_bytes = trace.prompt.len();
    let mut executed_operations = 0_usize;
    let mut failed_attempts = 0_usize;
    let mut retries = 0_usize;
    let mut retry_pending = false;
    for (index, attempt) in trace.attempts.iter().enumerate() {
        if retry_pending {
            retries += 1;
        }
        let attempt_started = Instant::now();
        let model_output = measure(&attempt.model_output, cl100k, o200k);
        add_measurement(&mut normalized_output, &model_output);
        visible_bytes = visible_bytes
            .checked_add(attempt.model_output.len())
            .ok_or_else(|| io::Error::other("agent task visible-byte count overflow"))?;
        let parsed = parse_agent_request(&attempt.model_output);
        let (tool_result, operation, outcome, failed) = match attempt.kind {
            AttemptKind::InvalidRequest => {
                if parsed.is_ok() {
                    return Err(io::Error::other(
                        "invalid-request attempt contains a valid request",
                    )
                    .into());
                }
                (
                    INVALID_REQUEST_RESULT.to_owned(),
                    None,
                    "invalid-request".to_owned(),
                    true,
                )
            }
            AttemptKind::PolicyRejected => {
                let request = parsed.map_err(|_| {
                    io::Error::other("policy-rejected attempt does not contain a valid request")
                })?;
                if request_allowed(task, &request) {
                    return Err(
                        io::Error::other("policy-rejected request satisfies task policy").into(),
                    );
                }
                (
                    POLICY_REJECTED_RESULT.to_owned(),
                    Some(operation_name(request.operation())),
                    "policy-rejected".to_owned(),
                    true,
                )
            }
            AttemptKind::Request => {
                let request = parsed
                    .map_err(|_| io::Error::other("request attempt is not canonical ASH/1 ASON"))?;
                if !request_allowed(task, &request) {
                    return Err(
                        io::Error::other("request attempt violates declared task policy").into(),
                    );
                }
                let elapsed_millis =
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let remaining = task
                    .limits
                    .millis
                    .checked_sub(elapsed_millis)
                    .filter(|remaining| *remaining > 0)
                    .ok_or_else(|| io::Error::other("agent ASH replay exceeded task deadline"))?;
                let response = tokio::time::timeout(
                    Duration::from_millis(remaining),
                    session.execute(&request),
                )
                .await
                .map_err(|_| io::Error::other("agent ASH operation exceeded task deadline"))??;
                executed_operations += 1;
                let failed = response.status() != Status::Success;
                let status = response.status().code();
                (
                    response.encode()?.encode(),
                    Some(operation_name(request.operation())),
                    format!("status-{status}"),
                    failed,
                )
            }
        };
        if failed {
            failed_attempts += 1;
        }
        retry_pending = failed;
        if sha256_hex(tool_result.as_bytes()) != attempt.tool_result_sha256 {
            return Err(io::Error::other(format!(
                "agent ASH tool-result digest differs for task {} attempt {index}",
                task.id
            ))
            .into());
        }
        let tool_measurement = measure(&tool_result, cl100k, o200k);
        add_measurement(&mut normalized_input, &tool_measurement);
        visible_bytes = visible_bytes
            .checked_add(tool_result.len())
            .ok_or_else(|| io::Error::other("agent task visible-byte count overflow"))?;
        if visible_bytes > MAX_TASK_VISIBLE_BYTES {
            return Err(
                io::Error::other("agent task visible evidence exceeds its byte ceiling").into(),
            );
        }
        reports.push(AgentAttemptReport {
            index,
            kind: attempt.kind,
            operation,
            outcome,
            model_output,
            tool_result: tool_measurement,
            model_output_sha256: sha256_hex(attempt.model_output.as_bytes()),
            tool_result_sha256: attempt.tool_result_sha256.clone(),
            provider_request_sha256: attempt.provider_request_sha256.clone(),
            provider_response_sha256: attempt.provider_response_sha256.clone(),
            raw_stdout_sha256: None,
            raw_stderr_sha256: None,
            model_elapsed_millis: attempt.model_elapsed_millis,
            replay_elapsed_ns: attempt_started.elapsed().as_nanos().max(1),
        });
    }
    Ok((
        reports,
        normalized_input,
        normalized_output,
        executed_operations,
        failed_attempts,
        retries,
    ))
}

async fn replay_native_task(
    task: &TaskDefinition,
    locked: &super::TaskLockEntry,
    trace: &AgentTaskTrace,
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Result<ReplayedTask, Box<dyn Error>> {
    let fixture = fixture_path(&task.workspace)?;
    let directory = TempDir::new()?;
    copy_workspace(&fixture, directory.path())?;
    let initial_tree_sha256 = tree_sha256(directory.path())?;
    if initial_tree_sha256 != locked.initial_tree_sha256 {
        return Err(io::Error::other(format!(
            "agent native-shell initial tree changed for task {}",
            task.id
        ))
        .into());
    }
    let started = Instant::now();
    let mut reports = Vec::with_capacity(trace.attempts.len());
    let mut normalized_input = measure(&trace.prompt, cl100k, o200k);
    let mut normalized_output = Measurement::default();
    let mut visible_bytes = trace.prompt.len();
    let mut failed_attempts = 0_usize;
    let mut retries = 0_usize;
    let mut retry_pending = false;
    for (index, attempt) in trace.attempts.iter().enumerate() {
        if retry_pending {
            retries += 1;
        }
        let model_output = measure(&attempt.model_output, cl100k, o200k);
        add_measurement(&mut normalized_output, &model_output);
        visible_bytes = visible_bytes
            .checked_add(attempt.model_output.len())
            .ok_or_else(|| io::Error::other("agent task visible-byte count overflow"))?;
        let elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let remaining = task
            .limits
            .millis
            .checked_sub(elapsed_millis)
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| io::Error::other("agent native-shell replay exceeded task deadline"))?;
        let process = run_native_agent_command(
            directory.path(),
            &attempt.model_output,
            task.limits.output_bytes,
            remaining,
        )
        .await?;
        let tool_result = native_tool_result(&process);
        if sha256_hex(tool_result.as_bytes()) != attempt.tool_result_sha256 {
            return Err(io::Error::other(format!(
                "agent native-shell tool-result digest differs for task {} attempt {index}",
                task.id
            ))
            .into());
        }
        let failed = process.timed_out || process.output_exceeded || process.exit_code != Some(0);
        if failed {
            failed_attempts += 1;
        }
        retry_pending = failed;
        let tool_measurement = measure(&tool_result, cl100k, o200k);
        add_measurement(&mut normalized_input, &tool_measurement);
        visible_bytes = visible_bytes
            .checked_add(tool_result.len())
            .ok_or_else(|| io::Error::other("agent task visible-byte count overflow"))?;
        if visible_bytes > MAX_TASK_VISIBLE_BYTES {
            return Err(
                io::Error::other("agent task visible evidence exceeds its byte ceiling").into(),
            );
        }
        let outcome = if process.timed_out {
            "timeout".to_owned()
        } else if process.output_exceeded {
            "output-limit".to_owned()
        } else {
            process
                .exit_code
                .map_or_else(|| "signal".to_owned(), |code| format!("exit-{code}"))
        };
        reports.push(AgentAttemptReport {
            index,
            kind: attempt.kind,
            operation: Some("native-shell"),
            outcome,
            model_output,
            tool_result: tool_measurement,
            model_output_sha256: sha256_hex(attempt.model_output.as_bytes()),
            tool_result_sha256: attempt.tool_result_sha256.clone(),
            provider_request_sha256: attempt.provider_request_sha256.clone(),
            provider_response_sha256: attempt.provider_response_sha256.clone(),
            raw_stdout_sha256: Some(sha256_hex(&process.stdout)),
            raw_stderr_sha256: Some(sha256_hex(&process.stderr)),
            model_elapsed_millis: attempt.model_elapsed_millis,
            replay_elapsed_ns: process.elapsed_ns,
        });
    }
    add_measurement(
        &mut normalized_output,
        &sum_text_measurements(
            [trace.final_stdout.as_str(), trace.final_stderr.as_str()].into_iter(),
            cl100k,
            o200k,
        ),
    );
    let final_tree_sha256 = tree_sha256(directory.path())?;
    finish_task_report(
        task,
        locked,
        trace,
        reports,
        normalized_input,
        normalized_output,
        trace.attempts.len(),
        failed_attempts,
        retries,
        initial_tree_sha256,
        final_tree_sha256,
        directory.path(),
        started.elapsed().as_nanos().max(1),
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_task_report(
    task: &TaskDefinition,
    locked: &super::TaskLockEntry,
    trace: &AgentTaskTrace,
    attempts: Vec<AgentAttemptReport>,
    normalized_input: Measurement,
    normalized_output: Measurement,
    executed_operations: usize,
    failed_attempts: usize,
    retries: usize,
    initial_tree_sha256: String,
    final_tree_sha256: String,
    workspace: &Path,
    replay_elapsed_ns: u128,
) -> Result<ReplayedTask, Box<dyn Error>> {
    let semantic_output_match =
        trace.final_stdout == task.expected.stdout && trace.final_stderr == task.expected.stderr;
    let expected_files_match = task.expected.files.iter().all(|expected| {
        confined_path(workspace, &expected.path)
            .ok()
            .and_then(|path| fs::read(path).ok())
            .is_some_and(|content| content == expected.content.as_bytes())
    });
    let final_tree_match = final_tree_sha256 == locked.expected_final_tree_sha256;
    let success = semantic_output_match && expected_files_match && final_tree_match;
    let normalized_total = sum_measurements([&normalized_input, &normalized_output]);
    let provider_visible_tokens = trace.usage.visible_total()?;
    let model_elapsed_millis =
        trace
            .attempts
            .iter()
            .try_fold(trace.finish_elapsed_millis, |total, attempt| {
                total
                    .checked_add(attempt.model_elapsed_millis)
                    .ok_or_else(|| io::Error::other("agent model elapsed time overflow"))
            })?;
    Ok(ReplayedTask {
        report: AgentTaskReport {
            id: task.id.clone(),
            family: task.family.clone(),
            declared_initial_tree_sha256: locked.initial_tree_sha256.clone(),
            actual_initial_tree_sha256: initial_tree_sha256,
            expected_final_tree_sha256: locked.expected_final_tree_sha256.clone(),
            actual_final_tree_sha256: final_tree_sha256,
            tool_calls: attempts.len(),
            executed_operations,
            failed_attempts,
            retries,
            attempts,
            provider_usage: trace.usage.clone(),
            provider_visible_tokens,
            normalized_input,
            normalized_output,
            normalized_total,
            model_elapsed_millis,
            replay_elapsed_ns,
            final_stdout_sha256: sha256_hex(trace.final_stdout.as_bytes()),
            final_stderr_sha256: sha256_hex(trace.final_stderr.as_bytes()),
            finish_request_sha256: trace.finish_request_sha256.clone(),
            finish_response_sha256: trace.finish_response_sha256.clone(),
            semantic_output_match,
            expected_files_match,
            final_tree_match,
            success,
        },
    })
}

fn parse_agent_request(text: &str) -> Result<Request, io::Error> {
    let document = decode_with_limits(text, &Limits::default())
        .map_err(|_| io::Error::other("agent output is not valid ASON"))?;
    if document.encode() != text {
        return Err(io::Error::other("agent request is not canonical ASON"));
    }
    Request::decode(&document).map_err(|_| io::Error::other("agent request schema is invalid"))
}

fn request_allowed(task: &TaskDefinition, request: &Request) -> bool {
    request.permit().is_none()
        && request.budget().tokens() <= u32::try_from(task.limits.output_bytes).unwrap_or(u32::MAX)
        && request.budget().records() <= u32::try_from(task.limits.output_bytes).unwrap_or(u32::MAX)
        && request.budget().millis() <= task.limits.millis
        && arguments_allowed(task, request.arguments())
        && request.required_capabilities() & !task_capability_mask(task) == 0
}

fn arguments_allowed(task: &TaskDefinition, arguments: &Arguments) -> bool {
    let declared = |name: &str| task.capabilities.iter().any(|value| value == name);
    match arguments {
        Arguments::Exec(_) => declared("exec"),
        Arguments::Read(_) => declared("read"),
        Arguments::List(_) => declared("list"),
        Arguments::Search(_) => declared("search"),
        Arguments::Patch(_) => declared("patch"),
        Arguments::Fs(_) => declared("fs"),
        Arguments::Snapshot(_) => declared("snapshot"),
        Arguments::Ref(_) => declared("ref"),
        Arguments::Batch(batch) => {
            declared("batch")
                && batch
                    .nodes()
                    .iter()
                    .all(|node| arguments_allowed(task, node.arguments()))
        }
        Arguments::Cancel(_) => false,
    }
}

fn task_capability_mask(task: &TaskDefinition) -> u64 {
    task.capabilities.iter().fold(0, |mask, operation| {
        mask | match operation.as_str() {
            "exec" => Capability::HostProcess.mask(),
            "read" | "list" | "search" | "snapshot" => Capability::WorkspaceRead.mask(),
            "patch" | "fs" => Capability::WorkspaceWrite.mask(),
            "ref" => Capability::RetainedResult.mask(),
            _ => 0,
        }
    })
}

const fn operation_name(operation: Operation) -> &'static str {
    match operation {
        Operation::Exec => "exec",
        Operation::Read => "read",
        Operation::List => "list",
        Operation::Search => "search",
        Operation::Patch => "patch",
        Operation::Fs => "fs",
        Operation::Batch => "batch",
        Operation::RefBytes
        | Operation::RefLines
        | Operation::RefSearch
        | Operation::RefRelease
        | Operation::RefProject
        | Operation::RefMaterialize => "ref",
        Operation::Snapshot => "snapshot",
        Operation::Cancel => "cancel",
    }
}

async fn run_native_agent_command(
    workspace: &Path,
    script: &str,
    output_limit: usize,
    remaining_millis: u64,
) -> Result<NativeProcessOutput, Box<dyn Error>> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!("$ErrorActionPreference = 'Stop'; {script}"),
        ]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-eu", "-c", script]);
        command
    };
    command
        .current_dir(workspace)
        .env_clear()
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
        "TEMP",
        "TMP",
        "PSModulePath",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let started = Instant::now();
    let mut child = command.spawn()?;
    let streams = (child.stdout.take(), child.stderr.take());
    let (Some(stdout), Some(stderr)) = streams else {
        terminate(&mut child).await;
        return Err(io::Error::other("agent native-shell pipes were not configured").into());
    };
    let mut stdout_task = tokio::spawn(read_capped(stdout, output_limit));
    let mut stderr_task = tokio::spawn(read_capped(stderr, output_limit));
    let wait = tokio::time::timeout(Duration::from_millis(remaining_millis), child.wait()).await;
    let (status, timed_out) = match wait {
        Ok(status) => (Some(status?), false),
        Err(_) => {
            terminate(&mut child).await;
            (None, true)
        }
    };
    let stdout = captured_reader(&mut stdout_task, "stdout").await?;
    let stderr = captured_reader(&mut stderr_task, "stderr").await?;
    let total_bytes = stdout.total_bytes.saturating_add(stderr.total_bytes);
    Ok(NativeProcessOutput {
        exit_code: status.and_then(|status| status.code()),
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        timed_out,
        output_exceeded: total_bytes > output_limit,
        elapsed_ns: started.elapsed().as_nanos().max(1),
    })
}

async fn read_capped(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<CapturedStream, io::Error> {
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut total_bytes = 0_usize;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read);
        let remaining = limit.saturating_sub(captured.len());
        captured.extend_from_slice(&chunk[..read.min(remaining)]);
    }
    Ok(CapturedStream {
        bytes: captured,
        total_bytes,
    })
}

async fn captured_reader(
    task: &mut JoinHandle<Result<CapturedStream, io::Error>>,
    stream: &str,
) -> Result<CapturedStream, Box<dyn Error>> {
    match tokio::time::timeout(CLEANUP_TIMEOUT, &mut *task).await {
        Ok(result) => Ok(result??),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(io::Error::other(format!(
                "agent native-shell {stream} drain exceeded its bound"
            ))
            .into())
        }
    }
}

fn native_tool_result(output: &NativeProcessOutput) -> String {
    let state = if output.timed_out {
        "timeout".to_owned()
    } else if output.output_exceeded {
        "output-limit".to_owned()
    } else {
        output
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| code.to_string())
    };
    let stdout = normalize_agent_stream(&output.stdout);
    let stderr = normalize_agent_stream(&output.stderr);
    format!(
        "exit:{state}\nstdout:{}\n{}stderr:{}\n{}",
        stdout.len(),
        stdout,
        stderr.len(),
        stderr
    )
}

fn normalize_agent_stream(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn text_evidence(
    text: &str,
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> TextEvidence {
    TextEvidence {
        sha256: sha256_hex(text.as_bytes()),
        measurement: measure(text, cl100k, o200k),
    }
}

fn add_provider_usage(
    total: &mut ProviderUsageTotal,
    value: &ProviderUsage,
) -> Result<(), io::Error> {
    total.input_tokens = total
        .input_tokens
        .checked_add(value.input_tokens)
        .ok_or_else(|| io::Error::other("provider input Token total overflow"))?;
    total.cached_input_tokens = total
        .cached_input_tokens
        .checked_add(value.cached_input_tokens)
        .ok_or_else(|| io::Error::other("provider cached Token total overflow"))?;
    total.visible_output_tokens = total
        .visible_output_tokens
        .checked_add(value.visible_output_tokens)
        .ok_or_else(|| io::Error::other("provider output Token total overflow"))?;
    total.visible_total_tokens = total
        .visible_total_tokens
        .checked_add(value.visible_total()?)
        .ok_or_else(|| io::Error::other("provider visible Token total overflow"))?;
    total.hidden_reasoning_tokens =
        match (total.hidden_reasoning_tokens, value.hidden_reasoning_tokens) {
            (Some(left), Some(right)) => Some(
                left.checked_add(right)
                    .ok_or_else(|| io::Error::other("provider reasoning Token total overflow"))?,
            ),
            _ => None,
        };
    Ok(())
}

fn build_summary(runs: &[AgentRunReport]) -> Result<AgentSummary, io::Error> {
    let ash = summarize_arm(runs, AgentArm::Ash)?;
    let native_shell = summarize_arm(runs, AgentArm::NativeShell)?;
    let success_rate_gap_basis_points = ash
        .success_basis_points
        .abs_diff(native_shell.success_basis_points);
    let valid = success_rate_gap_basis_points <= 100
        && ash.successful_tasks > 0
        && native_shell.successful_tasks > 0;
    let comparison = AgentComparison {
        valid,
        success_rate_gap_basis_points,
        ash_vs_native_shell_median_provider_visible_tokens_percent: valid
            .then(|| {
                ratio_percent_u64(
                    ash.median_provider_visible_tokens_per_successful_task?,
                    native_shell.median_provider_visible_tokens_per_successful_task?,
                )
            })
            .flatten(),
        ash_vs_native_shell_normalized_cl100k_tokens_percent: valid.then(|| {
            ratio_percent(
                ash.normalized_total.cl100k_tokens,
                native_shell.normalized_total.cl100k_tokens,
            )
        }),
        ash_vs_native_shell_normalized_o200k_tokens_percent: valid.then(|| {
            ratio_percent(
                ash.normalized_total.o200k_tokens,
                native_shell.normalized_total.o200k_tokens,
            )
        }),
    };
    Ok(AgentSummary {
        ash,
        native_shell,
        comparison,
    })
}

fn summarize_arm(runs: &[AgentRunReport], arm: AgentArm) -> Result<AgentArmSummary, io::Error> {
    let selected = runs.iter().filter(|run| run.arm == arm).collect::<Vec<_>>();
    let mut provider_usage = ProviderUsageTotal {
        hidden_reasoning_tokens: Some(0),
        ..ProviderUsageTotal::default()
    };
    let mut normalized_total = Measurement::default();
    let mut successful_provider = Vec::new();
    let mut successful_cl100k = Vec::new();
    let mut successful_o200k = Vec::new();
    let mut tasks = 0_usize;
    let mut successful_tasks = 0_usize;
    let mut tool_calls = 0_usize;
    let mut executed_operations = 0_usize;
    let mut failed_attempts = 0_usize;
    let mut retries = 0_usize;
    let mut model_elapsed_millis = 0_u64;
    let mut replay_elapsed_ns = 0_u128;
    for run in &selected {
        tasks += run.tasks.len();
        successful_tasks += run.successful_tasks;
        tool_calls += run.tool_calls;
        executed_operations += run.executed_operations;
        failed_attempts += run.failed_attempts;
        retries += run.retries;
        model_elapsed_millis = model_elapsed_millis
            .checked_add(run.model_elapsed_millis)
            .ok_or_else(|| io::Error::other("agent summary elapsed time overflow"))?;
        replay_elapsed_ns = replay_elapsed_ns
            .checked_add(run.replay_elapsed_ns)
            .ok_or_else(|| io::Error::other("agent replay time overflow"))?;
        add_measurement(&mut normalized_total, &run.normalized_total);
        for task in &run.tasks {
            add_provider_usage(&mut provider_usage, &task.provider_usage)?;
            if task.success {
                successful_provider.push(task.provider_visible_tokens);
                successful_cl100k.push(task.normalized_total.cl100k_tokens);
                successful_o200k.push(task.normalized_total.o200k_tokens);
            }
        }
    }
    let success_basis_points = successful_tasks
        .saturating_mul(10_000)
        .checked_div(tasks)
        .unwrap_or(0);
    Ok(AgentArmSummary {
        runs: selected.len(),
        tasks,
        successful_tasks,
        success_basis_points,
        tool_calls,
        executed_operations,
        failed_attempts,
        retries,
        provider_usage,
        median_provider_visible_tokens_per_successful_task: median(&mut successful_provider),
        normalized_total,
        median_normalized_cl100k_tokens_per_successful_task: median(&mut successful_cl100k),
        median_normalized_o200k_tokens_per_successful_task: median(&mut successful_o200k),
        model_elapsed_millis,
        replay_elapsed_ns,
    })
}

fn median<T: Ord + Copy>(values: &mut [T]) -> Option<T> {
    if values.is_empty() {
        None
    } else {
        values.sort_unstable();
        Some(values[(values.len() - 1) / 2])
    }
}

fn ratio_percent_u64(value: u64, baseline: u64) -> Option<usize> {
    if baseline == 0 {
        None
    } else {
        usize::try_from(value.saturating_mul(100).div_ceil(baseline)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentArm, AgentAttemptTrace, AgentAuditRecord, AgentRunTrace, AgentTaskTrace, AgentTrace,
        AttemptKind, DriverMetadata, INVALID_REQUEST_RESULT, ModelMetadata, NativeProcessOutput,
        PrimerSet, ProviderUsage, agent_task_prompt, build_agent_report, native_tool_result,
        normalize_agent_stream, parse_agent_request, ratio_percent_u64, replay_ash_task,
        replay_native_task, run_native_agent_command, valid_timestamp, validate_agent_trace_audit,
        validate_trace,
    };
    use crate::tasks::{
        LOCK_BYTES, MANIFEST_BYTES, TaskDefinition, copy_workspace, execute_ash_task, fixture_path,
        load_lock, load_manifest, sha256_hex,
    };

    async fn capture_native_result(task: &TaskDefinition) -> String {
        let fixture = fixture_path(&task.workspace).expect("native fixture");
        let directory = tempfile::TempDir::new().expect("native capture workspace");
        copy_workspace(&fixture, directory.path()).expect("copy native fixture");
        let baseline = task.baselines.current().expect("platform baseline");
        let output = run_native_agent_command(
            directory.path(),
            &baseline.script,
            task.limits.output_bytes,
            task.limits.millis,
        )
        .await
        .expect("isolated native capture");
        let stdout = normalize_agent_stream(&output.stdout);
        let stderr = normalize_agent_stream(&output.stderr);
        assert!(
            !output.timed_out && !output.output_exceeded && output.exit_code == Some(0),
            "isolated native capture failed for {}: exit={:?}, stdout={stdout:?}, stderr={stderr:?}",
            task.id,
            output.exit_code
        );
        assert_eq!(
            stdout, task.expected.stdout,
            "native stdout for {}",
            task.id
        );
        assert_eq!(
            stderr, task.expected.stderr,
            "native stderr for {}",
            task.id
        );
        native_tool_result(&output)
    }

    fn usage() -> ProviderUsage {
        ProviderUsage {
            input_tokens: 10,
            cached_input_tokens: 0,
            visible_output_tokens: 5,
            hidden_reasoning_tokens: None,
            raw_usage_sha256: sha256_hex(b"{}"),
        }
    }

    #[test]
    fn invalid_agent_request_is_rejected_before_execution() {
        assert!(parse_agent_request("not ason\n").is_err());
        assert_eq!(INVALID_REQUEST_RESULT, "e:invalid-request\n");
    }

    #[test]
    fn native_tool_result_is_stable_and_normalized() {
        let output = NativeProcessOutput {
            exit_code: Some(0),
            stdout: b"ok\r\n".to_vec(),
            stderr: Vec::new(),
            timed_out: false,
            output_exceeded: false,
            elapsed_ns: 1,
        };
        assert_eq!(
            native_tool_result(&output),
            "exit:0\nstdout:3\nok\nstderr:0\n"
        );
        assert_eq!(normalize_agent_stream(b"a\rb\r\n"), "a\nb\n");
    }

    #[test]
    fn trace_scalars_use_bounded_canonical_forms() {
        assert!(valid_timestamp("2026-08-03T00:00:00Z"));
        assert!(valid_timestamp("2026-08-03T00:00:00.123456789Z"));
        assert!(!valid_timestamp("2026-08-03 00:00:00"));
        assert!(!valid_timestamp("2026-13-03T00:00:00Z"));
        assert_eq!(ratio_percent_u64(1, 3), Some(34));
        assert_eq!(ratio_percent_u64(1, 0), None);
    }

    #[test]
    fn strict_trace_requires_canonical_pairs() {
        let manifest = load_manifest().expect("manifest");
        let lock = load_lock(&manifest).expect("lock");
        let task_traces = || {
            manifest
                .tasks
                .iter()
                .map(|task| AgentTaskTrace {
                    id: task.id.clone(),
                    prompt: agent_task_prompt(task),
                    attempts: vec![AgentAttemptTrace {
                        kind: AttemptKind::Request,
                        model_output: "request\n".to_owned(),
                        tool_result_sha256: "0".repeat(64),
                        model_elapsed_millis: 1,
                        provider_request_sha256: "0".repeat(64),
                        provider_response_sha256: "0".repeat(64),
                    }],
                    final_stdout: String::new(),
                    final_stderr: String::new(),
                    finish_elapsed_millis: 1,
                    finish_request_sha256: "0".repeat(64),
                    finish_response_sha256: "0".repeat(64),
                    usage: usage(),
                })
                .collect::<Vec<_>>()
        };
        let mut trace = AgentTrace {
            schema: 1,
            evidence_kind: "model-selected-trace".to_owned(),
            experiment_id: "paired-smoke".to_owned(),
            captured_at_utc: "2026-08-03T00:00:00Z".to_owned(),
            manifest_sha256: sha256_hex(MANIFEST_BYTES),
            lock_sha256: sha256_hex(LOCK_BYTES),
            driver: DriverMetadata {
                name: "fixture".to_owned(),
                version: "1".to_owned(),
                source_sha256: "0".repeat(64),
            },
            model: ModelMetadata {
                provider: "fixture".to_owned(),
                id: "fixture".to_owned(),
                revision: "1".to_owned(),
                context_tokens: 1024,
                max_output_tokens: 256,
                temperature: "0".to_owned(),
                top_p: "1".to_owned(),
                reasoning_effort: "none".to_owned(),
            },
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            repetitions: 1,
            primers: PrimerSet {
                shared: "shared\n".to_owned(),
                ash: "ash\n".to_owned(),
                native_shell: "native\n".to_owned(),
                ash_tools: "tools\n".to_owned(),
                native_shell_tools: "tools\n".to_owned(),
            },
            audit_sha256: "0".repeat(64),
            runs: vec![
                AgentRunTrace {
                    arm: AgentArm::Ash,
                    repetition: 0,
                    seed: 7,
                    tasks: task_traces(),
                },
                AgentRunTrace {
                    arm: AgentArm::NativeShell,
                    repetition: 0,
                    seed: 7,
                    tasks: task_traces(),
                },
            ],
        };
        assert!(validate_trace(&trace, &manifest, &lock).is_ok());
        trace.runs[1].seed = 8;
        assert!(validate_trace(&trace, &manifest, &lock).is_err());
    }

    #[tokio::test]
    async fn paired_trace_replay_reaches_the_locked_state() {
        let manifest = load_manifest().expect("manifest");
        let lock = load_lock(&manifest).expect("lock");
        let task = &manifest.tasks[0];
        let locked = &lock.tasks[0];
        let ash = execute_ash_task(task)
            .await
            .expect("deterministic ASH plan");
        let native_result = capture_native_result(task).await;
        let ash_trace = AgentTaskTrace {
            id: task.id.clone(),
            prompt: agent_task_prompt(task),
            attempts: vec![
                AgentAttemptTrace {
                    kind: AttemptKind::InvalidRequest,
                    model_output: "not ason\n".to_owned(),
                    tool_result_sha256: sha256_hex(INVALID_REQUEST_RESULT.as_bytes()),
                    model_elapsed_millis: 1,
                    provider_request_sha256: "0".repeat(64),
                    provider_response_sha256: "0".repeat(64),
                },
                AgentAttemptTrace {
                    kind: AttemptKind::Request,
                    model_output: ash.steps[0].request.clone(),
                    tool_result_sha256: sha256_hex(ash.steps[0].response.as_bytes()),
                    model_elapsed_millis: 1,
                    provider_request_sha256: "0".repeat(64),
                    provider_response_sha256: "0".repeat(64),
                },
            ],
            final_stdout: task.expected.stdout.clone(),
            final_stderr: task.expected.stderr.clone(),
            finish_elapsed_millis: 1,
            finish_request_sha256: "0".repeat(64),
            finish_response_sha256: "0".repeat(64),
            usage: usage(),
        };
        let native_trace = AgentTaskTrace {
            id: task.id.clone(),
            prompt: agent_task_prompt(task),
            attempts: vec![AgentAttemptTrace {
                kind: AttemptKind::Request,
                model_output: task
                    .baselines
                    .current()
                    .expect("platform baseline")
                    .script
                    .clone(),
                tool_result_sha256: sha256_hex(native_result.as_bytes()),
                model_elapsed_millis: 1,
                provider_request_sha256: "0".repeat(64),
                provider_response_sha256: "0".repeat(64),
            }],
            final_stdout: task.expected.stdout.clone(),
            final_stderr: task.expected.stderr.clone(),
            finish_elapsed_millis: 1,
            finish_request_sha256: "0".repeat(64),
            finish_response_sha256: "0".repeat(64),
            usage: usage(),
        };
        let cl100k = tiktoken_rs::cl100k_base().expect("cl100k");
        let o200k = tiktoken_rs::o200k_base().expect("o200k");
        let replayed = replay_ash_task(task, locked, &ash_trace, &cl100k, &o200k)
            .await
            .expect("ASH trace replay");
        assert!(replayed.report.success);
        assert_eq!(replayed.report.failed_attempts, 1);
        assert_eq!(replayed.report.retries, 1);
        assert!(
            replay_native_task(task, locked, &native_trace, &cl100k, &o200k)
                .await
                .expect("native trace replay")
                .report
                .success
        );
    }

    #[tokio::test]
    async fn full_synthetic_pair_exercises_report_without_publishing_results() {
        let manifest = load_manifest().expect("manifest");
        let mut ash_tasks = Vec::with_capacity(manifest.tasks.len());
        let mut native_tasks = Vec::with_capacity(manifest.tasks.len());
        for task in &manifest.tasks {
            let ash = execute_ash_task(task).await.expect("ASH fixture trace");
            ash_tasks.push(AgentTaskTrace {
                id: task.id.clone(),
                prompt: agent_task_prompt(task),
                attempts: ash
                    .steps
                    .into_iter()
                    .map(|step| AgentAttemptTrace {
                        kind: AttemptKind::Request,
                        model_output: step.request,
                        tool_result_sha256: sha256_hex(step.response.as_bytes()),
                        model_elapsed_millis: 1,
                        provider_request_sha256: "0".repeat(64),
                        provider_response_sha256: "0".repeat(64),
                    })
                    .collect(),
                final_stdout: task.expected.stdout.clone(),
                final_stderr: task.expected.stderr.clone(),
                finish_elapsed_millis: 1,
                finish_request_sha256: "0".repeat(64),
                finish_response_sha256: "0".repeat(64),
                usage: usage(),
            });
            let result = capture_native_result(task).await;
            native_tasks.push(AgentTaskTrace {
                id: task.id.clone(),
                prompt: agent_task_prompt(task),
                attempts: vec![AgentAttemptTrace {
                    kind: AttemptKind::Request,
                    model_output: task
                        .baselines
                        .current()
                        .expect("platform baseline")
                        .script
                        .clone(),
                    tool_result_sha256: sha256_hex(result.as_bytes()),
                    model_elapsed_millis: 1,
                    provider_request_sha256: "0".repeat(64),
                    provider_response_sha256: "0".repeat(64),
                }],
                final_stdout: task.expected.stdout.clone(),
                final_stderr: task.expected.stderr.clone(),
                finish_elapsed_millis: 1,
                finish_request_sha256: "0".repeat(64),
                finish_response_sha256: "0".repeat(64),
                usage: usage(),
            });
        }
        let mut trace = AgentTrace {
            schema: 1,
            evidence_kind: "model-selected-trace".to_owned(),
            experiment_id: "synthetic-test-only".to_owned(),
            captured_at_utc: "2026-08-03T00:00:00Z".to_owned(),
            manifest_sha256: sha256_hex(MANIFEST_BYTES),
            lock_sha256: sha256_hex(LOCK_BYTES),
            driver: DriverMetadata {
                name: "test-only-deterministic-fixture".to_owned(),
                version: "1".to_owned(),
                source_sha256: "0".repeat(64),
            },
            model: ModelMetadata {
                provider: "test-only".to_owned(),
                id: "deterministic-fixture".to_owned(),
                revision: "1".to_owned(),
                context_tokens: 4096,
                max_output_tokens: 1024,
                temperature: "0".to_owned(),
                top_p: "1".to_owned(),
                reasoning_effort: "none".to_owned(),
            },
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            repetitions: 1,
            primers: PrimerSet {
                shared: "test-only shared primer\n".to_owned(),
                ash: "test-only ASH primer\n".to_owned(),
                native_shell: "test-only native primer\n".to_owned(),
                ash_tools: "test-only ASH tools\n".to_owned(),
                native_shell_tools: "test-only native tools\n".to_owned(),
            },
            audit_sha256: "0".repeat(64),
            runs: vec![
                AgentRunTrace {
                    arm: AgentArm::Ash,
                    repetition: 0,
                    seed: 1,
                    tasks: ash_tasks,
                },
                AgentRunTrace {
                    arm: AgentArm::NativeShell,
                    repetition: 0,
                    seed: 1,
                    tasks: native_tasks,
                },
            ],
        };
        let directory = tempfile::TempDir::new().expect("trace directory");
        let path = directory.path().join("trace.json");
        let audit_path = directory.path().join("audit.jsonl");
        let exchange_json = "{}".to_owned();
        let exchange_sha256 = sha256_hex(exchange_json.as_bytes());
        let mut audit = Vec::new();
        for run in &mut trace.runs {
            for task in &mut run.tasks {
                for (turn, attempt) in task.attempts.iter_mut().enumerate() {
                    attempt.provider_request_sha256 = exchange_sha256.clone();
                    attempt.provider_response_sha256 = exchange_sha256.clone();
                    audit.push(AgentAuditRecord {
                        schema: 1,
                        provider: trace.model.provider.clone(),
                        arm: run.arm,
                        repetition: run.repetition,
                        seed: run.seed,
                        task_id: task.id.clone(),
                        turn,
                        phase: "action".to_owned(),
                        request_sha256: exchange_sha256.clone(),
                        response_sha256: exchange_sha256.clone(),
                        request_json: exchange_json.clone(),
                        response_json: exchange_json.clone(),
                    });
                }
                task.finish_request_sha256 = exchange_sha256.clone();
                task.finish_response_sha256 = exchange_sha256.clone();
                audit.push(AgentAuditRecord {
                    schema: 1,
                    provider: trace.model.provider.clone(),
                    arm: run.arm,
                    repetition: run.repetition,
                    seed: run.seed,
                    task_id: task.id.clone(),
                    turn: task.attempts.len(),
                    phase: "finish".to_owned(),
                    request_sha256: exchange_sha256.clone(),
                    response_sha256: exchange_sha256.clone(),
                    request_json: exchange_json.clone(),
                    response_json: exchange_json.clone(),
                });
            }
        }
        let mut audit_bytes = Vec::new();
        for record in &audit {
            serde_json::to_writer(&mut audit_bytes, record).expect("audit record");
            audit_bytes.push(b'\n');
        }
        trace.audit_sha256 = sha256_hex(&audit_bytes);
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&trace).expect("trace JSON"),
        )
        .expect("trace file");
        std::fs::write(&audit_path, audit_bytes).expect("audit file");
        validate_agent_trace_audit(&path, &audit_path).expect("trace audit validation");
        let report = build_agent_report(&path, &audit_path)
            .await
            .expect("paired report");
        assert!(report.agent_results);
        assert!(report.audit_verified);
        assert!(report.gates.passed);
        assert_eq!(report.runs.len(), 2);
    }
}
