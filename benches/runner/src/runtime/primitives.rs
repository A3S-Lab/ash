use std::error::Error;
use std::io;
use std::time::Instant;

use ash_engine::{DagCompletion, DagNode, DagOutcome, execute_dag};
use ash_store::PathDictionary;

use super::{
    RuntimeConfig, RuntimeRun, ScenarioReport, percentile, require_stable_output, sha256_hex,
    throughput,
};

const PATH_DICTIONARY_ENTRIES: usize = 1_024;
const PATH_DICTIONARY_LOOKUPS: usize = 4_096;
const DAG_WIDTH: usize = 8;

pub(super) const DAG_SCENARIOS: [(usize, &str); 3] = [
    (64, "dag-schedule-64"),
    (256, "dag-schedule-256"),
    (1_024, "dag-schedule-1024"),
];

pub(super) fn measure_path_dictionary_scenario(
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let population = (0..PATH_DICTIONARY_ENTRIES)
        .map(|index| format!("src/d{:03}/f{index:06}.rs", index % 64))
        .collect::<Vec<_>>();
    let lookups = (0..PATH_DICTIONARY_LOOKUPS)
        .map(|position| population[(position * 37) % population.len()].clone())
        .collect::<Vec<_>>();
    let dictionary = PathDictionary::new(population.len())?;
    let initial = dictionary.intern(&population)?;
    if initial.introduced.len() != population.len()
        || !initial
            .ids
            .iter()
            .copied()
            .eq(1..=u64::try_from(population.len())?)
    {
        return Err(io::Error::other("path dictionary population was not canonical").into());
    }

    let mut input = b"path-dictionary-hot-v1".to_vec();
    append_path_sequence(&mut input, &population)?;
    append_path_sequence(&mut input, &lookups)?;
    let work_bytes = lookups.iter().try_fold(0_u64, |total, path| {
        total
            .checked_add(u64::try_from(path.len()).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("path lookup byte count overflow"))
    })?;

    let (warm_output, _) = execute_path_dictionary_once(&dictionary, &lookups)?;
    let mut expected_output = None;
    require_stable_output(&mut expected_output, &warm_output)?;
    let mut observations = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let (output, elapsed) = execute_path_dictionary_once(&dictionary, &lookups)?;
        require_stable_output(&mut expected_output, &output)?;
        observations.push(elapsed);
    }
    if dictionary.len()? != population.len() {
        return Err(io::Error::other("hot path lookup changed dictionary size").into());
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing path lookup output"))?;
    Ok(ScenarioReport {
        id: "path-dictionary-hot",
        work_items: lookups.len(),
        work_bytes,
        input_sha256: sha256_hex(&input),
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs: vec![single_caller_run(
            observations,
            u128::try_from(lookups.len())?,
            u128::from(work_bytes),
            &output,
        )],
    })
}

fn execute_path_dictionary_once(
    dictionary: &PathDictionary,
    lookups: &[String],
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let started = Instant::now();
    let result = dictionary.intern(lookups)?;
    let elapsed = started.elapsed().as_nanos().max(1);
    if !result.introduced.is_empty() {
        return Err(io::Error::other("hot path lookup introduced a new mapping").into());
    }
    Ok((encode_u64s(&result.ids), elapsed))
}

fn append_path_sequence(output: &mut Vec<u8>, paths: &[String]) -> Result<(), io::Error> {
    output.extend_from_slice(
        &u64::try_from(paths.len())
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    for path in paths {
        output.extend_from_slice(
            &u64::try_from(path.len())
                .map_err(io::Error::other)?
                .to_le_bytes(),
        );
        output.extend_from_slice(path.as_bytes());
    }
    Ok(())
}

pub(super) async fn measure_dag_scenario(
    node_count: usize,
    id: &'static str,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let input = dag_input_bytes(node_count)?;
    let work_bytes = u64::try_from(input.len())?;
    let (warm_output, _) = execute_dag_once(node_count).await?;
    let mut expected_output = None;
    require_stable_output(&mut expected_output, &warm_output)?;
    let mut observations = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let (output, elapsed) = execute_dag_once(node_count).await?;
        require_stable_output(&mut expected_output, &output)?;
        observations.push(elapsed);
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing DAG output"))?;
    Ok(ScenarioReport {
        id,
        work_items: node_count,
        work_bytes,
        input_sha256: sha256_hex(&input),
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs: vec![single_caller_run(
            observations,
            u128::try_from(node_count)?,
            u128::from(work_bytes),
            &output,
        )],
    })
}

async fn execute_dag_once(node_count: usize) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let nodes = (0..node_count)
        .map(|index| {
            let dependencies = if index < DAG_WIDTH {
                Vec::new()
            } else {
                vec![index - DAG_WIDTH]
            };
            DagNode::new(dependencies, index)
        })
        .collect();
    let started = Instant::now();
    let outcomes = execute_dag(nodes, |index| async move {
        Ok::<_, io::Error>(DagCompletion::new(index, true))
    })
    .await
    .map_err(|error| io::Error::other(error.to_string()))?;
    let elapsed = started.elapsed().as_nanos().max(1);
    let mut values = Vec::with_capacity(outcomes.len());
    for (expected, outcome) in outcomes.into_iter().enumerate() {
        match outcome {
            DagOutcome::Completed(actual) if actual == expected => {
                values.push(u64::try_from(actual)?)
            }
            DagOutcome::Completed(_) | DagOutcome::Skipped => {
                return Err(io::Error::other("DAG output order or completion changed").into());
            }
        }
    }
    Ok((encode_u64s(&values), elapsed))
}

fn dag_input_bytes(node_count: usize) -> Result<Vec<u8>, io::Error> {
    let mut input = b"dag-schedule-v1".to_vec();
    input.extend_from_slice(
        &u64::try_from(node_count)
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    input.extend_from_slice(
        &u64::try_from(DAG_WIDTH)
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    for index in 0..node_count {
        input.extend_from_slice(
            &u64::try_from(index)
                .map_err(io::Error::other)?
                .to_le_bytes(),
        );
        if index < DAG_WIDTH {
            input.extend_from_slice(&0_u64.to_le_bytes());
        } else {
            input.extend_from_slice(&1_u64.to_le_bytes());
            input.extend_from_slice(
                &u64::try_from(index - DAG_WIDTH)
                    .map_err(io::Error::other)?
                    .to_le_bytes(),
            );
        }
    }
    Ok(input)
}

fn encode_u64s(values: &[u64]) -> Vec<u8> {
    let mut output = Vec::with_capacity(values.len().saturating_mul(std::mem::size_of::<u64>()));
    for value in values {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output
}

fn single_caller_run(
    observations: Vec<u128>,
    work_items: u128,
    work_bytes: u128,
    output: &[u8],
) -> RuntimeRun {
    let mut ordered = observations.clone();
    ordered.sort_unstable();
    let p50 = percentile(&ordered, 50);
    let p95 = percentile(&ordered, 95);
    let p99 = percentile(&ordered, 99);
    RuntimeRun {
        compute_workers: 1,
        io_workers: 1,
        observations_ns: observations,
        p50_ns: p50,
        p95_ns: p95,
        p99_ns: p99,
        items_per_second: throughput(work_items, p50),
        bytes_per_second: throughput(work_bytes, p50),
        speedup_basis_points: None,
        parallel_efficiency_basis_points: None,
        output_sha256: sha256_hex(output),
    }
}
