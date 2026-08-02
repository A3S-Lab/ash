use std::error::Error;
use std::io;
use std::time::Instant;

use ash_engine::{ComputePool, Engine, Parallelism, SessionConfig};
use ash_ops::{PortableOperations, RepeatedLineReduction, collapse_repeated_lines};
use ash_protocol::ason::{Atom, Cell, Document, Field, Key, Table, Value};
use ash_protocol::request::{
    Arguments, Budget, MAX_REQUEST_RECORDS, MAX_REQUEST_TOKENS, RefArgs, Request,
};
use ash_protocol::response::{
    RESULT_REDUCED, RESULT_RETAINED, ReferenceResult, ResultData, Status,
};

use super::{
    MAX_RESPONSE_BYTES, REQUEST_MILLIS, RuntimeConfig, ScenarioReport, require_stable_output,
    runtime_run, sha256_hex,
};

const ROWS_PER_FIXTURE_FILE: usize = 64;
const MAX_ROWS: usize = 32_768;
const MAX_STRUCTURED_BYTES: usize = 8 * 1024 * 1024;
const SOURCE_COLUMNS: [&str; 8] = ["i", "p", "s", "c", "k", "m", "h", "x"];
const PROJECTED_COLUMNS: [&str; 6] = ["i", "p", "s", "c", "m", "h"];
const PROJECTED_INDEXES: [usize; 6] = [0, 1, 2, 3, 5, 6];
const REPEATED_LINES_PER_FIXTURE_FILE: usize = 512;
const REPEATED_RUN_LINES: usize = 512;
const MAX_REPEATED_LINES: usize = 262_144;

struct ProjectionWorkload {
    source: Vec<u8>,
    request: Request,
    rows: usize,
    expected_rows: Vec<Vec<Cell>>,
    input_sha256: String,
}

struct RepeatedLineWorkload {
    input: String,
    expected: String,
    lines: usize,
    collapsed_runs: usize,
    omitted_lines: usize,
    input_sha256: String,
}

