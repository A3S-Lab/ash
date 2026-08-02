use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ash_cli::ExecutionSession;
use ash_engine::Parallelism;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::{NamedTempFile, TempDir};

use super::{
    AgentArm, AgentAttemptTrace, AgentRunTrace, AgentTaskTrace, AgentTrace, AttemptKind,
    DriverMetadata, INVALID_REQUEST_RESULT, LOCK_BYTES, MANIFEST_BYTES, MAX_ATTEMPTS,
    MAX_MODEL_ELAPSED_MILLIS, MAX_PROVIDER_TOKENS, MAX_REPETITIONS, MAX_TRACE_BYTES, ModelMetadata,
    POLICY_REJECTED_RESULT, PrimerSet, ProviderUsage, TaskDefinition, agent_task_prompt,
    copy_workspace, fixture_path, load_lock, load_manifest, native_tool_result,
    parse_agent_request, request_allowed, run_native_agent_command, sha256_hex,
    task_capability_mask, tree_sha256, valid_name, validate_metadata, validate_trace,
    validate_visible_text,
};

const DRIVER_NAME: &str = "ash-openai-responses";
const PROVIDER_NAME: &str = "openai-responses";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const API_TIMEOUT: Duration = Duration::from_secs(300);
const DRIVER_SOURCE: &[u8] = include_bytes!("openai.rs");

const SHARED_PRIMER: &str = "Complete exactly the supplied workspace task. Work only through the act and finish functions. Call act until the workspace and output evidence are sufficient, then call finish with the exact stdout and stderr required by the objective. Do not explain, use Markdown, or invent tool results. Empty output is an empty string.\n";

const ASH_PRIMER: &str = r#"act.payload is exactly one canonical ASH/1 ASON request ending in LF. ASON atoms are bare when safe; strings use JSON quotes; vectors use [...]; null is ~; references use @N. Envelope order is t,i,o,a,u. Use a positive request id and a budget no larger than the task limits.
request skeleton:
t:1
i:1
o:<op>
<arguments>u{tok,rec,ms}:
4096,1024,30000
allowed operation forms:
r => a{p,m,o,n}: paths, mode(0 bytes/1 lines), offset, length
l => a{p,d,f}: roots, max depth, flags(1 hidden,2 files-only,4 dirs-only)
g => a{q,p,f}: query, roots, flags(1 regex,2 case-insensitive,4 hidden)
p => a{p,h,i,o,n,v,f}: sorted paths, matching BLAKE3 digests, file indexes, byte offsets, delete lengths, replacements, flags=0
f => a[N]{i,k,p,q,h,v}: increasing action id, kind(0 create/1 copy/2 move/3 remove), path, destination, source digest, content; use ~ for absent cells
b => a[N]{i,d,o,a}: increasing node id, sorted dependency ids, leaf opcode, quoted canonical one-field ASON argument document
Read responses expose exact content and BLAKE3 digest evidence. Reuse returned lowercase 64-hex digests for guarded patch/fs mutations. Only task-declared capabilities are permitted. Invalid ASON returns e:invalid-request; an undeclared operation returns e:policy-rejected; both are charged attempts.
"#;

