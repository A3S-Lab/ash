use std::error::Error;
use std::hint::black_box;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ash_engine::{ComputePool, Engine, Parallelism, ParallelismError, SessionConfig};
use ash_protocol::request::Request;
use ash_store::{CapturedContent, StoreLimits, StoreResidency};
use serde::Serialize;
use tokio::task::JoinHandle;

use super::{
    MAX_RESPONSE_BYTES, RuntimeConfig, RuntimeRun, STORE_CHUNK_BYTES, Scenario, ScenarioReport,
    fixture_bytes, percentile, ratio_basis_points, require_stable_output, sha256_hex, throughput,
};

const IDLE_SCENARIO_ID: &str = "io-spill-idle-compute";
const SATURATED_SCENARIO_ID: &str = "io-spill-saturated-compute";
const COMPUTE_LOAD: &str = "xorshift64-busy-loop";
const COMPUTE_BLOCK_ITERATIONS: u64 = 4_096;
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
pub(super) struct MixedIoReport {
    pub(super) idle_scenario_id: &'static str,
    pub(super) saturated_scenario_id: &'static str,
    pub(super) capture_bytes: usize,
    pub(super) fetched_tail_bytes: usize,
    pub(super) producer_chunk_bytes: usize,
    pub(super) memory_ceiling_bytes: usize,
    pub(super) compute_load: &'static str,
    pub(super) compute_block_iterations: u64,
    pub(super) saturation_proof: &'static str,
    pub(super) sample_order: &'static str,
    pub(super) ready_timeout_millis: u128,
    pub(super) capture_timeout_millis: u128,
    pub(super) stop_timeout_millis: u128,
    pub(super) comparisons: Vec<MixedIoComparison>,
}

#[derive(Debug, Serialize)]
pub(super) struct MixedIoComparison {
    pub(super) compute_workers: usize,
    pub(super) io_workers: usize,
    pub(super) idle_p50_ns: u128,
    pub(super) saturated_p50_ns: u128,
    pub(super) saturated_vs_idle_basis_points: u128,
}

pub(super) struct MixedIoMeasurement {
    pub(super) report: MixedIoReport,
    pub(super) scenarios: [ScenarioReport; 2],
}

struct MixedIoWorkload {
    input: Vec<u8>,
    fetch_bytes: usize,
}

#[derive(Clone, Copy)]
enum ComputeLoad {
    Idle,
    Saturated,
}

struct ComputeSaturation {
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    workers: usize,
    handles: Vec<JoinHandle<Result<u64, ParallelismError>>>,
}

impl ComputeSaturation {
    async fn start(pool: Arc<ComputePool>) -> Result<Self, Box<dyn Error>> {
        let workers = pool.workers().get();
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(workers);
        for lane in 0..workers {
            let pool = Arc::clone(&pool);
            let lane_stop = Arc::clone(&stop);
            let lane_ready = Arc::clone(&ready);
            let lane_active = Arc::clone(&active);
            handles.push(tokio::spawn(async move {
                pool.run(move || burn_compute_lane(lane, lane_stop, lane_ready, lane_active))
                    .await
            }));
        }
        let saturation = Self {
            stop,
            active,
            workers,
            handles,
        };
        let ready_result = tokio::time::timeout(READY_TIMEOUT, async {
            while ready.load(Ordering::SeqCst) != workers {
                tokio::task::yield_now().await;
            }
        })
        .await;
        if ready_result.is_err() {
            saturation.stop().await?;
            return Err(io::Error::other("compute saturation did not occupy every worker").into());
        }
        let active = saturation.active_workers();
        if active != workers {
            saturation.stop().await?;
            return Err(io::Error::other(format!(
                "compute saturation started {active} of {workers} workers"
            ))
            .into());
        }
        Ok(saturation)
    }

