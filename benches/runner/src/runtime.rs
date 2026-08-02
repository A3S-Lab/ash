use std::error::Error;
use std::fs;
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use ash_engine::{Engine, Parallelism, SessionConfig};
use ash_ops::PortableOperations;
use ash_platform::Workspace;
use ash_protocol::request::{
    Arguments, Budget, ExecArgs, InputSource, LIST_FILES_ONLY, ListArgs, MAX_REQUEST_RECORDS,
    MAX_REQUEST_TOKENS, Request, SEARCH_REGEX, SearchArgs, SnapshotArgs, SnapshotMode,
};
use ash_protocol::response::{
    RESULT_RETAINED, RESULT_TRUNCATED, ResultData, Status, TerminationKind,
};
use ash_store::{StoreLimits, StoreResidency};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

mod cold;
mod dispatch;
mod primitives;
mod reducer;

const DEFAULT_FILES: usize = 256;
const DEFAULT_BYTES_PER_FILE: usize = 32 * 1024;
const DEFAULT_SAMPLES: usize = 5;
const DEFAULT_STORE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_STORE_MEMORY_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_STORE_FETCH_BYTES: usize = 64 * 1024;
const DEFAULT_PROCESS_STREAM_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_PROCESS_FETCH_BYTES: usize = 64 * 1024;
const STORE_CHUNK_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const PROCESS_RESPONSE_BYTES: u64 = 64 * 1024;
const REQUEST_MILLIS: u64 = 120_000;
const NEEDLE: &str = "ASH_NEEDLE";
const REGEX_NEEDLE: &str = r"^ASH_NEEDLE file=[0-9]{6}$";
const PROCESS_READY_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_CANCEL_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_HELPER_SOURCE: &str = r#"
use std::error::Error;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const CHUNK_BYTES: usize = 16 * 1024;
const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    match arguments.next().as_deref() {
        Some(mode) if mode == "exit" => {
            if arguments.next().is_some() {
                return Err("unexpected exit argument".into());
            }
        }
        Some(mode) if mode == "respond" => {
            let request = arguments
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or("missing request")?;
            let response = arguments
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or("missing response")?;
            if arguments.next().is_some() {
                return Err("unexpected respond argument".into());
            }
            let mut input = Vec::new();
            io::stdin().read_to_end(&mut input)?;
            if input != decode_hex(&request)? {
                return Err("unexpected request input".into());
            }
            io::stdout().write_all(&decode_hex(&response)?)?;
            io::stdout().flush()?;
        }
        Some(mode) if mode == "emit" => {
            let bytes = arguments
                .next()
                .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
                .filter(|bytes: &usize| *bytes > 0 && *bytes <= MAX_STREAM_BYTES)
                .ok_or("invalid emit byte count")?;
            if arguments.next().is_some() {
                return Err("unexpected emit argument".into());
            }
            let stdout = thread::spawn(move || {
                let stdout = io::stdout();
                emit(BufWriter::new(stdout.lock()), bytes, 0)
            });
            let stderr = thread::spawn(move || {
                let stderr = io::stderr();
                emit(BufWriter::new(stderr.lock()), bytes, 13)
            });
            stdout.join().map_err(|_| "stdout thread panicked")??;
            stderr.join().map_err(|_| "stderr thread panicked")??;
        }
        Some(mode) if mode == "tree" => {
            let ready = arguments.next().ok_or("missing ready path")?;
            if arguments.next().is_some() {
                return Err("unexpected tree argument".into());
            }
            let descendant = Command::new(std::env::current_exe()?)
                .arg("hold")
                .stdin(Stdio::null())
                .spawn()?;
            fs::write(ready, descendant.id().to_string())?;
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        Some(mode) if mode == "hold" => {
            if arguments.next().is_some() {
                return Err("unexpected hold argument".into());
            }
            io::stdout().write_all(b"descendant-stdout-ready\n")?;
            io::stdout().flush()?;
            io::stderr().write_all(b"descendant-stderr-ready\n")?;
            io::stderr().flush()?;
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        _ => return Err("unknown process helper mode".into()),
    }
    Ok(())
}

fn emit(mut writer: impl Write, total: usize, seed: usize) -> io::Result<()> {
    let mut chunk = [0_u8; CHUNK_BYTES];
    let mut offset = 0_usize;
    while offset < total {
        let length = (total - offset).min(chunk.len());
        for (index, byte) in chunk[..length].iter_mut().enumerate() {
            let position = offset + index;
            *byte = if position % 127 == 126 {
                b'\n'
            } else {
                b'a' + ((position + seed) % 26) as u8
            };
        }
        writer.write_all(&chunk[..length])?;
        offset += length;
    }
    writer.flush()
}

fn decode_hex(value: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    if value.len() % 2 != 0 {
        return Err("odd response hex length".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect()
}
"#;

#[derive(Debug, Serialize)]
pub(crate) struct RuntimeReport {
    schema: u8,
    host: HostReport,
    fixture: FixtureReport,
    samples: usize,
    scenarios: Vec<ScenarioReport>,
}

#[derive(Debug, Serialize)]
struct HostReport {
    os: &'static str,
    arch: &'static str,
    available_cpus: usize,
}

#[derive(Debug, Serialize)]
struct FixtureReport {
    files: usize,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct ScenarioReport {
    id: &'static str,
    work_items: usize,
    work_bytes: u64,
    input_sha256: String,
    output_bytes: usize,
    output_sha256: String,
    runs: Vec<RuntimeRun>,
}

#[derive(Debug, Serialize)]
struct RuntimeRun {
    compute_workers: usize,
    io_workers: usize,
    observations_ns: Vec<u128>,
    p50_ns: u128,
    p95_ns: u128,
    p99_ns: u128,
    items_per_second: u128,
    bytes_per_second: u128,
    speedup_basis_points: Option<u128>,
    parallel_efficiency_basis_points: Option<u128>,
    output_sha256: String,
}

struct RuntimeFixture {
    directory: TempDir,
    report: FixtureReport,
}

struct ProcessFixture {
    directory: TempDir,
    operations: PortableOperations,
    workspace: String,
    executable: String,
    helper_source_sha256: String,
}

struct RuntimeConfig {
    files: usize,
    bytes_per_file: usize,
    samples: usize,
    store_bytes: usize,
    store_memory_bytes: usize,
    store_fetch_bytes: usize,
    process_stream_bytes: usize,
    process_fetch_bytes: usize,
    worker_counts: Vec<usize>,
}

struct StoreWorkload {
    input: Vec<u8>,
    memory_bytes: usize,
    fetch_bytes: usize,
}

struct WorkspaceWorkload<'a> {
    scenario: Scenario,
    workspace: &'a str,
    fixture: &'a FixtureReport,
    request: Request,
}

impl RuntimeConfig {
    fn detected() -> Self {
        let available = available_cpus();
        let mut worker_counts = vec![1, 2, 4, 8, available];
        worker_counts.retain(|workers| *workers <= available);
        worker_counts.sort_unstable();
        worker_counts.dedup();
        Self {
            files: DEFAULT_FILES,
            bytes_per_file: DEFAULT_BYTES_PER_FILE,
            samples: DEFAULT_SAMPLES,
            store_bytes: DEFAULT_STORE_BYTES,
            store_memory_bytes: DEFAULT_STORE_MEMORY_BYTES,
            store_fetch_bytes: DEFAULT_STORE_FETCH_BYTES,
            process_stream_bytes: DEFAULT_PROCESS_STREAM_BYTES,
            process_fetch_bytes: DEFAULT_PROCESS_FETCH_BYTES,
            worker_counts,
        }
    }

    fn validate(&mut self) -> Result<(), io::Error> {
        self.worker_counts.sort_unstable();
        self.worker_counts.dedup();
        if self.files == 0
            || self.bytes_per_file < NEEDLE.len() + 1
            || self.samples == 0
            || self.store_bytes == 0
            || self.store_memory_bytes >= self.store_bytes
            || self.store_fetch_bytes == 0
            || self.store_fetch_bytes > self.store_bytes
            || self.process_stream_bytes == 0
            || self.process_stream_bytes > 64 * 1024 * 1024
            || self.process_fetch_bytes == 0
            || self.process_fetch_bytes > self.process_stream_bytes
            || self.worker_counts.is_empty()
            || self.worker_counts[0] != 1
            || self.worker_counts.contains(&0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid runtime benchmark configuration",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Scenario {
    List,
    SearchLiteral,
    SearchRegex,
    Snapshot,
}

impl Scenario {
    const ALL: [Self; 4] = [
        Self::List,
        Self::SearchLiteral,
        Self::SearchRegex,
        Self::Snapshot,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::List => "list-recursive",
            Self::SearchLiteral => "search-literal",
            Self::SearchRegex => "search-regex",
            Self::Snapshot => "snapshot-blake3",
        }
    }

    const fn work_bytes(self, fixture_bytes: u64) -> u64 {
        match self {
            Self::List => 0,
            Self::SearchLiteral | Self::SearchRegex | Self::Snapshot => fixture_bytes,
        }
    }

    fn request(self, files: usize) -> Result<Request, Box<dyn Error>> {
        let paths = fixture_roots(files);
        let arguments = match self {
            Self::List => Arguments::List(ListArgs::new(paths, 64, LIST_FILES_ONLY)?),
            Self::SearchLiteral => Arguments::Search(SearchArgs::new(NEEDLE, paths, 0)?),
            Self::SearchRegex => {
                Arguments::Search(SearchArgs::new(REGEX_NEEDLE, paths, SEARCH_REGEX)?)
            }
            Self::Snapshot => Arguments::Snapshot(SnapshotArgs::new(
                paths,
                64,
                SnapshotMode::Capture,
                None,
                0,
            )?),
        };
        Ok(Request::new(
            1,
            arguments,
            Budget::new(MAX_REQUEST_TOKENS, MAX_REQUEST_RECORDS, REQUEST_MILLIS)?,
        )?)
    }

    fn validate_response(
        self,
        response: &ash_protocol::response::FinalResponse,
        files: usize,
    ) -> Result<(), io::Error> {
        let entries = match (self, response.data()) {
            (Self::List, Some(ResultData::List(entries))) => entries.len(),
            (Self::SearchLiteral | Self::SearchRegex, Some(ResultData::Search(matches))) => {
                matches.len()
            }
            (Self::Snapshot, Some(ResultData::Snapshot(entries))) if !entries.is_empty() => {
                return Ok(());
            }
            _ => {
                return Err(io::Error::other(format!(
                    "{} returned an unexpected result type",
                    self.id()
                )));
            }
        };
        if entries != files {
            return Err(io::Error::other(format!(
                "{} returned {entries} records for {files} fixture files",
                self.id()
            )));
        }
        Ok(())
    }
}

fn fixture_roots(files: usize) -> Vec<String> {
    (0..files.clamp(1, 16))
        .map(|bucket| format!("src/d{bucket:02}"))
        .collect()
}

pub(crate) async fn runtime_report(
    ash_binary: Option<&std::path::Path>,
) -> Result<RuntimeReport, Box<dyn Error>> {
    runtime_report_with_config(RuntimeConfig::detected(), ash_binary).await
}

async fn runtime_report_with_config(
    mut config: RuntimeConfig,
    ash_binary: Option<&std::path::Path>,
) -> Result<RuntimeReport, Box<dyn Error>> {
    config.validate()?;
    let fixture = prepare_fixture(config.files, config.bytes_per_file)?;
    let workspace = Workspace::new(fixture.directory.path())?;
    let operations = PortableOperations::new(workspace);
    let workspace_text = fixture.directory.path().to_string_lossy().into_owned();
    let process_fixture = prepare_process_fixture()?;
    let cold_fixture = cold::prepare_fixture(ash_binary, &process_fixture)?;
    let mut scenarios = Vec::with_capacity(Scenario::ALL.len() + 12);
    for scenario in Scenario::ALL {
        scenarios.push(
            measure_scenario(
                scenario,
                &operations,
                &workspace_text,
                &fixture.report,
                &config,
            )
            .await?,
        );
    }
    require_equivalent_output(&scenarios, "search-literal", "search-regex")?;
    scenarios.push(measure_store_scenario(&workspace_text, &config).await?);
    scenarios
        .push(cold::measure_cold_startup_scenario(&cold_fixture, &workspace_text, &config).await?);
    scenarios.push(measure_process_spawn_scenario(&process_fixture, &config).await?);
    scenarios.push(measure_process_capture_scenario(&process_fixture, &config).await?);
    scenarios.push(measure_process_cancel_scenario(&process_fixture, &config).await?);
    scenarios.push(dispatch::measure_rpc_dispatch_scenario(&workspace_text, &config).await?);
    scenarios.push(
        reducer::measure_structured_projection_scenario(&operations, &workspace_text, &config)
            .await?,
    );
    scenarios.push(reducer::measure_repeated_line_scenario(&config)?);
    scenarios.push(primitives::measure_path_dictionary_scenario(&config)?);
    for (nodes, id) in primitives::DAG_SCENARIOS {
        scenarios.push(primitives::measure_dag_scenario(nodes, id, &config).await?);
    }
    Ok(RuntimeReport {
        schema: 10,
        host: HostReport {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            available_cpus: available_cpus(),
        },
        fixture: fixture.report,
        samples: config.samples,
        scenarios,
    })
}

fn require_equivalent_output(
    scenarios: &[ScenarioReport],
    left_id: &str,
    right_id: &str,
) -> Result<(), io::Error> {
    let find = |id| scenarios.iter().find(|scenario| scenario.id == id);
    let (Some(left), Some(right)) = (find(left_id), find(right_id)) else {
        return Err(io::Error::other(format!(
            "missing equivalent runtime scenarios {left_id} and {right_id}"
        )));
    };
    if left.output_bytes != right.output_bytes || left.output_sha256 != right.output_sha256 {
        return Err(io::Error::other(format!(
            "runtime scenarios {left_id} and {right_id} emitted different evidence"
        )));
    }
    Ok(())
}

async fn measure_process_spawn_scenario(
    fixture: &ProcessFixture,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let request = process_request(2, fixture, vec!["exit".to_owned()])?;
    let input_sha256 = process_input_sha256(fixture, "mode=exit\n");
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        let (warm_output, _) =
            execute_process_spawn_once(&engine, fixture, parallelism, &request, 15_000).await?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = u64::try_from(sample)?.saturating_add(15_001);
            let (output, elapsed) =
                execute_process_spawn_once(&engine, fixture, parallelism, &request, session_id)
                    .await?;
            require_stable_output(&mut expected_output, &output)?;
            observations.push(elapsed);
        }
        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("process spawn benchmark emitted no output"))?;
        runs.push(runtime_run(
            parallelism,
            observations,
            &mut baseline,
            1,
            0,
            output,
        ));
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing process spawn output"))?;
    Ok(ScenarioReport {
        id: "exec-spawn-empty",
        work_items: 1,
        work_bytes: 0,
        input_sha256,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_process_spawn_once(
    engine: &Engine,
    fixture: &ProcessFixture,
    parallelism: Parallelism,
    request: &Request,
    session_id: u64,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let session = engine.open_session(SessionConfig::new(
        session_id,
        &fixture.workspace,
        PROCESS_RESPONSE_BYTES,
        parallelism,
    ))?;
    let started = Instant::now();
    let program = session.begin(request).await?;
    let response = fixture.operations.execute(request, &program).await?;
    let _canonical = response.encode()?.encode();
    let elapsed = started.elapsed().as_nanos().max(1);
    let result = match response.data() {
        Some(ResultData::Exec(result)) => result,
        _ => return Err(io::Error::other("unexpected empty process result").into()),
    };
    if response.status() != Status::Success
        || response.flags() != 0
        || result.termination != TerminationKind::Exited
        || result.code != Some(0)
        || result.stdout.projection.is_some()
        || result.stdout.reference.is_some()
        || result.stderr.projection.is_some()
        || result.stderr.reference.is_some()
    {
        return Err(io::Error::other("empty process spawn produced unexpected evidence").into());
    }
    let output = format!(
        "status={}\ntermination={}\ncode={}\nstdout=0\nstderr=0\nflags={}\n",
        response.status().code(),
        result.termination as u8,
        result.code.unwrap_or_default(),
        response.flags(),
    )
    .into_bytes();
    drop(program);
    drop(session);
    Ok((output, elapsed))
}

async fn measure_store_scenario(
    workspace: &str,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let request = Scenario::SearchLiteral.request(1)?;
    let workload = StoreWorkload {
        input: fixture_bytes(0x5a17, config.store_bytes),
        memory_bytes: config.store_memory_bytes,
        fetch_bytes: config.store_fetch_bytes,
    };
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        let (warm_output, _) =
            execute_store_once(&engine, workspace, parallelism, &request, 10_000, &workload)
                .await?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = u64::try_from(sample)?.saturating_add(10_001);
            let (output, elapsed) = execute_store_once(
                &engine,
                workspace,
                parallelism,
                &request,
                session_id,
                &workload,
            )
            .await?;
            require_stable_output(&mut expected_output, &output)?;
            observations.push(elapsed);
        }

        let mut ordered = observations.clone();
        ordered.sort_unstable();
        let p50 = percentile(&ordered, 50);
        let p95 = percentile(&ordered, 95);
        let p99 = percentile(&ordered, 99);
        let baseline = *baseline.get_or_insert(p50);
        let speedup = ratio_basis_points(baseline, p50);
        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("store benchmark emitted no output"))?;
        runs.push(RuntimeRun {
            compute_workers: workers,
            io_workers: parallelism.io_workers().get(),
            observations_ns: observations,
            p50_ns: p50,
            p95_ns: p95,
            p99_ns: p99,
            items_per_second: throughput(1, p50),
            bytes_per_second: throughput(workload.input.len() as u128, p50),
            speedup_basis_points: Some(speedup),
            parallel_efficiency_basis_points: Some(speedup / workers as u128),
            output_sha256: sha256_hex(output),
        });
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing store output"))?;
    Ok(ScenarioReport {
        id: "result-store-spill-fetch",
        work_items: 1,
        work_bytes: u64::try_from(workload.input.len())?,
        input_sha256: sha256_hex(&workload.input),
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_store_once(
    engine: &Engine,
    workspace: &str,
    parallelism: Parallelism,
    request: &Request,
    session_id: u64,
    workload: &StoreWorkload,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let mut session_config =
        SessionConfig::new(session_id, workspace, MAX_RESPONSE_BYTES, parallelism);
    session_config.store_limits = StoreLimits {
        max_bytes: u64::try_from(workload.input.len())?,
        max_entries: 2,
    };
    let session = engine.open_session(session_config)?;
    let program = session.begin(request).await?;
    let store = program.store().clone();
    let started = Instant::now();
    let mut capture = store.capture(workload.memory_bytes);
    for chunk in workload.input.chunks(STORE_CHUNK_BYTES) {
        capture.append(chunk).await?;
    }
    let captured = capture.finish().await?;
    if captured.residency() != StoreResidency::Disk {
        return Err(io::Error::other("store benchmark did not spill").into());
    }
    let commit_store = store.clone();
    let aliases = program
        .compute_pool()
        .run(move || commit_store.retain_captures(vec![captured]))
        .await??;
    let alias = *aliases
        .first()
        .ok_or_else(|| io::Error::other("store benchmark returned no alias"))?;
    let lease = store.get(alias)?;
    let fetch_bytes = u64::try_from(workload.fetch_bytes)?;
    let offset = lease.len().saturating_sub(fetch_bytes);
    let output = lease.read_range(offset, fetch_bytes, fetch_bytes).await?;
    drop(lease);
    store.release(alias)?;
    drop(store);
    drop(program);
    drop(session);
    let elapsed = started.elapsed().as_nanos().max(1);
    Ok((output, elapsed))
}

async fn measure_process_capture_scenario(
    fixture: &ProcessFixture,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let request = process_request(
        2,
        fixture,
        vec!["emit".to_owned(), config.process_stream_bytes.to_string()],
    )?;
    let work_bytes = u64::try_from(config.process_stream_bytes)?
        .checked_mul(2)
        .ok_or_else(|| io::Error::other("process capture workload is too large"))?;
    let input_sha256 = process_input_sha256(
        fixture,
        &format!(
            "mode=emit\nstream_bytes={}\nfetch_bytes={}\n",
            config.process_stream_bytes, config.process_fetch_bytes
        ),
    );
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        let (warm_output, _) = execute_process_capture_once(
            &engine,
            fixture,
            parallelism,
            &request,
            20_000,
            config.process_stream_bytes,
            config.process_fetch_bytes,
        )
        .await?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = u64::try_from(sample)?.saturating_add(20_001);
            let (output, elapsed) = execute_process_capture_once(
                &engine,
                fixture,
                parallelism,
                &request,
                session_id,
                config.process_stream_bytes,
                config.process_fetch_bytes,
            )
            .await?;
            require_stable_output(&mut expected_output, &output)?;
            observations.push(elapsed);
        }
        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("process capture benchmark emitted no output"))?;
        runs.push(runtime_run(
            parallelism,
            observations,
            &mut baseline,
            2,
            u128::from(work_bytes),
            output,
        ));
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing capture output"))?;
    Ok(ScenarioReport {
        id: "exec-capture-pressure",
        work_items: 2,
        work_bytes,
        input_sha256,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_process_capture_once(
    engine: &Engine,
    fixture: &ProcessFixture,
    parallelism: Parallelism,
    request: &Request,
    session_id: u64,
    stream_bytes: usize,
    fetch_bytes: usize,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let retained_bytes = u64::try_from(stream_bytes)?
        .checked_mul(2)
        .ok_or_else(|| io::Error::other("process capture is too large"))?;
    let mut session_config = SessionConfig::new(
        session_id,
        &fixture.workspace,
        PROCESS_RESPONSE_BYTES,
        parallelism,
    );
    session_config.store_limits = StoreLimits {
        max_bytes: retained_bytes,
        max_entries: 2,
    };
    let session = engine.open_session(session_config)?;
    let started = Instant::now();
    let program = session.begin(request).await?;
    let response = fixture.operations.execute(request, &program).await?;
    let _canonical = response.encode()?.encode();
    let elapsed = started.elapsed().as_nanos().max(1);
    if response.status() != Status::Success
        || response.flags() & (RESULT_RETAINED | RESULT_TRUNCATED)
            != (RESULT_RETAINED | RESULT_TRUNCATED)
    {
        return Err(io::Error::other("process capture did not retain reduced evidence").into());
    }
    let (stdout_reference, stderr_reference) = match response.data() {
        Some(ResultData::Exec(result)) if result.termination == TerminationKind::Exited => (
            result
                .stdout
                .reference
                .ok_or_else(|| io::Error::other("stdout was not retained"))?,
            result
                .stderr
                .reference
                .ok_or_else(|| io::Error::other("stderr was not retained"))?,
        ),
        _ => return Err(io::Error::other("unexpected process capture result").into()),
    };
    if stdout_reference == stderr_reference {
        return Err(io::Error::other("process streams unexpectedly deduplicated").into());
    }
    let store = program.store().clone();
    let mut output = Vec::with_capacity(fetch_bytes.saturating_mul(2));
    for reference in [stdout_reference, stderr_reference] {
        let lease = store.get(reference)?;
        if lease.len() != u64::try_from(stream_bytes)? || lease.residency() != StoreResidency::Disk
        {
            return Err(io::Error::other("process stream did not remain disk-backed").into());
        }
        let fetch_bytes = u64::try_from(fetch_bytes)?;
        output.extend_from_slice(
            &lease
                .read_range(
                    lease.len().saturating_sub(fetch_bytes),
                    fetch_bytes,
                    fetch_bytes,
                )
                .await?,
        );
        drop(lease);
        store.release(reference)?;
    }
    let mut expected = process_stream_tail(stream_bytes, fetch_bytes, 0);
    expected.extend_from_slice(&process_stream_tail(stream_bytes, fetch_bytes, 13));
    if output != expected {
        return Err(io::Error::other("retained process stream bytes changed").into());
    }
    drop(store);
    drop(program);
    drop(session);
    Ok((output, elapsed))
}

async fn measure_process_cancel_scenario(
    fixture: &ProcessFixture,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let input_sha256 = process_input_sha256(fixture, "mode=tree\n");
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        let (warm_output, _) =
            execute_process_cancel_once(&engine, fixture, parallelism, 30_000).await?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = u64::try_from(sample)?.saturating_add(30_001);
            let (output, elapsed) =
                execute_process_cancel_once(&engine, fixture, parallelism, session_id).await?;
            require_stable_output(&mut expected_output, &output)?;
            observations.push(elapsed);
        }
        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("process cancellation emitted no output"))?;
        runs.push(runtime_run(
            parallelism,
            observations,
            &mut baseline,
            1,
            0,
            output,
        ));
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing cancellation output"))?;
    Ok(ScenarioReport {
        id: "exec-cancel-tree-empty",
        work_items: 1,
        work_bytes: 0,
        input_sha256,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_process_cancel_once(
    engine: &Engine,
    fixture: &ProcessFixture,
    parallelism: Parallelism,
    session_id: u64,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let ready_name = format!(".cancel-ready-{session_id}");
    let ready_path = fixture.directory.path().join(&ready_name);
    match fs::remove_file(&ready_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let request = process_request(3, fixture, vec!["tree".to_owned(), ready_name])?;
    let session = engine.open_session(SessionConfig::new(
        session_id,
        &fixture.workspace,
        PROCESS_RESPONSE_BYTES,
        parallelism,
    ))?;
    let program = session.begin(&request).await?;
    let operations = fixture.operations.clone();
    let task_request = request.clone();
    let mut task = tokio::spawn(async move { operations.execute(&task_request, &program).await });
    let ready = tokio::time::timeout(PROCESS_READY_TIMEOUT, async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(&ready_path).await
                && value
                    .parse::<u32>()
                    .ok()
                    .is_some_and(|process_id| process_id > 0)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    if ready.is_err() {
        let _ = session.cancel(request.id());
        if tokio::time::timeout(PROCESS_CANCEL_TIMEOUT, &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        return Err(io::Error::other("process tree did not become ready").into());
    }

    let started = Instant::now();
    if !session.cancel(request.id())? {
        task.abort();
        let _ = task.await;
        return Err(io::Error::other("active process request was not cancellable").into());
    }
    let response = match tokio::time::timeout(PROCESS_CANCEL_TIMEOUT, &mut task).await {
        Ok(result) => result??,
        Err(_) => {
            task.abort();
            let _ = task.await;
            return Err(io::Error::other("process-tree cancellation exceeded its bound").into());
        }
    };
    let _canonical = response.encode()?.encode();
    let elapsed = started.elapsed().as_nanos().max(1);
    let termination = match response.data() {
        Some(ResultData::Exec(result)) => result.termination,
        _ => return Err(io::Error::other("unexpected cancellation result").into()),
    };
    if response.status() != Status::Cancelled || termination != TerminationKind::Cancelled {
        return Err(io::Error::other("process tree did not report cancellation").into());
    }
    if session.cancel(request.id())? {
        return Err(io::Error::other("cancelled process remained registered").into());
    }
    tokio::fs::remove_file(&ready_path).await?;
    drop(session);
    Ok((
        format!(
            "status={}\ntermination={}\n",
            response.status().code(),
            termination as u8
        )
        .into_bytes(),
        elapsed,
    ))
}

fn process_request(
    request_id: u64,
    fixture: &ProcessFixture,
    argv: Vec<String>,
) -> Result<Request, Box<dyn Error>> {
    Ok(Request::new(
        request_id,
        Arguments::Exec(ExecArgs::new(
            &fixture.executable,
            argv,
            ".",
            vec![],
            InputSource::None,
            0,
        )?),
        Budget::new(MAX_REQUEST_TOKENS, MAX_REQUEST_RECORDS, REQUEST_MILLIS)?,
    )?)
}

fn process_input_sha256(fixture: &ProcessFixture, scenario: &str) -> String {
    sha256_hex(
        format!(
            "helper_source_sha256={}\n{scenario}",
            fixture.helper_source_sha256
        )
        .as_bytes(),
    )
}

fn process_stream_tail(total: usize, length: usize, seed: usize) -> Vec<u8> {
    let start = total.saturating_sub(length);
    (start..total)
        .map(|position| {
            if position % 127 == 126 {
                b'\n'
            } else {
                b'a' + ((position + seed) % 26) as u8
            }
        })
        .collect()
}

fn runtime_run(
    parallelism: Parallelism,
    observations: Vec<u128>,
    baseline: &mut Option<u128>,
    work_items: u128,
    work_bytes: u128,
    output: &[u8],
) -> RuntimeRun {
    let workers = parallelism.compute_workers().get();
    let mut ordered = observations.clone();
    ordered.sort_unstable();
    let p50 = percentile(&ordered, 50);
    let p95 = percentile(&ordered, 95);
    let p99 = percentile(&ordered, 99);
    let baseline = *baseline.get_or_insert(p50);
    let speedup = ratio_basis_points(baseline, p50);
    RuntimeRun {
        compute_workers: workers,
        io_workers: parallelism.io_workers().get(),
        observations_ns: observations,
        p50_ns: p50,
        p95_ns: p95,
        p99_ns: p99,
        items_per_second: throughput(work_items, p50),
        bytes_per_second: throughput(work_bytes, p50),
        speedup_basis_points: Some(speedup),
        parallel_efficiency_basis_points: Some(speedup / workers as u128),
        output_sha256: sha256_hex(output),
    }
}

async fn measure_scenario(
    scenario: Scenario,
    operations: &PortableOperations,
    workspace: &str,
    fixture: &FixtureReport,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let workload = WorkspaceWorkload {
        scenario,
        workspace,
        fixture,
        request: scenario.request(fixture.files)?,
    };
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        let (warm_output, _) = execute_once(&workload, &engine, operations, parallelism, 1).await?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = u64::try_from(sample)?.saturating_add(2);
            let (output, elapsed) =
                execute_once(&workload, &engine, operations, parallelism, session_id).await?;
            require_stable_output(&mut expected_output, &output)?;
            observations.push(elapsed);
        }

        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("runtime benchmark emitted no output"))?;
        runs.push(runtime_run(
            parallelism,
            observations,
            &mut baseline,
            fixture.files as u128,
            u128::from(scenario.work_bytes(fixture.bytes)),
            output,
        ));
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing runtime output"))?;
    Ok(ScenarioReport {
        id: scenario.id(),
        work_items: fixture.files,
        work_bytes: scenario.work_bytes(fixture.bytes),
        input_sha256: fixture.sha256.clone(),
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_once(
    workload: &WorkspaceWorkload<'_>,
    engine: &Engine,
    operations: &PortableOperations,
    parallelism: Parallelism,
    session_id: u64,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let session = engine.open_session(SessionConfig::new(
        session_id,
        workload.workspace,
        MAX_RESPONSE_BYTES,
        parallelism,
    ))?;
    let started = Instant::now();
    let program = session.begin(&workload.request).await?;
    let response = operations.execute(&workload.request, &program).await?;
    if response.status() != Status::Success {
        return Err(io::Error::other(format!(
            "runtime scenario returned status {}",
            response.status().code()
        ))
        .into());
    }
    workload
        .scenario
        .validate_response(&response, workload.fixture.files)?;
    let output = response.encode()?.encode().into_bytes();
    let elapsed = started.elapsed().as_nanos().max(1);
    drop(program);
    Ok((output, elapsed))
}

fn prepare_fixture(files: usize, bytes_per_file: usize) -> Result<RuntimeFixture, io::Error> {
    let directory = tempfile::Builder::new().prefix("ash-runtime-").tempdir()?;
    let buckets = files.clamp(1, 16);
    let mut digest = Sha256::new();
    let mut total_bytes = 0_u64;
    for index in 0..files {
        let logical = format!("src/d{:02}/f{index:06}.txt", index % buckets);
        let native = logical
            .split('/')
            .fold(directory.path().to_path_buf(), |path, part| path.join(part));
        let parent = native
            .parent()
            .ok_or_else(|| io::Error::other("runtime fixture path has no parent"))?;
        fs::create_dir_all(parent)?;
        let bytes = fixture_bytes(index, bytes_per_file);
        fs::write(&native, &bytes)?;
        digest.update((logical.len() as u64).to_le_bytes());
        digest.update(logical.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(&bytes);
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("runtime fixture is too large"))?;
    }
    Ok(RuntimeFixture {
        directory,
        report: FixtureReport {
            files,
            bytes: total_bytes,
            sha256: hex(&digest.finalize()),
        },
    })
}

fn prepare_process_fixture() -> Result<ProcessFixture, Box<dyn Error>> {
    let directory = tempfile::Builder::new()
        .prefix("ash-runtime-process-")
        .tempdir()?;
    let bin_directory = directory.path().join("bin");
    fs::create_dir(&bin_directory)?;
    let source = directory.path().join("runtime-process-helper.rs");
    fs::write(&source, PROCESS_HELPER_SOURCE)?;
    let executable_name = if cfg!(windows) {
        "runtime-process-helper.exe"
    } else {
        "runtime-process-helper"
    };
    let executable_path = bin_directory.join(executable_name);
    let status = Command::new("rustc")
        .arg("--edition=2024")
        .arg("-O")
        .arg(&source)
        .arg("-o")
        .arg(&executable_path)
        .status()?;
    if !status.success() {
        return Err(io::Error::other("failed to compile runtime process helper").into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&executable_path, fs::Permissions::from_mode(0o700))?;
    }
    let workspace = Workspace::new(directory.path())?;
    Ok(ProcessFixture {
        workspace: directory.path().to_string_lossy().into_owned(),
        operations: PortableOperations::new(workspace),
        executable: format!("bin/{executable_name}"),
        helper_source_sha256: sha256_hex(PROCESS_HELPER_SOURCE.as_bytes()),
        directory,
    })
}

fn fixture_bytes(index: usize, length: usize) -> Vec<u8> {
    let mut output = format!("{NEEDLE} file={index:06}\n").into_bytes();
    let mut state = (index as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    while output.len() < length {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let byte = if output.len() % 127 == 126 {
            b'\n'
        } else {
            b'a' + (value % 26) as u8
        };
        output.push(byte);
    }
    output.truncate(length);
    output
}

fn require_stable_output(expected: &mut Option<Vec<u8>>, actual: &[u8]) -> Result<(), io::Error> {
    match expected {
        Some(expected) if expected != actual => Err(io::Error::other(
            "worker counts produced different output bytes",
        )),
        Some(_) => Ok(()),
        None => {
            *expected = Some(actual.to_vec());
            Ok(())
        }
    }
}

fn percentile(ordered: &[u128], percent: usize) -> u128 {
    let rank = percent.saturating_mul(ordered.len()).div_ceil(100);
    ordered[rank.saturating_sub(1).min(ordered.len() - 1)]
}

fn throughput(work: u128, nanoseconds: u128) -> u128 {
    work.saturating_mul(1_000_000_000)
        .checked_div(nanoseconds)
        .unwrap_or(0)
}

fn ratio_basis_points(numerator: u128, denominator: u128) -> u128 {
    numerator
        .saturating_mul(10_000)
        .checked_div(denominator)
        .unwrap_or(0)
}

fn available_cpus() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{RuntimeConfig, runtime_report_with_config};

    #[tokio::test(flavor = "current_thread")]
    async fn real_runtime_outputs_are_stable_across_worker_counts() {
        let report = runtime_report_with_config(
            RuntimeConfig {
                files: 8,
                bytes_per_file: 4 * 1024,
                samples: 2,
                store_bytes: 128 * 1024,
                store_memory_bytes: 16 * 1024,
                store_fetch_bytes: 4 * 1024,
                process_stream_bytes: 5 * 1024 * 1024,
                process_fetch_bytes: 4 * 1024,
                worker_counts: vec![2, 1, 2],
            },
            None,
        )
        .await
        .expect("runtime report");

        assert_eq!(report.schema, 10);
        assert_eq!(report.fixture.files, 8);
        assert_eq!(report.fixture.bytes, 8 * 4 * 1024);
        assert_eq!(report.samples, 2);
        assert_eq!(report.scenarios.len(), 16);
        for scenario in &report.scenarios {
            assert!(scenario.output_bytes > 0);
            for run in &scenario.runs {
                assert_eq!(run.observations_ns.len(), 2);
                assert!(run.p50_ns <= run.p95_ns);
                assert!(run.p95_ns <= run.p99_ns);
                assert_eq!(run.output_sha256, scenario.output_sha256);
            }
        }
        let scaled = report
            .scenarios
            .iter()
            .filter(|scenario| scenario.runs[0].speedup_basis_points.is_some())
            .collect::<Vec<_>>();
        assert_eq!(scaled.len(), 11);
        for scenario in scaled {
            assert_eq!(scenario.runs.len(), 2);
            assert_eq!(scenario.runs[0].compute_workers, 1);
            assert_eq!(scenario.runs[0].io_workers, 1);
            assert_eq!(scenario.runs[1].compute_workers, 2);
            assert_eq!(scenario.runs[1].io_workers, 2);
            assert_eq!(scenario.runs[0].speedup_basis_points, Some(10_000));
            assert_eq!(
                scenario.runs[0].parallel_efficiency_basis_points,
                Some(10_000)
            );
        }
        let matrix = &report.scenarios[..4];
        assert_eq!(
            matrix
                .iter()
                .map(|scenario| scenario.id)
                .collect::<Vec<_>>(),
            vec![
                "list-recursive",
                "search-literal",
                "search-regex",
                "snapshot-blake3"
            ]
        );
        for scenario in matrix {
            assert_eq!(scenario.work_items, 8);
            assert_eq!(&scenario.input_sha256, &report.fixture.sha256);
        }
        assert_eq!(matrix[0].work_bytes, 0);
        for scenario in &matrix[1..] {
            assert_eq!(scenario.work_bytes, report.fixture.bytes);
        }
        assert_ne!(
            report.scenarios[0].output_sha256,
            report.scenarios[1].output_sha256
        );
        assert_eq!(
            report.scenarios[1].output_sha256,
            report.scenarios[2].output_sha256
        );
        assert_ne!(
            report.scenarios[2].output_sha256,
            report.scenarios[3].output_sha256
        );
        let store = &report.scenarios[4];
        assert_eq!(store.id, "result-store-spill-fetch");
        assert_eq!(store.work_items, 1);
        assert_eq!(store.work_bytes, 128 * 1024);
        assert_ne!(&store.input_sha256, &report.fixture.sha256);
        assert_eq!(store.output_bytes, 4 * 1024);

        let cold = &report.scenarios[5];
        assert_eq!(cold.id, "cli-cold-startup");
        assert_eq!(cold.work_items, 1);
        assert!(cold.work_bytes > 0);
        assert_eq!(cold.runs.len(), 1);
        assert_eq!(cold.runs[0].observations_ns.len(), 2);
        assert_eq!(cold.runs[0].speedup_basis_points, None);
        assert_eq!(cold.runs[0].parallel_efficiency_basis_points, None);

        let spawn = &report.scenarios[6];
        assert_eq!(spawn.id, "exec-spawn-empty");
        assert_eq!(spawn.work_items, 1);
        assert_eq!(spawn.work_bytes, 0);

        let capture = &report.scenarios[7];
        assert_eq!(capture.id, "exec-capture-pressure");
        assert_eq!(capture.work_items, 2);
        assert_eq!(capture.work_bytes, 2 * 5 * 1024 * 1024);
        assert_eq!(capture.output_bytes, 2 * 4 * 1024);

        let cancellation = &report.scenarios[8];
        assert_eq!(cancellation.id, "exec-cancel-tree-empty");
        assert_eq!(cancellation.work_items, 1);
        assert_eq!(cancellation.work_bytes, 0);

        let dispatch = &report.scenarios[9];
        assert_eq!(dispatch.id, "rpc-warm-dispatch");
        assert_eq!(dispatch.work_items, 1);
        assert!(dispatch.work_bytes > 0);

        let reducer = &report.scenarios[10];
        assert_eq!(reducer.id, "ref-project-structured");
        assert_eq!(reducer.work_items, 8 * 64);
        assert!(reducer.work_bytes > 0);
        assert_eq!(reducer.runs.len(), 2);

        let repeated = &report.scenarios[11];
        assert_eq!(repeated.id, "reduce-repeated-lines");
        assert_eq!(repeated.work_items, 8 * 512);
        assert!(repeated.work_bytes > repeated.output_bytes as u64);
        assert_eq!(repeated.runs.len(), 2);

        let primitives = &report.scenarios[12..];
        assert_eq!(
            primitives
                .iter()
                .map(|scenario| scenario.id)
                .collect::<Vec<_>>(),
            vec![
                "path-dictionary-hot",
                "dag-schedule-64",
                "dag-schedule-256",
                "dag-schedule-1024"
            ]
        );
        for scenario in primitives {
            assert_eq!(scenario.runs.len(), 1);
            assert_eq!(scenario.runs[0].compute_workers, 1);
            assert_eq!(scenario.runs[0].io_workers, 1);
            assert_eq!(scenario.runs[0].speedup_basis_points, None);
            assert_eq!(scenario.runs[0].parallel_efficiency_basis_points, None);
            assert!(scenario.work_bytes > 0);
        }
        let dictionary = &primitives[0];
        assert_eq!(dictionary.work_items, 4_096);
        assert_eq!(dictionary.output_bytes, 4_096 * 8);
        for (scenario, nodes) in primitives[1..].iter().zip([64, 256, 1_024]) {
            assert_eq!(scenario.work_items, nodes);
            assert_eq!(scenario.output_bytes, nodes * 8);
        }
    }
}