#[derive(Debug)]
struct CaptureOptions {
    trace_path: PathBuf,
    audit_path: PathBuf,
    experiment_id: String,
    model: String,
    context_tokens: u64,
    max_output_tokens: u64,
    reasoning_effort: String,
    repetitions: u16,
    seed: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AuditRecord {
    schema: u8,
    provider: &'static str,
    arm: AgentArm,
    repetition: u16,
    seed: u64,
    task_id: String,
    turn: usize,
    phase: &'static str,
    request_sha256: String,
    response_sha256: String,
    request_json: String,
    response_json: String,
}

struct OpenAiClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

struct ApiTurn {
    model_revision: String,
    call: ModelCall,
    output: Vec<Value>,
    usage: Value,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    elapsed_millis: u64,
    request_sha256: String,
    response_sha256: String,
    request_json: String,
    response_json: String,
}

enum ModelCall {
    Act { call_id: String, payload: String },
    Finish { stdout: String, stderr: String },
}

#[derive(Default)]
struct UsageAccumulator {
    input_tokens: u64,
    cached_input_tokens: u64,
    visible_output_tokens: u64,
    reasoning_tokens: u64,
    raw: Vec<Value>,
}

struct ActionResult {
    kind: AttemptKind,
    tool_result: String,
}

enum Backend {
    Ash(ExecutionSession),
    NativeShell,
}

struct CaptureExecutor {
    _directory: TempDir,
    backend: Backend,
    tool_elapsed: Duration,
    output_limit: usize,
    millis_limit: u64,
}

pub(crate) async fn capture_openai_agent_trace(
    arguments: &[OsString],
) -> Result<(), Box<dyn Error>> {
    let options = CaptureOptions::parse(arguments)?;
    let client = OpenAiClient::from_environment()?;
    capture_with(options, client).await
}

async fn capture_with(options: CaptureOptions, client: OpenAiClient) -> Result<(), Box<dyn Error>> {
    let manifest = load_manifest()?;
    let lock = load_lock(&manifest)?;
    let tools = tool_schemas();
    let tools_json = serde_json::to_string(&tools)?;
    let primers = PrimerSet {
        shared: SHARED_PRIMER.to_owned(),
        ash: ASH_PRIMER.to_owned(),
        native_shell: native_primer(),
        ash_tools: tools_json.clone(),
        native_shell_tools: tools_json,
    };
    let mut records = Vec::new();
    let mut runs = Vec::with_capacity(usize::from(options.repetitions) * 2);
    let mut revision = None;

    for repetition in 0..options.repetitions {
        let seed = paired_seed(options.seed, repetition);
        let order = shuffled_indices(manifest.tasks.len(), seed);
        for arm in [AgentArm::Ash, AgentArm::NativeShell] {
            let mut tasks = Vec::with_capacity(order.len());
            for &index in &order {
                let task = &manifest.tasks[index];
                let locked = &lock.tasks[index];
                tasks.push(
                    capture_task(
                        &client,
                        &options,
                        &primers,
                        &tools,
                        arm,
                        repetition,
                        seed,
                        task,
                        locked.initial_tree_sha256.as_str(),
                        &mut revision,
                        &mut records,
                    )
                    .await?,
                );
            }
            runs.push(AgentRunTrace {
                arm,
                repetition,
                seed,
                tasks,
            });
        }
    }

    let audit_bytes = encode_audit(&records)?;
    if u64::try_from(audit_bytes.len())? > MAX_TRACE_BYTES {
        return Err(io::Error::other("provider audit exceeds its byte ceiling").into());
    }
    let audit_sha256 = sha256_hex(&audit_bytes);
    let trace = AgentTrace {
        schema: 1,
        evidence_kind: "model-selected-trace".to_owned(),
        experiment_id: options.experiment_id.clone(),
        captured_at_utc: utc_timestamp()?,
        manifest_sha256: sha256_hex(MANIFEST_BYTES),
        lock_sha256: sha256_hex(LOCK_BYTES),
        driver: DriverMetadata {
            name: DRIVER_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            source_sha256: sha256_hex(DRIVER_SOURCE),
        },
        model: ModelMetadata {
            provider: PROVIDER_NAME.to_owned(),
            id: options.model.clone(),
            revision: revision
                .ok_or_else(|| io::Error::other("provider returned no model revision"))?,
            context_tokens: options.context_tokens,
            max_output_tokens: options.max_output_tokens,
            temperature: "provider-default".to_owned(),
            top_p: "provider-default".to_owned(),
            reasoning_effort: options.reasoning_effort.clone(),
        },
        platform: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        repetitions: options.repetitions,
        primers,
        audit_sha256: audit_sha256.clone(),
        runs,
    };
    validate_trace(&trace, &manifest, &lock)?;
    let mut trace_bytes = serde_json::to_vec_pretty(&trace)?;
    trace_bytes.push(b'\n');
    if u64::try_from(trace_bytes.len())? > MAX_TRACE_BYTES {
        return Err(io::Error::other("agent trace exceeds its byte ceiling").into());
    }
    write_new_atomic(&options.audit_path, &audit_bytes)?;
    write_new_atomic(&options.trace_path, &trace_bytes)?;

    let receipt = json!({
        "schema": 1,
        "trace": options.trace_path,
        "trace_sha256": sha256_hex(&trace_bytes),
        "audit": options.audit_path,
        "audit_sha256": audit_sha256,
        "model_revision": trace.model.revision,
        "repetitions": trace.repetitions,
        "tasks": manifest.tasks.len(),
    });
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn capture_task(
    client: &OpenAiClient,
    options: &CaptureOptions,
    primers: &PrimerSet,
    tools: &Value,
    arm: AgentArm,
    repetition: u16,
    seed: u64,
    task: &TaskDefinition,
    locked_initial_tree_sha256: &str,
    revision: &mut Option<String>,
    records: &mut Vec<AuditRecord>,
) -> Result<AgentTaskTrace, Box<dyn Error>> {
    let prompt = agent_task_prompt(task);
    let arm_primer = match arm {
        AgentArm::Ash => &primers.ash,
        AgentArm::NativeShell => &primers.native_shell,
    };
    let mut executor = CaptureExecutor::open(task, arm, locked_initial_tree_sha256)?;
    let result = capture_task_inner(
        client,
        options,
        tools,
        &primers.shared,
        arm_primer,
        arm,
        repetition,
        seed,
        task,
        prompt,
        &mut executor,
        revision,
        records,
    )
    .await;
    let close = executor.close();
    match (result, close) {
        (Ok(trace), Ok(())) => Ok(trace),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn capture_task_inner(
    client: &OpenAiClient,
    options: &CaptureOptions,
    tools: &Value,
    shared_primer: &str,
    arm_primer: &str,
    arm: AgentArm,
    repetition: u16,
    seed: u64,
    task: &TaskDefinition,
    prompt: String,
    executor: &mut CaptureExecutor,
    revision: &mut Option<String>,
    records: &mut Vec<AuditRecord>,
) -> Result<AgentTaskTrace, Box<dyn Error>> {
    let mut attempts = Vec::new();
    let mut usage = UsageAccumulator::default();
    let mut input = vec![
        json!({"role": "developer", "content": shared_primer}),
        json!({"role": "developer", "content": arm_primer}),
        json!({"role": "user", "content": prompt}),
    ];

    loop {
        let turn = client.respond(options, tools, &input).await?;
        bind_revision(revision, &turn.model_revision)?;
        usage.add(&turn)?;
        let phase = match &turn.call {
            ModelCall::Act { .. } => "action",
            ModelCall::Finish { .. } => "finish",
        };
        records.push(AuditRecord {
            schema: 1,
            provider: PROVIDER_NAME,
            arm,
            repetition,
            seed,
            task_id: task.id.clone(),
            turn: attempts.len(),
            phase,
            request_sha256: turn.request_sha256.clone(),
            response_sha256: turn.response_sha256.clone(),
            request_json: turn.request_json.clone(),
            response_json: turn.response_json.clone(),
        });

        match turn.call {
            ModelCall::Act { call_id, payload } => {
                if attempts.len() >= MAX_ATTEMPTS {
                    return Err(io::Error::other(format!(
                        "model exceeded the action ceiling for task {}",
                        task.id
                    ))
                    .into());
                }
                validate_visible_text(&payload, false)?;
                let action = executor.act(task, &payload).await?;
                let tool_result_sha256 = sha256_hex(action.tool_result.as_bytes());
                attempts.push(AgentAttemptTrace {
                    kind: action.kind,
                    model_output: payload,
                    tool_result_sha256,
                    model_elapsed_millis: turn.elapsed_millis,
                    provider_request_sha256: turn.request_sha256,
                    provider_response_sha256: turn.response_sha256,
                });
                input.extend(turn.output);
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": action.tool_result,
                }));
            }
            ModelCall::Finish { stdout, stderr } => {
                if attempts.is_empty() {
                    return Err(io::Error::other(format!(
                        "model finished task {} before any action",
                        task.id
                    ))
                    .into());
                }
                validate_visible_text(&stdout, true)?;
                validate_visible_text(&stderr, true)?;
                if stdout.len().saturating_add(stderr.len()) > task.limits.output_bytes {
                    return Err(io::Error::other(format!(
                        "model finish output exceeds the task ceiling for {}",
                        task.id
                    ))
                    .into());
                }
                return Ok(AgentTaskTrace {
                    id: task.id.clone(),
                    prompt,
                    attempts,
                    final_stdout: stdout,
                    final_stderr: stderr,
                    finish_elapsed_millis: turn.elapsed_millis,
                    finish_request_sha256: turn.request_sha256,
                    finish_response_sha256: turn.response_sha256,
                    usage: usage.finish()?,
                });
            }
        }
    }
}

impl CaptureOptions {
    fn parse(arguments: &[OsString]) -> Result<Self, io::Error> {
        let Some(trace_path) = arguments.first() else {
            return Err(io::Error::other(capture_usage()));
        };
        if trace_path.to_string_lossy().starts_with("--") {
            return Err(io::Error::other(capture_usage()));
        }
        let mut audit_path = None;
        let mut experiment_id = None;
        let mut model = None;
        let mut context_tokens = None;
        let mut max_output_tokens = None;
        let mut reasoning_effort = None;
        let mut repetitions = None;
        let mut seed = None;
        let mut index = 1;
        while index < arguments.len() {
            let flag = arguments[index]
                .to_str()
                .ok_or_else(|| io::Error::other("capture option is not UTF-8"))?;
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| io::Error::other(capture_usage()))?;
            let value_text = value
                .to_str()
                .ok_or_else(|| io::Error::other("capture value is not UTF-8"))?;
            match flag {
                "--audit" => set_once(&mut audit_path, PathBuf::from(value), flag)?,
                "--experiment-id" => set_once(&mut experiment_id, value_text.to_owned(), flag)?,
                "--model" => set_once(&mut model, value_text.to_owned(), flag)?,
                "--context-tokens" => set_once(
                    &mut context_tokens,
                    parse_number(value_text, "context tokens")?,
                    flag,
                )?,
                "--max-output-tokens" => set_once(
                    &mut max_output_tokens,
                    parse_number(value_text, "max output tokens")?,
                    flag,
                )?,
                "--reasoning-effort" => {
                    set_once(&mut reasoning_effort, value_text.to_owned(), flag)?
                }
                "--repetitions" => set_once(
                    &mut repetitions,
                    value_text
                        .parse::<u16>()
                        .map_err(|_| io::Error::other("repetitions are invalid"))?,
                    flag,
                )?,
                "--seed" => set_once(
                    &mut seed,
                    parse_number(value_text, "task-order seed")?,
                    flag,
                )?,
                _ => return Err(io::Error::other(capture_usage())),
            }
            index += 2;
        }
        let experiment_id = experiment_id.ok_or_else(|| io::Error::other(capture_usage()))?;
        let model = model.ok_or_else(|| io::Error::other(capture_usage()))?;
        let context_tokens = context_tokens.ok_or_else(|| io::Error::other(capture_usage()))?;
        let max_output_tokens =
            max_output_tokens.ok_or_else(|| io::Error::other(capture_usage()))?;
        let reasoning_effort = reasoning_effort.ok_or_else(|| io::Error::other(capture_usage()))?;
        let repetitions = repetitions.unwrap_or(1);
        if !valid_name(&experiment_id)
            || validate_metadata(&model).is_err()
            || !matches!(
                reasoning_effort.as_str(),
                "none" | "low" | "medium" | "high" | "xhigh" | "max"
            )
            || repetitions == 0
            || repetitions > MAX_REPETITIONS
            || context_tokens == 0
            || context_tokens > MAX_PROVIDER_TOKENS
            || max_output_tokens == 0
            || max_output_tokens > context_tokens
        {
            return Err(io::Error::other("capture configuration is invalid"));
        }
        let audit_path = audit_path.ok_or_else(|| io::Error::other(capture_usage()))?;
        let trace_path = PathBuf::from(trace_path);
        if trace_path == audit_path || trace_path.exists() || audit_path.exists() {
            return Err(io::Error::other(
                "trace and audit paths must be distinct new files",
            ));
        }
        Ok(Self {
            trace_path,
            audit_path,
            experiment_id,
            model,
            context_tokens,
            max_output_tokens,
            reasoning_effort,
            repetitions,
            seed: seed.unwrap_or(0x4153_482d_4147_454e),
        })
    }
}

