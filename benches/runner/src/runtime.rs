use std::error::Error;
use std::fs;
use std::io;
use std::time::Instant;

use ash_engine::{Engine, Parallelism, SessionConfig};
use ash_ops::PortableOperations;
use ash_platform::Workspace;
use ash_protocol::request::{
    Arguments, Budget, MAX_REQUEST_RECORDS, MAX_REQUEST_TOKENS, Request, SearchArgs, SnapshotArgs,
    SnapshotMode,
};
use ash_protocol::response::Status;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const DEFAULT_FILES: usize = 256;
const DEFAULT_BYTES_PER_FILE: usize = 32 * 1024;
const DEFAULT_SAMPLES: usize = 5;
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const REQUEST_MILLIS: u64 = 120_000;
const NEEDLE: &str = "ASH_NEEDLE";

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
    output_bytes: usize,
    output_sha256: String,
    runs: Vec<RuntimeRun>,
}

#[derive(Debug, Serialize)]
struct RuntimeRun {
    workers: usize,
    observations_ns: Vec<u128>,
    p50_ns: u128,
    p95_ns: u128,
    p99_ns: u128,
    items_per_second: u128,
    bytes_per_second: u128,
    speedup_basis_points: u128,
    parallel_efficiency_basis_points: u128,
    output_sha256: String,
}

struct RuntimeFixture {
    directory: TempDir,
    report: FixtureReport,
}

struct RuntimeConfig {
    files: usize,
    bytes_per_file: usize,
    samples: usize,
    worker_counts: Vec<usize>,
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
            worker_counts,
        }
    }

    fn validate(&mut self) -> Result<(), io::Error> {
        self.worker_counts.sort_unstable();
        self.worker_counts.dedup();
        if self.files == 0
            || self.bytes_per_file < NEEDLE.len() + 1
            || self.samples == 0
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
    Search,
    Snapshot,
}

impl Scenario {
    const ALL: [Self; 2] = [Self::Search, Self::Snapshot];

    const fn id(self) -> &'static str {
        match self {
            Self::Search => "search-literal",
            Self::Snapshot => "snapshot-blake3",
        }
    }

    fn request(self) -> Result<Request, Box<dyn Error>> {
        let arguments = match self {
            Self::Search => Arguments::Search(SearchArgs::new(NEEDLE, vec![".".to_owned()], 0)?),
            Self::Snapshot => Arguments::Snapshot(SnapshotArgs::new(
                vec![".".to_owned()],
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
}

pub(crate) async fn runtime_report() -> Result<RuntimeReport, Box<dyn Error>> {
    runtime_report_with_config(RuntimeConfig::detected()).await
}

async fn runtime_report_with_config(
    mut config: RuntimeConfig,
) -> Result<RuntimeReport, Box<dyn Error>> {
    config.validate()?;
    let fixture = prepare_fixture(config.files, config.bytes_per_file)?;
    let workspace = Workspace::new(fixture.directory.path())?;
    let operations = PortableOperations::new(workspace);
    let workspace_text = fixture.directory.path().to_string_lossy().into_owned();
    let mut scenarios = Vec::with_capacity(Scenario::ALL.len());
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
    Ok(RuntimeReport {
        schema: 2,
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

async fn measure_scenario(
    scenario: Scenario,
    operations: &PortableOperations,
    workspace: &str,
    fixture: &FixtureReport,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let request = scenario.request()?;
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        let (warm_output, _) =
            execute_once(&engine, operations, workspace, parallelism, &request, 1).await?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = u64::try_from(sample)?.saturating_add(2);
            let (output, elapsed) = execute_once(
                &engine,
                operations,
                workspace,
                parallelism,
                &request,
                session_id,
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
            .ok_or_else(|| io::Error::other("runtime benchmark emitted no output"))?;
        runs.push(RuntimeRun {
            workers,
            observations_ns: observations,
            p50_ns: p50,
            p95_ns: p95,
            p99_ns: p99,
            items_per_second: throughput(fixture.files as u128, p50),
            bytes_per_second: throughput(u128::from(fixture.bytes), p50),
            speedup_basis_points: speedup,
            parallel_efficiency_basis_points: speedup / workers as u128,
            output_sha256: sha256_hex(output),
        });
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing runtime output"))?;
    Ok(ScenarioReport {
        id: scenario.id(),
        work_items: fixture.files,
        work_bytes: fixture.bytes,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_once(
    engine: &Engine,
    operations: &PortableOperations,
    workspace: &str,
    parallelism: Parallelism,
    request: &Request,
    session_id: u64,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let session = engine.open_session(SessionConfig::new(
        session_id,
        workspace,
        MAX_RESPONSE_BYTES,
        parallelism,
    ))?;
    let started = Instant::now();
    let program = session.begin(request).await?;
    let response = operations.execute(request, &program).await?;
    if response.status() != Status::Success {
        return Err(io::Error::other(format!(
            "runtime scenario returned status {}",
            response.status().code()
        ))
        .into());
    }
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
            "worker counts produced different canonical ASON",
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
    async fn real_search_and_snapshot_outputs_are_stable_across_worker_counts() {
        let report = runtime_report_with_config(RuntimeConfig {
            files: 8,
            bytes_per_file: 4 * 1024,
            samples: 2,
            worker_counts: vec![2, 1, 2],
        })
        .await
        .expect("runtime report");

        assert_eq!(report.schema, 2);
        assert_eq!(report.fixture.files, 8);
        assert_eq!(report.fixture.bytes, 8 * 4 * 1024);
        assert_eq!(report.samples, 2);
        assert_eq!(report.scenarios.len(), 2);
        for scenario in &report.scenarios {
            assert_eq!(scenario.work_items, 8);
            assert_eq!(scenario.work_bytes, report.fixture.bytes);
            assert!(scenario.output_bytes > 0);
            assert_eq!(scenario.runs.len(), 2);
            assert_eq!(scenario.runs[0].workers, 1);
            assert_eq!(scenario.runs[0].speedup_basis_points, 10_000);
            assert_eq!(scenario.runs[0].parallel_efficiency_basis_points, 10_000);
            for run in &scenario.runs {
                assert_eq!(run.observations_ns.len(), 2);
                assert!(run.p50_ns <= run.p95_ns);
                assert!(run.p95_ns <= run.p99_ns);
                assert_eq!(run.output_sha256, scenario.output_sha256);
            }
        }
        assert_ne!(
            report.scenarios[0].output_sha256,
            report.scenarios[1].output_sha256
        );
    }
}