pub(super) async fn measure_structured_projection_scenario(
    operations: &PortableOperations,
    workspace: &str,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let workload = ProjectionWorkload::new(config.files)?;
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let engine = Engine::new(parallelism)?;
        let (warm_output, _) = execute_once(
            &workload,
            &engine,
            operations,
            workspace,
            parallelism,
            50_000,
        )
        .await?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for sample in 0..config.samples {
            let session_id = u64::try_from(sample)?.saturating_add(50_001);
            let (output, elapsed) = execute_once(
                &workload,
                &engine,
                operations,
                workspace,
                parallelism,
                session_id,
            )
            .await?;
            require_stable_output(&mut expected_output, &output)?;
            observations.push(elapsed);
        }

        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("structured projection emitted no output"))?;
        runs.push(runtime_run(
            parallelism,
            observations,
            &mut baseline,
            u128::try_from(workload.rows)?,
            u128::try_from(workload.source.len())?,
            output,
        ));
    }

    let output =
        expected_output.ok_or_else(|| io::Error::other("missing structured projection output"))?;
    Ok(ScenarioReport {
        id: "ref-project-structured",
        work_items: workload.rows,
        work_bytes: u64::try_from(workload.source.len())?,
        input_sha256: workload.input_sha256,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

pub(super) fn measure_repeated_line_scenario(
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let workload = RepeatedLineWorkload::new(config.files)?;
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let pool = ComputePool::new(parallelism)?;
        let (warm_output, _) = execute_repeated_line_once(&pool, &workload)?;
        require_stable_output(&mut expected_output, &warm_output)?;

        let mut observations = Vec::with_capacity(config.samples);
        for _ in 0..config.samples {
            let (output, elapsed) = execute_repeated_line_once(&pool, &workload)?;
            require_stable_output(&mut expected_output, &output)?;
            observations.push(elapsed);
        }

        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("repeated-line reduction emitted no output"))?;
        runs.push(runtime_run(
            parallelism,
            observations,
            &mut baseline,
            u128::try_from(workload.lines)?,
            u128::try_from(workload.input.len())?,
            output,
        ));
    }

    let output = expected_output
        .ok_or_else(|| io::Error::other("missing repeated-line reduction output"))?;
    Ok(ScenarioReport {
        id: "reduce-repeated-lines",
        work_items: workload.lines,
        work_bytes: u64::try_from(workload.input.len())?,
        input_sha256: workload.input_sha256,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

impl ProjectionWorkload {
    fn new(fixture_files: usize) -> Result<Self, Box<dyn Error>> {
        let rows = fixture_files
            .checked_mul(ROWS_PER_FIXTURE_FILE)
            .ok_or_else(|| io::Error::other("structured projection row count overflow"))?
            .clamp(1, MAX_ROWS);
        let source = structured_source(rows)?;
        if source.len() > MAX_STRUCTURED_BYTES {
            return Err(
                io::Error::other("structured projection fixture exceeds decoder ceiling").into(),
            );
        }
        let request = Request::new(
            1,
            Arguments::Ref(RefArgs::project(
                1,
                "d",
                0,
                u64::try_from(rows)?,
                PROJECTED_COLUMNS.iter().map(ToString::to_string).collect(),
            )?),
            Budget::new(MAX_REQUEST_TOKENS, MAX_REQUEST_RECORDS, REQUEST_MILLIS)?,
        )?;
        let request_bytes = request.encode()?.encode().into_bytes();
        let mut input = b"ref-project-structured-v1".to_vec();
        append_evidence(&mut input, &source)?;
        append_evidence(&mut input, &request_bytes)?;
        Ok(Self {
            source,
            request,
            rows,
            expected_rows: (0..rows).map(projected_row).collect(),
            input_sha256: sha256_hex(&input),
        })
    }
}

impl RepeatedLineWorkload {
    fn new(fixture_files: usize) -> Result<Self, Box<dyn Error>> {
        let lines = fixture_files
            .checked_mul(REPEATED_LINES_PER_FIXTURE_FILE)
            .ok_or_else(|| io::Error::other("repeated-line fixture count overflow"))?
            .clamp(1, MAX_REPEATED_LINES);
        let groups = lines.div_ceil(REPEATED_RUN_LINES);
        let mut input = String::new();
        let mut expected = String::new();
        let mut remaining = lines;
        for group in 0..groups {
            let count = remaining.min(REPEATED_RUN_LINES);
            let line = repeated_line(group);
            for _ in 0..count {
                input.push_str(&line);
            }
            expected.push_str(&line);
            if count > 1 {
                expected.push_str(&format!("×{count}\n"));
            }
            remaining -= count;
        }
        let collapsed_runs = groups - usize::from(lines % REPEATED_RUN_LINES == 1);
        let omitted_lines = lines - groups;
        let mut evidence = b"reduce-repeated-lines-v1".to_vec();
        append_evidence(&mut evidence, input.as_bytes())?;
        Ok(Self {
            input,
            expected,
            lines,
            collapsed_runs,
            omitted_lines,
            input_sha256: sha256_hex(&evidence),
        })
    }
}

fn execute_repeated_line_once(
    pool: &ComputePool,
    workload: &RepeatedLineWorkload,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let started = Instant::now();
    let reduction = pool.install(|| collapse_repeated_lines(&workload.input));
    let elapsed = started.elapsed().as_nanos().max(1);
    validate_repeated_line_reduction(&reduction, workload)?;
    Ok((reduction_evidence(&reduction)?, elapsed))
}

fn validate_repeated_line_reduction(
    reduction: &RepeatedLineReduction,
    workload: &RepeatedLineWorkload,
) -> Result<(), io::Error> {
    if reduction.text() != workload.expected
        || reduction.collapsed_runs() != workload.collapsed_runs
        || reduction.omitted_lines() != workload.omitted_lines
    {
        return Err(io::Error::other(
            "repeated-line reduction changed run counts or stable output",
        ));
    }
    Ok(())
}

fn reduction_evidence(reduction: &RepeatedLineReduction) -> Result<Vec<u8>, io::Error> {
    let mut output = b"reduce-repeated-lines-output-v1".to_vec();
    output.extend_from_slice(
        &u64::try_from(reduction.collapsed_runs())
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    output.extend_from_slice(
        &u64::try_from(reduction.omitted_lines())
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    append_evidence(&mut output, reduction.text().as_bytes())?;
    Ok(output)
}

fn repeated_line(group: usize) -> String {
    let seed = u64::try_from(group)
        .unwrap_or(u64::MAX)
        .wrapping_mul(0x517c_c1b7_2722_0a95);
    format!(
        "diagnostic-{group:06} path=src/d{:02}/f{group:06}.rs code=E{:03} payload={seed:016x}\n",
        group % 64,
        group % 256,
    )
}

async fn execute_once(
    workload: &ProjectionWorkload,
    engine: &Engine,
    operations: &PortableOperations,
    workspace: &str,
    parallelism: Parallelism,
    session_id: u64,
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let session = engine.open_session(SessionConfig::new(
        session_id,
        workspace,
        MAX_RESPONSE_BYTES,
        parallelism,
    ))?;
    let reference = session.store().retain(workload.source.clone())?;
    if reference != 1 {
        return Err(
            io::Error::other("structured projection source alias was not canonical").into(),
        );
    }

    let started = Instant::now();
    let program = session.begin(&workload.request).await?;
    let response = operations.execute(&workload.request, &program).await?;
    let output = response.encode()?.encode().into_bytes();
    let elapsed = started.elapsed().as_nanos().max(1);
    validate_response(&response, workload, reference)?;
    drop(program);
    drop(session);
    Ok((output, elapsed))
}

fn validate_response(
    response: &ash_protocol::response::FinalResponse,
    workload: &ProjectionWorkload,
    reference: u64,
) -> Result<(), io::Error> {
    if response.status() != Status::Success
        || response.flags() != RESULT_REDUCED | RESULT_RETAINED
        || response.reference() != Some(reference)
    {
        return Err(io::Error::other(
            "structured projection returned unexpected status metadata",
        ));
    }
    let Some(ResultData::Reference(ReferenceResult::Projection(table))) = response.data() else {
        return Err(io::Error::other(
            "structured projection returned an unexpected result type",
        ));
    };
    if table.rows() != workload.expected_rows.as_slice()
        || !table
            .columns()
            .iter()
            .map(Key::as_str)
            .eq(PROJECTED_COLUMNS)
    {
        return Err(io::Error::other(
            "structured projection changed row order or selected values",
        ));
    }
    Ok(())
}

fn structured_source(rows: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let columns = SOURCE_COLUMNS
        .iter()
        .map(|column| Key::new(*column))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = (0..rows).map(structured_row).collect();
    let document = Document::new(vec![Field::new(
        Key::new("d")?,
        Value::Table(Table::new(columns, rows)?),
    )])?;
    Ok(document.encode().into_bytes())
}

fn structured_row(index: usize) -> Vec<Cell> {
    let seed = u64::try_from(index)
        .unwrap_or(u64::MAX)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    [
        index.to_string(),
        format!("src/d{:02}/f{index:06}.rs", index % 64),
        if index.is_multiple_of(11) {
            "err"
        } else {
            "ok"
        }
        .to_owned(),
        (index % 256).to_string(),
        ["rustc", "cargo", "test", "lint"][index % 4].to_owned(),
        format!("diag-{index:06}"),
        format!("{seed:016x}{:016x}", seed.rotate_left(17)),
        format!("{:016x}{:016x}", seed.rotate_left(31), seed.reverse_bits()),
    ]
    .into_iter()
    .map(|value| Cell::Atom(Atom::text(value)))
    .collect()
}

fn projected_row(index: usize) -> Vec<Cell> {
    let row = structured_row(index);
    PROJECTED_INDEXES
        .iter()
        .map(|index| row[*index].clone())
        .collect()
}

fn append_evidence(output: &mut Vec<u8>, evidence: &[u8]) -> Result<(), io::Error> {
    output.extend_from_slice(
        &u64::try_from(evidence.len())
            .map_err(io::Error::other)?
            .to_le_bytes(),
    );
    output.extend_from_slice(evidence);
    Ok(())
}