impl OpenAiClient {
    fn from_environment() -> Result<Self, Box<dyn Error>> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| io::Error::other("OPENAI_API_KEY is required"))?;
        if api_key.trim().is_empty() || api_key.chars().any(char::is_control) {
            return Err(io::Error::other("OPENAI_API_KEY is invalid").into());
        }
        let base_url =
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
        Self::new(base_url, api_key)
    }

    fn new(base_url: String, api_key: String) -> Result<Self, Box<dyn Error>> {
        let base_url = base_url.trim_end_matches('/');
        let parsed = reqwest::Url::parse(base_url)?;
        let host = parsed.host_str().unwrap_or_default();
        let secure = parsed.scheme() == "https";
        let loopback =
            parsed.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1" | "::1");
        if (!secure && !loopback)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || base_url.chars().any(char::is_control)
        {
            return Err(io::Error::other(
                "OPENAI_BASE_URL must use HTTPS or an HTTP loopback address",
            )
            .into());
        }
        let endpoint = if base_url.ends_with("/responses") {
            base_url.to_owned()
        } else {
            format!("{base_url}/responses")
        };
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(API_TIMEOUT)
            .user_agent(concat!("a3s-ash-bench/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            endpoint,
            api_key,
        })
    }

    async fn respond(
        &self,
        options: &CaptureOptions,
        tools: &Value,
        input: &[Value],
    ) -> Result<ApiTurn, Box<dyn Error>> {
        let body = response_request(options, tools, input);
        let request_bytes = serde_json::to_vec(&body)?;
        if request_bytes.len() > MAX_RESPONSE_BYTES {
            return Err(io::Error::other("provider request exceeds its byte ceiling").into());
        }
        let request_json = String::from_utf8(request_bytes.clone())?;
        let started = Instant::now();
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .header(CONTENT_TYPE, "application/json")
            .body(request_bytes.clone())
            .send()
            .await?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(io::Error::other("provider response exceeds its byte ceiling").into());
        }
        let response_bytes = response.bytes().await?;
        if response_bytes.len() > MAX_RESPONSE_BYTES {
            return Err(io::Error::other("provider response exceeds its byte ceiling").into());
        }
        let response_json = String::from_utf8(response_bytes.to_vec())?;
        if !status.is_success() {
            let excerpt = response_json
                .chars()
                .filter(|character| !character.is_control() || *character == '\n')
                .take(2048)
                .collect::<String>();
            return Err(io::Error::other(format!(
                "provider returned HTTP {}: {excerpt}",
                status.as_u16()
            ))
            .into());
        }
        parse_turn(
            &response_json,
            started.elapsed(),
            sha256_hex(&request_bytes),
            sha256_hex(&response_bytes),
            request_json,
        )
    }
}