    fn active_workers(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    async fn stop(mut self) -> Result<(), Box<dyn Error>> {
        self.stop.store(true, Ordering::SeqCst);
        let handles = std::mem::take(&mut self.handles);
        let iterations = tokio::time::timeout(STOP_TIMEOUT, async move {
            let mut iterations = Vec::with_capacity(handles.len());
            for handle in handles {
                let joined = handle.await.map_err(|error| {
                    io::Error::other(format!("compute lane join failed: {error}"))
                })?;
                iterations.push(joined.map_err(|error| {
                    io::Error::other(format!("compute lane execution failed: {error}"))
                })?);
            }
            Ok::<_, io::Error>(iterations)
        })
        .await
        .map_err(|_| io::Error::other("compute saturation did not stop within its bound"))??;
        if iterations.len() != self.workers
            || iterations
                .iter()
                .any(|iterations| *iterations < COMPUTE_BLOCK_ITERATIONS)
        {
            return Err(
                io::Error::other("compute saturation returned incomplete lane evidence").into(),
            );
        }
        if self.active_workers() != 0 {
            return Err(io::Error::other("compute saturation retained an active worker").into());
        }
        Ok(())
    }
}

impl Drop for ComputeSaturation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn burn_compute_lane(
    lane: usize,
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
) -> u64 {
    active.fetch_add(1, Ordering::SeqCst);
    let mut state = (lane as u64)
        .wrapping_add(1)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut iterations = 0_u64;
    loop {
        for _ in 0..COMPUTE_BLOCK_ITERATIONS {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        }
        black_box(state);
        iterations = iterations.saturating_add(COMPUTE_BLOCK_ITERATIONS);
        if iterations == COMPUTE_BLOCK_ITERATIONS {
            ready.fetch_add(1, Ordering::SeqCst);
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
    }
    active.fetch_sub(1, Ordering::SeqCst);
    iterations
}

pub(super) async fn measure(
    workspace: &str,
    config: &RuntimeConfig,
) -> Result<MixedIoMeasurement, Box<dyn Error>> {
    let request = Scenario::SearchLiteral.request(1)?;
    let workload = MixedIoWorkload {
        input: fixture_bytes(0x6d1e, config.store_bytes),
        fetch_bytes: config.store_fetch_bytes,
    };
    let mut idle_runs = Vec::with_capacity(config.worker_counts.len());
    let mut saturated_runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        for (session_id, load) in [
            (40_000, ComputeLoad::Idle),
            (40_001, ComputeLoad::Saturated),
        ] {
            let (output, _) = execute_once(
                &engine,
                workspace,
                parallelism,
                &request,
                session_id,
                &workload,
                load,
            )
            .await?;
            require_stable_output(&mut expected_output, &output)?;
        }

        let mut idle_observations = Vec::with_capacity(config.samples);
        let mut saturated_observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = 40_002_u64.saturating_add(u64::try_from(sample)?.saturating_mul(2));
            let loads = if sample % 2 == 0 {
                [ComputeLoad::Idle, ComputeLoad::Saturated]
            } else {
                [ComputeLoad::Saturated, ComputeLoad::Idle]
            };
            for (offset, load) in loads.into_iter().enumerate() {
                let (output, elapsed) = execute_once(
                    &engine,
                    workspace,
                    parallelism,
                    &request,
                    session_id.saturating_add(u64::try_from(offset)?),
                    &workload,
                    load,
                )
                .await?;
                require_stable_output(&mut expected_output, &output)?;
                match load {
                    ComputeLoad::Idle => idle_observations.push(elapsed),
                    ComputeLoad::Saturated => saturated_observations.push(elapsed),
                }
            }
        }
        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("mixed I/O benchmark emitted no output"))?;
        idle_runs.push(runtime_run(
            parallelism,
            idle_observations,
            workload.input.len(),
            output,
        ));
        saturated_runs.push(runtime_run(
            parallelism,
            saturated_observations,
            workload.input.len(),
            output,
        ));
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing mixed I/O output"))?;
    let work_bytes = u64::try_from(workload.input.len())?;
    let input_sha256 = sha256_hex(&workload.input);
    let output_sha256 = sha256_hex(&output);
    let comparisons = idle_runs
        .iter()
        .zip(&saturated_runs)
        .map(|(idle, saturated)| {
            if idle.compute_workers != saturated.compute_workers
                || idle.io_workers != saturated.io_workers
            {
                return Err(io::Error::other(
                    "mixed I/O paired runs used different worker configurations",
                ));
            }
            Ok(MixedIoComparison {
                compute_workers: idle.compute_workers,
                io_workers: idle.io_workers,
                idle_p50_ns: idle.p50_ns,
                saturated_p50_ns: saturated.p50_ns,
                saturated_vs_idle_basis_points: ratio_basis_points(saturated.p50_ns, idle.p50_ns),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let scenario = |id, runs| ScenarioReport {
        id,
        work_items: 1,
        work_bytes,
        input_sha256: input_sha256.clone(),
        output_bytes: output.len(),
        output_sha256: output_sha256.clone(),
        runs,
    };
    Ok(MixedIoMeasurement {
        report: MixedIoReport {
            idle_scenario_id: IDLE_SCENARIO_ID,
            saturated_scenario_id: SATURATED_SCENARIO_ID,
            capture_bytes: workload.input.len(),
            fetched_tail_bytes: workload.fetch_bytes,
            producer_chunk_bytes: STORE_CHUNK_BYTES,
            memory_ceiling_bytes: 0,
            compute_load: COMPUTE_LOAD,
            compute_block_iterations: COMPUTE_BLOCK_ITERATIONS,
            saturation_proof: "all-compute-workers-active-at-capture-finish",
            sample_order: "alternating-paired-order",
            ready_timeout_millis: READY_TIMEOUT.as_millis(),
            capture_timeout_millis: CAPTURE_TIMEOUT.as_millis(),
            stop_timeout_millis: STOP_TIMEOUT.as_millis(),
            comparisons,
        },
        scenarios: [
            scenario(IDLE_SCENARIO_ID, idle_runs),
            scenario(SATURATED_SCENARIO_ID, saturated_runs),
        ],
    })
}

async fn execute_once(
    engine: &Engine,
    workspace: &str,
    parallelism: Parallelism,
    request: &Request,
    session_id: u64,
    workload: &MixedIoWorkload,
    load: ComputeLoad,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let mut session_config =
        SessionConfig::new(session_id, workspace, MAX_RESPONSE_BYTES, parallelism);
    session_config.store_limits = StoreLimits {
        max_bytes: u64::try_from(workload.input.len())?,
        max_entries: 1,
    };
    let session = engine.open_session(session_config)?;
    let program = session.begin(request).await?;
    let store = program.store().clone();
    let saturation = match load {
        ComputeLoad::Idle => None,
        ComputeLoad::Saturated => {
            Some(ComputeSaturation::start(Arc::clone(program.compute_pool())).await?)
        }
    };
    let started = Instant::now();
    let capture_result = tokio::time::timeout(CAPTURE_TIMEOUT, async {
        let mut capture = store.capture(0);
        for chunk in workload.input.chunks(STORE_CHUNK_BYTES) {
            capture.append(chunk).await?;
        }
        let captured = capture.finish().await?;
        Ok::<_, ash_store::StoreError>((captured, started.elapsed().as_nanos().max(1)))
    })
    .await;
    let active_workers = saturation
        .as_ref()
        .map_or(0, ComputeSaturation::active_workers);
    if let Some(saturation) = saturation {
        saturation.stop().await?;
    }
    let (captured, elapsed) =
        capture_result.map_err(|_| io::Error::other("mixed I/O capture exceeded its bound"))??;
    if matches!(load, ComputeLoad::Saturated)
        && active_workers != parallelism.compute_workers().get()
    {
        return Err(io::Error::other(format!(
            "only {active_workers} compute workers remained active at I/O completion"
        ))
        .into());
    }
    validate_capture(&captured, workload.input.len())?;
    let commit_store = store.clone();
    let aliases = program
        .compute_pool()
        .run(move || commit_store.retain_captures(vec![captured]))
        .await??;
    let alias = *aliases
        .first()
        .ok_or_else(|| io::Error::other("mixed I/O capture returned no alias"))?;
    let lease = store.get(alias)?;
    let fetch_bytes = u64::try_from(workload.fetch_bytes)?;
    let output = lease
        .read_range(
            lease.len().saturating_sub(fetch_bytes),
            fetch_bytes,
            fetch_bytes,
        )
        .await?;
    let expected = &workload.input[workload.input.len() - workload.fetch_bytes..];
    if output != expected {
        return Err(io::Error::other("mixed I/O retained tail changed").into());
    }
    drop(lease);
    store.release(alias)?;
    drop(store);
    drop(program);
    drop(session);
    Ok((output, elapsed))
}

fn validate_capture(captured: &CapturedContent, expected_bytes: usize) -> Result<(), io::Error> {
    let expected_bytes =
        u64::try_from(expected_bytes).map_err(|_| io::Error::other("mixed I/O size overflow"))?;
    if captured.residency() != StoreResidency::Disk || captured.len() != expected_bytes {
        return Err(io::Error::other(
            "mixed I/O capture was not an exact disk-backed value",
        ));
    }
    Ok(())
}

fn runtime_run(
    parallelism: Parallelism,
    observations: Vec<u128>,
    work_bytes: usize,
    output: &[u8],
) -> RuntimeRun {
    let mut ordered = observations.clone();
    ordered.sort_unstable();
    let p50_ns = percentile(&ordered, 50);
    RuntimeRun {
        compute_workers: parallelism.compute_workers().get(),
        io_workers: parallelism.io_workers().get(),
        p50_ns,
        p95_ns: percentile(&ordered, 95),
        p99_ns: percentile(&ordered, 99),
        observations_ns: observations,
        items_per_second: throughput(1, p50_ns),
        bytes_per_second: throughput(work_bytes as u128, p50_ns),
        speedup_basis_points: None,
        parallel_efficiency_basis_points: None,
        output_sha256: sha256_hex(output),
    }
}