impl UsageAccumulator {
    fn add(&mut self, turn: &ApiTurn) -> Result<(), io::Error> {
        let visible = turn
            .output_tokens
            .checked_sub(turn.reasoning_tokens)
            .ok_or_else(|| io::Error::other("provider reasoning usage exceeds output usage"))?;
        self.input_tokens = checked_add(self.input_tokens, turn.input_tokens, "input usage")?;
        self.cached_input_tokens = checked_add(
            self.cached_input_tokens,
            turn.cached_input_tokens,
            "cached input usage",
        )?;
        self.visible_output_tokens =
            checked_add(self.visible_output_tokens, visible, "visible output usage")?;
        self.reasoning_tokens = checked_add(
            self.reasoning_tokens,
            turn.reasoning_tokens,
            "reasoning usage",
        )?;
        self.raw.push(turn.usage.clone());
        Ok(())
    }

    fn finish(self) -> Result<ProviderUsage, Box<dyn Error>> {
        let raw = serde_json::to_vec(&self.raw)?;
        Ok(ProviderUsage {
            input_tokens: self.input_tokens,
            cached_input_tokens: self.cached_input_tokens,
            visible_output_tokens: self.visible_output_tokens,
            hidden_reasoning_tokens: Some(self.reasoning_tokens),
            raw_usage_sha256: sha256_hex(&raw),
        })
    }
}

impl CaptureExecutor {
    fn open(
        task: &TaskDefinition,
        arm: AgentArm,
        locked_initial_tree_sha256: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let fixture = fixture_path(&task.workspace)?;
        let directory = TempDir::new()?;
        copy_workspace(&fixture, directory.path())?;
        if tree_sha256(directory.path())? != locked_initial_tree_sha256 {
            return Err(io::Error::other(format!(
                "initial workspace changed before capture for task {}",
                task.id
            ))
            .into());
        }
        let backend = match arm {
            AgentArm::Ash => {
                let workspace = directory
                    .path()
                    .to_str()
                    .ok_or_else(|| io::Error::other("capture workspace path is not valid UTF-8"))?;
                Backend::Ash(ExecutionSession::open(
                    1,
                    workspace,
                    u64::try_from(task.limits.output_bytes)?,
                    Parallelism::detected(),
                    task_capability_mask(task),
                )?)
            }
            AgentArm::NativeShell => Backend::NativeShell,
        };
        Ok(Self {
            _directory: directory,
            backend,
            tool_elapsed: Duration::ZERO,
            output_limit: task.limits.output_bytes,
            millis_limit: task.limits.millis,
        })
    }

    async fn act(
        &mut self,
        task: &TaskDefinition,
        model_output: &str,
    ) -> Result<ActionResult, Box<dyn Error>> {
        let remaining = self
            .millis_limit
            .checked_sub(u64::try_from(self.tool_elapsed.as_millis()).unwrap_or(u64::MAX))
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| io::Error::other("capture tool deadline was exhausted"))?;
        let started = Instant::now();
        let result = match &self.backend {
            Backend::Ash(session) => {
                let parsed = parse_agent_request(model_output);
                match parsed {
                    Err(_) => ActionResult {
                        kind: AttemptKind::InvalidRequest,
                        tool_result: INVALID_REQUEST_RESULT.to_owned(),
                    },
                    Ok(request) if !request_allowed(task, &request) => ActionResult {
                        kind: AttemptKind::PolicyRejected,
                        tool_result: POLICY_REJECTED_RESULT.to_owned(),
                    },
                    Ok(request) => {
                        let response = tokio::time::timeout(
                            Duration::from_millis(remaining),
                            session.execute(&request),
                        )
                        .await
                        .map_err(|_| io::Error::other("ASH capture operation timed out"))??;
                        ActionResult {
                            kind: AttemptKind::Request,
                            tool_result: response.encode()?.encode(),
                        }
                    }
                }
            }
            Backend::NativeShell => {
                let process = run_native_agent_command(
                    self._directory.path(),
                    model_output,
                    self.output_limit,
                    remaining,
                )
                .await?;
                ActionResult {
                    kind: AttemptKind::Request,
                    tool_result: native_tool_result(&process),
                }
            }
        };
        self.tool_elapsed = self.tool_elapsed.saturating_add(started.elapsed());
        Ok(result)
    }

    fn close(self) -> Result<(), Box<dyn Error>> {
        match self.backend {
            Backend::Ash(session) => Ok(session.close()?),
            Backend::NativeShell => Ok(()),
        }
    }
}

fn response_request(options: &CaptureOptions, tools: &Value, input: &[Value]) -> Value {
    json!({
        "input": input,
        "max_output_tokens": options.max_output_tokens,
        "model": options.model,
        "parallel_tool_calls": false,
        "reasoning": {"effort": options.reasoning_effort},
        "store": false,
        "tool_choice": "required",
        "tools": tools,
    })
}

fn tool_schemas() -> Value {
    json!([
        {
            "type": "function",
            "name": "act",
            "description": "Execute one exact ASH request or native-shell script for the active arm.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "payload": {
                        "type": "string",
                        "description": "Exact action bytes represented as UTF-8 text."
                    }
                },
                "required": ["payload"],
                "additionalProperties": false
            }
        },
        {
            "type": "function",
            "name": "finish",
            "description": "Finish the task with its exact semantic stdout and stderr.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "stdout": {"type": "string"},
                    "stderr": {"type": "string"}
                },
                "required": ["stdout", "stderr"],
                "additionalProperties": false
            }
        }
    ])
}

fn parse_turn(
    response_json: &str,
    elapsed: Duration,
    request_sha256: String,
    response_sha256: String,
    request_json: String,
) -> Result<ApiTurn, Box<dyn Error>> {
    let root: Value = serde_json::from_str(response_json)?;
    let object = root
        .as_object()
        .ok_or_else(|| io::Error::other("provider response is not an object"))?;
    if object.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(io::Error::other("provider response did not complete").into());
    }
    let response_id = required_string(object.get("id"), "response id")?;
    let model_revision = required_string(object.get("model"), "model revision")?;
    validate_metadata(&response_id)?;
    validate_metadata(&model_revision)?;
    let output = object
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("provider response output is missing"))?;
    if output.iter().any(|item| {
        !matches!(
            item.get("type").and_then(Value::as_str),
            Some("reasoning" | "function_call")
        )
    }) {
        return Err(io::Error::other("provider returned an unexpected output item").into());
    }
    let calls = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .collect::<Vec<_>>();
    let [item] = calls.as_slice() else {
        return Err(
            io::Error::other("provider must return exactly one function call per turn").into(),
        );
    };
    let call_id = required_string(item.get("call_id"), "function call id")?;
    let name = required_string(item.get("name"), "function name")?;
    let arguments = required_string(item.get("arguments"), "function arguments")?;
    validate_metadata(&call_id)?;
    let call = match name.as_str() {
        "act" => {
            let arguments: ActArguments = serde_json::from_str(&arguments)?;
            ModelCall::Act {
                call_id,
                payload: arguments.payload,
            }
        }
        "finish" => {
            let arguments: FinishArguments = serde_json::from_str(&arguments)?;
            ModelCall::Finish {
                stdout: arguments.stdout,
                stderr: arguments.stderr,
            }
        }
        _ => return Err(io::Error::other("provider called an unknown function").into()),
    };
    let usage = object
        .get("usage")
        .cloned()
        .ok_or_else(|| io::Error::other("provider usage is missing"))?;
    let parsed_usage: OpenAiUsage = serde_json::from_value(usage.clone())?;
    if parsed_usage.input_tokens == 0 || parsed_usage.output_tokens == 0 {
        return Err(io::Error::other("provider usage is empty").into());
    }
    let elapsed_millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    if elapsed_millis > MAX_MODEL_ELAPSED_MILLIS {
        return Err(io::Error::other("provider turn exceeded the elapsed-time ceiling").into());
    }
    Ok(ApiTurn {
        model_revision,
        call,
        output: output.clone(),
        usage,
        input_tokens: parsed_usage.input_tokens,
        cached_input_tokens: parsed_usage.input_tokens_details.cached_tokens,
        output_tokens: parsed_usage.output_tokens,
        reasoning_tokens: parsed_usage.output_tokens_details.reasoning_tokens,
        elapsed_millis,
        request_sha256,
        response_sha256,
        request_json,
        response_json: response_json.to_owned(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActArguments {
    payload: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishArguments {
    stdout: String,
    stderr: String,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    input_tokens: u64,
    #[serde(default)]
    input_tokens_details: InputTokenDetails,
    output_tokens: u64,
    #[serde(default)]
    output_tokens_details: OutputTokenDetails,
}

#[derive(Default, Deserialize)]
struct InputTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Default, Deserialize)]
struct OutputTokenDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

fn native_primer() -> String {
    if cfg!(windows) {
        "act.payload is exactly one non-interactive PowerShell script executed at the workspace root with ErrorActionPreference=Stop. Use only task-declared capabilities. Write only requested semantic output to stdout; diagnostics go to stderr. A nonzero exit, timeout, or output-limit result may be corrected by another act call.\n".to_owned()
    } else {
        "act.payload is exactly one POSIX sh -eu script executed at the workspace root with LC_ALL=C. Use only task-declared capabilities. Write only requested semantic output to stdout; diagnostics go to stderr. A nonzero exit, timeout, or output-limit result may be corrected by another act call.\n".to_owned()
    }
}

fn bind_revision(revision: &mut Option<String>, actual: &str) -> Result<(), io::Error> {
    match revision {
        Some(expected) if expected != actual => Err(io::Error::other(format!(
            "provider model revision changed from {expected} to {actual}"
        ))),
        Some(_) => Ok(()),
        None => {
            *revision = Some(actual.to_owned());
            Ok(())
        }
    }
}

fn required_string(value: Option<&Value>, field: &str) -> Result<String, io::Error> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other(format!("provider {field} is missing")))
}

fn checked_add(left: u64, right: u64, name: &str) -> Result<u64, io::Error> {
    left.checked_add(right)
        .filter(|total| *total <= MAX_PROVIDER_TOKENS)
        .ok_or_else(|| io::Error::other(format!("provider {name} exceeds its ceiling")))
}

fn encode_audit(records: &[AuditRecord]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut output = Vec::new();
    for record in records {
        serde_json::to_writer(&mut output, record)?;
        output.push(b'\n');
    }
    Ok(output)
}

fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    if bytes.is_empty() {
        return Err(io::Error::other("evidence file cannot be empty").into());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata = fs::metadata(parent)?;
    if !metadata.is_dir() {
        return Err(io::Error::other("evidence parent is not a directory").into());
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

fn paired_seed(base: u64, repetition: u16) -> u64 {
    splitmix64(base ^ u64::from(repetition))
}

fn shuffled_indices(length: usize, seed: u64) -> Vec<usize> {
    let mut values = (0..length).collect::<Vec<_>>();
    let mut state = seed;
    for index in (1..length).rev() {
        state = splitmix64(state);
        let selected =
            usize::try_from(state % u64::try_from(index + 1).unwrap_or(u64::MAX)).unwrap_or(0);
        values.swap(index, selected);
    }
    values
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn utc_timestamp() -> Result<String, io::Error> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("system clock predates the Unix epoch"))?
        .as_secs();
    let days = i64::try_from(seconds / 86_400)
        .map_err(|_| io::Error::other("system time is out of range"))?;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn parse_number(value: &str, name: &str) -> Result<u64, io::Error> {
    value
        .parse::<u64>()
        .map_err(|_| io::Error::other(format!("{name} are invalid")))
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), io::Error> {
    if slot.replace(value).is_some() {
        Err(io::Error::other(format!("duplicate capture option {flag}")))
    } else {
        Ok(())
    }
}

fn capture_usage() -> &'static str {
    "usage: a3s-ash-bench --capture-openai-agent-trace <trace> --audit <jsonl> --experiment-id <id> --model <model> --context-tokens <n> --max-output-tokens <n> --reasoning-effort <none|low|medium|high|xhigh|max> [--repetitions <n>] [--seed <n>]"
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::{
        AgentArm, CaptureOptions, ModelCall, OpenAiClient, capture_with, civil_from_days,
        paired_seed, parse_turn, response_request, shuffled_indices, tool_schemas,
    };
    use crate::tasks::agent::{build_agent_report, validate_agent_trace_audit};
    use crate::tasks::{execute_ash_task, load_manifest};

    fn options() -> CaptureOptions {
        CaptureOptions {
            trace_path: "trace.json".into(),
            audit_path: "audit.jsonl".into(),
            experiment_id: "mock-capture".to_owned(),
            model: "gpt-5.6".to_owned(),
            context_tokens: 1_050_000,
            max_output_tokens: 128_000,
            reasoning_effort: "medium".to_owned(),
            repetitions: 1,
            seed: 7,
        }
    }

    fn action_response() -> String {
        call_response(1, "act", json!({"payload": "pwd\n"}))
    }

    fn call_response(sequence: usize, name: &str, arguments: Value) -> String {
        serde_json::to_string(&json!({
            "id": format!("resp_{sequence}"),
            "model": "gpt-5.6-sol-2026-08-01",
            "status": "completed",
            "output": [
                {"type": "reasoning", "id": format!("reason_{sequence}"), "summary": [], "encrypted_content": format!("opaque-{sequence}")},
                {
                    "type": "function_call",
                    "call_id": format!("call_{sequence}"),
                    "name": name,
                    "arguments": serde_json::to_string(&arguments).expect("mock arguments")
                }
            ],
            "usage": {
                "input_tokens": 20,
                "input_tokens_details": {"cached_tokens": 3},
                "output_tokens": 8,
                "output_tokens_details": {"reasoning_tokens": 5}
            }
        }))
        .expect("mock response")
    }

    fn serve_responses(listener: TcpListener, responses: Vec<String>) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().expect("mock request");
                let request = read_http_request(&mut stream);
                let header_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("request header terminator")
                    + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                assert!(headers.contains("authorization: Bearer test-secret"));
                let body: Value =
                    serde_json::from_slice(&request[header_end..]).expect("request JSON body");
                assert_eq!(body["store"], false);
                assert_eq!(body["parallel_tool_calls"], false);
                assert_eq!(body["tool_choice"], "required");
                assert!(body.get("previous_response_id").is_none());
                assert_eq!(body["tools"].as_array().expect("request tools").len(), 2);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                );
                stream.write_all(reply.as_bytes()).expect("write response");
            }
        })
    }

    fn read_http_request(stream: &mut impl Read) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 16 * 1024];
        let header_end = loop {
            let count = stream.read(&mut buffer).expect("read request");
            assert!(count > 0, "request ended before headers");
            request.extend_from_slice(&buffer[..count]);
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .expect("request content length");
        let mut body_start = header_end;
        loop {
            while request
                .get(body_start..body_start + 4)
                .is_some_and(|prefix| prefix == b"\r\n\r\n")
            {
                body_start += 4;
            }
            if request.len() >= body_start + content_length {
                break;
            }
            let count = stream.read(&mut buffer).expect("read request body");
            assert!(count > 0, "request body ended early");
            request.extend_from_slice(&buffer[..count]);
        }
        if body_start != header_end {
            request.drain(header_end..body_start);
        }
        request.truncate(header_end + content_length);
        request
    }

    #[test]
    fn capture_options_are_explicit_and_bounded() {
        let directory = tempfile::TempDir::new().expect("options directory");
        let trace = directory.path().join("trace.json");
        let audit = directory.path().join("audit.jsonl");
        let arguments = [
            trace.into_os_string(),
            OsString::from("--audit"),
            audit.into_os_string(),
            OsString::from("--experiment-id"),
            OsString::from("run-1"),
            OsString::from("--model"),
            OsString::from("gpt-5.6"),
            OsString::from("--context-tokens"),
            OsString::from("1050000"),
            OsString::from("--max-output-tokens"),
            OsString::from("128000"),
            OsString::from("--reasoning-effort"),
            OsString::from("medium"),
        ];
        let parsed = CaptureOptions::parse(&arguments).expect("capture options");
        assert_eq!(parsed.context_tokens, 1_050_000);
        assert_eq!(parsed.repetitions, 1);
        let mut invalid = arguments;
        invalid[12] = OsString::from("auto");
        assert!(CaptureOptions::parse(&invalid).is_err());
    }

    #[test]
    fn stateless_history_keeps_exact_function_result_and_required_single_call() {
        let options = options();
        let input = vec![
            json!({"role": "developer", "content": "shared\n"}),
            json!({"type": "reasoning", "id": "reason_1", "encrypted_content": "opaque"}),
            json!({"type": "function_call", "call_id": "call_1", "name": "act", "arguments": "{\"payload\":\"pwd\\n\"}"}),
            json!({"type": "function_call_output", "call_id": "call_1", "output": "s:0\n"}),
        ];
        let body = response_request(&options, &tool_schemas(), &input);
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["store"], false);
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["input"][1]["type"], "reasoning");
        assert_eq!(body["input"][3]["output"], "s:0\n");
        assert!(
            body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .all(|tool| tool["strict"] == true)
        );
    }

    #[test]
    fn parser_keeps_visible_and_reasoning_usage_separate() {
        let response = action_response();
        let turn = parse_turn(
            &response,
            Duration::from_millis(9),
            "a".repeat(64),
            "b".repeat(64),
            "{}".to_owned(),
        )
        .expect("parsed turn");
        assert_eq!(turn.cached_input_tokens, 3);
        assert_eq!(turn.output_tokens - turn.reasoning_tokens, 3);
        assert!(matches!(
            turn.call,
            ModelCall::Act { ref payload, .. } if payload == "pwd\n"
        ));
    }

    #[tokio::test]
    async fn http_adapter_sends_bearer_request_without_recording_the_key() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener");
        let address = listener.local_addr().expect("mock address");
        let server = serve_responses(listener, vec![action_response()]);
        let client = OpenAiClient::new(format!("http://{address}/v1"), "test-secret".to_owned())
            .expect("mock client");
        let turn = client
            .respond(
                &options(),
                &tool_schemas(),
                &[json!({"role": "user", "content": "task\n"})],
            )
            .await
            .expect("mock API turn");
        server.join().expect("mock server");
        assert!(!turn.request_json.contains("test-secret"));
        let parsed: Value = serde_json::from_str(&turn.request_json).expect("request JSON");
        assert_eq!(parsed["model"], "gpt-5.6");
    }

    #[tokio::test]
    async fn full_mock_capture_writes_valid_audit_and_replays_both_arms() {
        let manifest = load_manifest().expect("task manifest");
        let seed = 7;
        let order = shuffled_indices(manifest.tasks.len(), paired_seed(seed, 0));
        let mut responses = Vec::new();
        let mut sequence = 1_usize;
        for arm in [AgentArm::Ash, AgentArm::NativeShell] {
            for &index in &order {
                let task = &manifest.tasks[index];
                match arm {
                    AgentArm::Ash => {
                        let run = execute_ash_task(task).await.expect("deterministic ASH run");
                        for step in run.steps {
                            responses.push(call_response(
                                sequence,
                                "act",
                                json!({"payload": step.request}),
                            ));
                            sequence += 1;
                        }
                    }
                    AgentArm::NativeShell => {
                        responses.push(call_response(
                            sequence,
                            "act",
                            json!({
                                "payload": task
                                    .baselines
                                    .current()
                                    .expect("platform baseline")
                                    .script
                            }),
                        ));
                        sequence += 1;
                    }
                }
                responses.push(call_response(
                    sequence,
                    "finish",
                    json!({
                        "stdout": task.expected.stdout,
                        "stderr": task.expected.stderr,
                    }),
                ));
                sequence += 1;
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").expect("full mock listener");
        let address = listener.local_addr().expect("full mock address");
        let server = serve_responses(listener, responses);
        let directory = tempfile::TempDir::new().expect("capture evidence directory");
        let trace_path = directory.path().join("trace.json");
        let audit_path = directory.path().join("audit.jsonl");
        let capture_options = CaptureOptions {
            trace_path: trace_path.clone(),
            audit_path: audit_path.clone(),
            experiment_id: "full-mock-capture".to_owned(),
            model: "gpt-5.6".to_owned(),
            context_tokens: 1_050_000,
            max_output_tokens: 128_000,
            reasoning_effort: "medium".to_owned(),
            repetitions: 1,
            seed,
        };
        let client = OpenAiClient::new(format!("http://{address}/v1"), "test-secret".to_owned())
            .expect("full mock client");
        capture_with(capture_options, client)
            .await
            .expect("full mock capture");
        server.join().expect("full mock server");
        validate_agent_trace_audit(&trace_path, &audit_path).expect("bound capture audit");
        let audit = std::fs::read_to_string(&audit_path).expect("capture audit");
        assert!(!audit.contains("test-secret"));
        let report = build_agent_report(&trace_path, &audit_path)
            .await
            .expect("captured paired replay");
        assert!(report.audit_verified);
        assert!(report.gates.passed);
        assert_eq!(report.runs.len(), 2);
        assert!(trace_path.is_file());
    }

    #[test]
    fn calendar_and_shuffle_are_stable() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_308), (2025, 8, 8));
        assert_eq!(shuffled_indices(7, 42), shuffled_indices(7, 42));
        assert_ne!(shuffled_indices(7, 42), shuffled_indices(7, 43));
    }
}
