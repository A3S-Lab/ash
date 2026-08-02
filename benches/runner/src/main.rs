#![forbid(unsafe_code)]

use std::error::Error;
use std::fs;
use std::path::Path;

use ash_ops::{collapse_repeated_blocks, collapse_repeated_lines, focus_error_output};
use ash_protocol::ason::{Atom, Cell, Document, Field, Key, Record, Table, Value, decode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use sha2::{Digest, Sha256};
use tiktoken_rs::{cl100k_base, o200k_base};

mod runtime;
mod tasks;

const CORPUS_BYTES: &[u8] = include_bytes!("../../corpus/v1.json");
const CORPUS_TEXT: &str = include_str!("../../corpus/v1.json");
const WORKSPACE_LOCK: &[u8] = include_bytes!("../../../Cargo.lock");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema: u8,
    datasets: Vec<Dataset>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dataset {
    id: String,
    kind: String,
    rows: usize,
    paths: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: u8,
    corpus: String,
    corpus_sha256: String,
    workspace_lock_sha256: String,
    tokenizers: [&'static str; 2],
    datasets: Vec<DatasetReport>,
    aggregate: EncodingSet,
    formula_algebra: FormulaAlgebraReport,
    repeated_line_reduction: RepeatedLineReport,
    repeated_block_reduction: RepeatedBlockReport,
    error_focus_reduction: ErrorFocusReport,
    gates: Gates,
}

#[derive(Debug, Serialize)]
struct RepeatedLineReport {
    source_lines: usize,
    projected_lines: usize,
    collapsed_runs: usize,
    omitted_lines: usize,
    source: Measurement,
    projection: Measurement,
    gates: ReductionGates,
}

#[derive(Debug, Serialize)]
struct RepeatedBlockReport {
    source_lines: usize,
    projected_lines: usize,
    collapsed_blocks: usize,
    omitted_repetitions: usize,
    omitted_lines: usize,
    source: Measurement,
    projection: Measurement,
    gates: ReductionGates,
}

#[derive(Debug, Serialize)]
struct ErrorFocusReport {
    source_lines: usize,
    projected_lines: usize,
    diagnostic_lines: usize,
    omitted_spans: usize,
    omitted_lines: usize,
    source: Measurement,
    projection: Measurement,
    gates: ReductionGates,
}

#[derive(Debug, Serialize)]
struct ReductionGates {
    projection_vs_source_bytes_percent: usize,
    projection_vs_source_cl100k_percent: usize,
    projection_vs_source_o200k_percent: usize,
    required_max_percent: usize,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct FormulaAlgebraReport {
    operators: Vec<FormulaOperatorReport>,
    candidates: FormulaCandidates,
    gates: FormulaGates,
}

#[derive(Debug, Serialize)]
struct FormulaOperatorReport {
    id: &'static str,
    symbol: &'static str,
    legacy_ascii_wrapper: Measurement,
    canonical_symbol: Measurement,
}

#[derive(Debug, Serialize)]
struct FormulaCandidates {
    legacy_ascii_wrapper: Measurement,
    direct_greek: Measurement,
    direct_ascii_letters: Measurement,
    canonical_symbols: Measurement,
}

#[derive(Debug, Serialize)]
struct FormulaGates {
    canonical_vs_legacy_bytes_percent: usize,
    canonical_vs_legacy_cl100k_percent: usize,
    canonical_vs_legacy_o200k_percent: usize,
    required_max_percent: usize,
    matches_direct_ascii_token_floor: bool,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct DatasetReport {
    id: String,
    rows: usize,
    encodings: EncodingSet,
}

#[derive(Clone, Debug, Default, Serialize)]
struct EncodingSet {
    ason: Measurement,
    json_records: Measurement,
    json_columns: Measurement,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
struct Measurement {
    bytes: usize,
    cl100k_tokens: usize,
    o200k_tokens: usize,
}

#[derive(Debug, Serialize)]
struct Gates {
    semantic_round_trip: bool,
    ason_vs_record_json_cl100k_percent: usize,
    ason_vs_record_json_o200k_percent: usize,
    required_max_percent: usize,
    passed: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if matches!(arguments.as_slice(), [command, ..] if command == "--runtime") {
        let ash_binary = match arguments.as_slice() {
            [_] => None,
            [_, path] => Some(std::path::Path::new(path)),
            _ => {
                return Err("usage: a3s-ash-bench --runtime [path-to-ash]".into());
            }
        };
        let mut encoded = serde_json::to_vec_pretty(&runtime::runtime_report(ash_binary).await?)?;
        encoded.push(b'\n');
        print!("{}", std::str::from_utf8(&encoded)?);
        return Ok(());
    }
    if matches!(arguments.as_slice(), [command] if command == "--tasks") {
        let mut encoded = serde_json::to_vec_pretty(&tasks::build_report().await?)?;
        encoded.push(b'\n');
        print!("{}", std::str::from_utf8(&encoded)?);
        return Ok(());
    }
    if matches!(arguments.as_slice(), [command, _] if command == "--validate-agent-trace") {
        let [_, path] = arguments.as_slice() else {
            unreachable!("agent trace validation arguments were matched above");
        };
        tasks::validate_agent_trace(Path::new(path))?;
        return Ok(());
    }
    if matches!(arguments.as_slice(), [command, _, allow] if command == "--agent-trace" && allow == "--allow-native-agent-exec")
    {
        let [_, path, _] = arguments.as_slice() else {
            unreachable!("agent trace replay arguments were matched above");
        };
        let mut encoded =
            serde_json::to_vec_pretty(&tasks::build_agent_report(Path::new(path)).await?)?;
        encoded.push(b'\n');
        print!("{}", std::str::from_utf8(&encoded)?);
        return Ok(());
    }
    if matches!(arguments.as_slice(), [command, _] if command == "--write-task-lock" || command == "--check-task-lock")
    {
        let encoded = tasks::encoded_lock().await?;
        match arguments.as_slice() {
            [command, path] if command == "--write-task-lock" => fs::write(path, encoded)?,
            [command, path] if command == "--check-task-lock" => {
                if fs::read(path)? != encoded {
                    return Err("checked-in task corpus lock is stale".into());
                }
            }
            _ => unreachable!("task lock arguments were matched above"),
        }
        return Ok(());
    }
    let report = build_report()?;
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    match arguments.as_slice() {
        [] => print!("{}", std::str::from_utf8(&encoded)?),
        [command, path] if command == "--write" => fs::write(path, encoded)?,
        [command, path] if command == "--check" => {
            if fs::read(path)? != encoded {
                return Err("checked-in benchmark evidence is stale".into());
            }
        }
        _ => {
            return Err(
                "usage: a3s-ash-bench [--runtime [path-to-ash]|--tasks|--validate-agent-trace <path>|--agent-trace <path> --allow-native-agent-exec|--write-task-lock <path>|--check-task-lock <path>|--write <path>|--check <path>]".into(),
            );
        }
    }
    Ok(())
}

fn build_report() -> Result<Report, Box<dyn Error>> {
    let corpus: Corpus = serde_json::from_str(CORPUS_TEXT)?;
    if corpus.schema != 1 || corpus.datasets.is_empty() {
        return Err("invalid benchmark corpus".into());
    }
    let cl100k = cl100k_base()?;
    let o200k = o200k_base()?;
    let formula_algebra = build_formula_algebra(&cl100k, &o200k)?;
    let repeated_line_reduction = build_repeated_line_report(&cl100k, &o200k)?;
    let repeated_block_reduction = build_repeated_block_report(&cl100k, &o200k)?;
    let error_focus_reduction = build_error_focus_report(&cl100k, &o200k)?;
    let mut datasets = Vec::with_capacity(corpus.datasets.len());
    let mut aggregate = EncodingSet::default();
    for dataset in corpus.datasets {
        let document = generate_document(&dataset)?;
        let ason = document.encode();
        if decode(&ason)? != document {
            return Err("ASON semantic round trip failed".into());
        }
        let records = json_records(&document);
        let columns = json_columns(&document);
        let mut records = serde_json::to_string(&records)?;
        records.push('\n');
        let mut columns = serde_json::to_string(&columns)?;
        columns.push('\n');
        let encodings = EncodingSet {
            ason: measure(&ason, &cl100k, &o200k),
            json_records: measure(&records, &cl100k, &o200k),
            json_columns: measure(&columns, &cl100k, &o200k),
        };
        add_measurements(&mut aggregate, &encodings);
        datasets.push(DatasetReport {
            id: dataset.id,
            rows: dataset.rows,
            encodings,
        });
    }
    let cl100k_percent = percentage(
        aggregate.ason.cl100k_tokens,
        aggregate.json_records.cl100k_tokens,
    );
    let o200k_percent = percentage(
        aggregate.ason.o200k_tokens,
        aggregate.json_records.o200k_tokens,
    );
    const REQUIRED_MAX_PERCENT: usize = 65;
    let passed = cl100k_percent <= REQUIRED_MAX_PERCENT
        && o200k_percent <= REQUIRED_MAX_PERCENT
        && aggregate.ason.bytes < aggregate.json_records.bytes;
    if !passed {
        return Err("ASON token-efficiency release gate failed".into());
    }
    Ok(Report {
        schema: 5,
        corpus: "benches/corpus/v1.json".to_owned(),
        corpus_sha256: hex(&Sha256::digest(CORPUS_BYTES)),
        workspace_lock_sha256: hex(&Sha256::digest(WORKSPACE_LOCK)),
        tokenizers: [
            "tiktoken-rs/0.12.0:cl100k_base",
            "tiktoken-rs/0.12.0:o200k_base",
        ],
        datasets,
        aggregate,
        formula_algebra,
        repeated_line_reduction,
        repeated_block_reduction,
        error_focus_reduction,
        gates: Gates {
            semantic_round_trip: true,
            ason_vs_record_json_cl100k_percent: cl100k_percent,
            ason_vs_record_json_o200k_percent: o200k_percent,
            required_max_percent: REQUIRED_MAX_PERCENT,
            passed,
        },
    })
}

fn build_repeated_line_report(
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Result<RepeatedLineReport, Box<dyn Error>> {
    const GROUPS: usize = 64;
    const RUN_LINES: usize = 128;
    const REQUIRED_MAX_PERCENT: usize = 5;

    let mut source = String::new();
    for group in 0..GROUPS {
        let line = format!(
            "error[E{:03}] path=src/component-{:02}/module-{group:02}.rs repeated diagnostic {group:02}\n",
            group % 32,
            group % 16,
        );
        for _ in 0..RUN_LINES {
            source.push_str(&line);
        }
    }
    let reduction = collapse_repeated_lines(&source);
    if reduction.collapsed_runs() != GROUPS || reduction.omitted_lines() != GROUPS * (RUN_LINES - 1)
    {
        return Err("repeated-line reducer produced unexpected evidence".into());
    }
    let source_measurement = measure(&source, cl100k, o200k);
    let projection = measure(reduction.text(), cl100k, o200k);
    let bytes_percent = percentage(projection.bytes, source_measurement.bytes);
    let cl100k_percent = percentage(projection.cl100k_tokens, source_measurement.cl100k_tokens);
    let o200k_percent = percentage(projection.o200k_tokens, source_measurement.o200k_tokens);
    let passed = bytes_percent <= REQUIRED_MAX_PERCENT
        && cl100k_percent <= REQUIRED_MAX_PERCENT
        && o200k_percent <= REQUIRED_MAX_PERCENT;
    if !passed {
        return Err("repeated-line token-efficiency gate failed".into());
    }
    Ok(RepeatedLineReport {
        source_lines: GROUPS * RUN_LINES,
        projected_lines: reduction.text().lines().count(),
        collapsed_runs: reduction.collapsed_runs(),
        omitted_lines: reduction.omitted_lines(),
        source: source_measurement,
        projection,
        gates: ReductionGates {
            projection_vs_source_bytes_percent: bytes_percent,
            projection_vs_source_cl100k_percent: cl100k_percent,
            projection_vs_source_o200k_percent: o200k_percent,
            required_max_percent: REQUIRED_MAX_PERCENT,
            passed,
        },
    })
}

fn build_repeated_block_report(
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Result<RepeatedBlockReport, Box<dyn Error>> {
    const GROUPS: usize = 32;
    const BLOCK_LINES: usize = 6;
    const REPETITIONS: usize = 64;
    const REQUIRED_MAX_PERCENT: usize = 5;

    let mut source = String::new();
    for group in 0..GROUPS {
        let mut block = String::new();
        for frame in 0..BLOCK_LINES {
            block.push_str(&format!(
                "error[E{:03}] path=src/component-{:02}/module-{group:02}.rs frame={frame:02} repeated block {group:02}\n",
                (group + frame) % 32,
                group % 16,
            ));
        }
        for _ in 0..REPETITIONS {
            source.push_str(&block);
        }
    }
    let reduction = collapse_repeated_blocks(&source);
    if reduction.collapsed_blocks() != GROUPS
        || reduction.omitted_repetitions() != GROUPS * (REPETITIONS - 1)
        || reduction.omitted_lines() != GROUPS * BLOCK_LINES * (REPETITIONS - 1)
    {
        return Err("repeated-block reducer produced unexpected evidence".into());
    }
    let source_measurement = measure(&source, cl100k, o200k);
    let projection = measure(reduction.text(), cl100k, o200k);
    let bytes_percent = percentage(projection.bytes, source_measurement.bytes);
    let cl100k_percent = percentage(projection.cl100k_tokens, source_measurement.cl100k_tokens);
    let o200k_percent = percentage(projection.o200k_tokens, source_measurement.o200k_tokens);
    let passed = bytes_percent <= REQUIRED_MAX_PERCENT
        && cl100k_percent <= REQUIRED_MAX_PERCENT
        && o200k_percent <= REQUIRED_MAX_PERCENT;
    if !passed {
        return Err("repeated-block token-efficiency gate failed".into());
    }
    Ok(RepeatedBlockReport {
        source_lines: GROUPS * BLOCK_LINES * REPETITIONS,
        projected_lines: reduction.text().lines().count(),
        collapsed_blocks: reduction.collapsed_blocks(),
        omitted_repetitions: reduction.omitted_repetitions(),
        omitted_lines: reduction.omitted_lines(),
        source: source_measurement,
        projection,
        gates: ReductionGates {
            projection_vs_source_bytes_percent: bytes_percent,
            projection_vs_source_cl100k_percent: cl100k_percent,
            projection_vs_source_o200k_percent: o200k_percent,
            required_max_percent: REQUIRED_MAX_PERCENT,
            passed,
        },
    })
}

fn build_error_focus_report(
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Result<ErrorFocusReport, Box<dyn Error>> {
    const GROUPS: usize = 32;
    const LINES_PER_GROUP: usize = 256;
    const ANCHOR_LINE: usize = 128;
    const REQUIRED_MAX_PERCENT: usize = 5;

    let mut source = String::new();
    for group in 0..GROUPS {
        for line in 0..LINES_PER_GROUP {
            if line == ANCHOR_LINE {
                source.push_str(&format!(
                    "error[E{:03}] path=src/component-{:02}/module-{group:02}.rs diagnostic focus {group:02}\n",
                    group % 32,
                    group % 16,
                ));
            } else {
                source.push_str(&format!(
                    "trace group={group:02} step={line:03} path=src/component-{:02}/module-{group:02}.rs payload={:08x}\n",
                    group % 16,
                    group * LINES_PER_GROUP + line,
                ));
            }
        }
    }
    let reduction = focus_error_output(&source);
    let source_lines = GROUPS * LINES_PER_GROUP;
    let retained_lines = 2 * 2 + GROUPS * (2 + 1 + 6);
    if reduction.diagnostic_lines() != GROUPS
        || reduction.omitted_spans() != GROUPS + 1
        || reduction.omitted_lines() != source_lines - retained_lines
    {
        return Err("error-focus reducer produced unexpected evidence".into());
    }
    let source_measurement = measure(&source, cl100k, o200k);
    let projection = measure(reduction.text(), cl100k, o200k);
    let bytes_percent = percentage(projection.bytes, source_measurement.bytes);
    let cl100k_percent = percentage(projection.cl100k_tokens, source_measurement.cl100k_tokens);
    let o200k_percent = percentage(projection.o200k_tokens, source_measurement.o200k_tokens);
    let passed = bytes_percent <= REQUIRED_MAX_PERCENT
        && cl100k_percent <= REQUIRED_MAX_PERCENT
        && o200k_percent <= REQUIRED_MAX_PERCENT;
    if !passed {
        return Err("error-focus token-efficiency gate failed".into());
    }
    Ok(ErrorFocusReport {
        source_lines,
        projected_lines: reduction.text().lines().count(),
        diagnostic_lines: reduction.diagnostic_lines(),
        omitted_spans: reduction.omitted_spans(),
        omitted_lines: reduction.omitted_lines(),
        source: source_measurement,
        projection,
        gates: ReductionGates {
            projection_vs_source_bytes_percent: bytes_percent,
            projection_vs_source_cl100k_percent: cl100k_percent,
            projection_vs_source_o200k_percent: o200k_percent,
            required_max_percent: REQUIRED_MAX_PERCENT,
            passed,
        },
    })
}

fn build_formula_algebra(
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Result<FormulaAlgebraReport, Box<dyn Error>> {
    struct FormulaCase {
        id: &'static str,
        symbol: &'static str,
        legacy: &'static str,
        greek: &'static str,
        ascii: &'static str,
        canonical: &'static str,
    }

    let cases = [
        FormulaCase {
            id: "bytes",
            symbol: "/",
            legacy: "o:h\na{b}:\n[@7,0,4096]\n",
            greek: "o:β\na:[@7,0,4096]\n",
            ascii: "o:b\na:[@7,0,4096]\n",
            canonical: "o:/\na:[@7,0,4096]\n",
        },
        FormulaCase {
            id: "lines",
            symbol: "#",
            legacy: "o:h\na{l}:\n[@7,2,32]\n",
            greek: "o:λ\na:[@7,2,32]\n",
            ascii: "o:l\na:[@7,2,32]\n",
            canonical: "o:#\na:[@7,2,32]\n",
        },
        FormulaCase {
            id: "search",
            symbol: "?",
            legacy: "o:h\na{g}:\n[@7,0,1048576,TODO,0]\n",
            greek: "o:σ\na:[@7,0,1048576,TODO,0]\n",
            ascii: "o:g\na:[@7,0,1048576,TODO,0]\n",
            canonical: "o:?\na:[@7,0,1048576,TODO,0]\n",
        },
        FormulaCase {
            id: "release",
            symbol: "-",
            legacy: "o:h\na{d}:\n[@7]\n",
            greek: "o:δ\na:[@7]\n",
            ascii: "o:d\na:[@7]\n",
            canonical: "o:-\na:[@7]\n",
        },
        FormulaCase {
            id: "project",
            symbol: "|",
            legacy: "o:h\na{p}:\n[@7,d,0,64,p,l,t]\n",
            greek: "o:π\na:[@7,d,0,64,p,l,t]\n",
            ascii: "o:p\na:[@7,d,0,64,p,l,t]\n",
            canonical: "o:|\na:[@7,d,0,64,p,l,t]\n",
        },
        FormulaCase {
            id: "materialize",
            symbol: ">",
            legacy: "o:h\na{w}:\n[@8,artifacts/out.bin]\n",
            greek: "o:μ\na:[@8,artifacts/out.bin]\n",
            ascii: "o:w\na:[@8,artifacts/out.bin]\n",
            canonical: "o:>\na:[@8,artifacts/out.bin]\n",
        },
    ];

    let mut legacy = String::new();
    let mut greek = String::new();
    let mut ascii = String::new();
    let mut canonical = String::new();
    let mut operators = Vec::with_capacity(cases.len());
    for case in cases {
        legacy.push_str(case.legacy);
        greek.push_str(case.greek);
        ascii.push_str(case.ascii);
        canonical.push_str(case.canonical);
        operators.push(FormulaOperatorReport {
            id: case.id,
            symbol: case.symbol,
            legacy_ascii_wrapper: measure(case.legacy, cl100k, o200k),
            canonical_symbol: measure(case.canonical, cl100k, o200k),
        });
    }
    let candidates = FormulaCandidates {
        legacy_ascii_wrapper: measure(&legacy, cl100k, o200k),
        direct_greek: measure(&greek, cl100k, o200k),
        direct_ascii_letters: measure(&ascii, cl100k, o200k),
        canonical_symbols: measure(&canonical, cl100k, o200k),
    };
    let bytes_percent = percentage(
        candidates.canonical_symbols.bytes,
        candidates.legacy_ascii_wrapper.bytes,
    );
    let cl100k_percent = percentage(
        candidates.canonical_symbols.cl100k_tokens,
        candidates.legacy_ascii_wrapper.cl100k_tokens,
    );
    let o200k_percent = percentage(
        candidates.canonical_symbols.o200k_tokens,
        candidates.legacy_ascii_wrapper.o200k_tokens,
    );
    const REQUIRED_MAX_PERCENT: usize = 85;
    let matches_direct_ascii_token_floor = candidates.canonical_symbols.cl100k_tokens
        == candidates.direct_ascii_letters.cl100k_tokens
        && candidates.canonical_symbols.o200k_tokens
            == candidates.direct_ascii_letters.o200k_tokens;
    let passed = bytes_percent <= REQUIRED_MAX_PERCENT
        && cl100k_percent <= REQUIRED_MAX_PERCENT
        && o200k_percent <= REQUIRED_MAX_PERCENT
        && matches_direct_ascii_token_floor
        && candidates.canonical_symbols.bytes < candidates.direct_greek.bytes
        && candidates.canonical_symbols.cl100k_tokens < candidates.direct_greek.cl100k_tokens
        && candidates.canonical_symbols.o200k_tokens < candidates.direct_greek.o200k_tokens;
    if !passed {
        return Err("symbol formula token-efficiency gate failed".into());
    }
    Ok(FormulaAlgebraReport {
        operators,
        candidates,
        gates: FormulaGates {
            canonical_vs_legacy_bytes_percent: bytes_percent,
            canonical_vs_legacy_cl100k_percent: cl100k_percent,
            canonical_vs_legacy_o200k_percent: o200k_percent,
            required_max_percent: REQUIRED_MAX_PERCENT,
            matches_direct_ascii_token_floor,
            passed,
        },
    })
}

fn generate_document(dataset: &Dataset) -> Result<Document, Box<dyn Error>> {
    if dataset.rows == 0 || dataset.rows > 10_000 || dataset.paths > 1_000 {
        return Err("dataset dimensions exceed benchmark limits".into());
    }
    if dataset.kind != "batch" && dataset.paths == 0 {
        return Err("path-oriented datasets require a path dictionary".into());
    }
    let mut fields = vec![field("s", Value::Scalar(Atom::text("0")))?];
    if dataset.paths > 0 {
        let paths = (0..dataset.paths)
            .map(|index| {
                vec![
                    Cell::Atom(Atom::text((index + 1).to_string())),
                    Cell::Atom(Atom::text(format!(
                        "src/component-{index:02}/module-{index:02}.rs"
                    ))),
                ]
            })
            .collect();
        fields.push(field(
            "p",
            Value::Table(Table::new(keys(&["i", "v"])?, paths)?),
        )?);
    }
    let (columns, rows) = match dataset.kind.as_str() {
        "search" => (
            vec!["p", "l", "c", "t"],
            (0..dataset.rows)
                .map(|index| {
                    vec![
                        reference(index, dataset.paths),
                        atom(index * 3 + 1),
                        atom(index % 17 + 1),
                        Cell::Atom(Atom::text(format!(
                            "symbol_{:03} matches repeated repository evidence",
                            index % 32
                        ))),
                    ]
                })
                .collect(),
        ),
        "diagnostic" => (
            vec!["p", "l", "q", "c", "m"],
            (0..dataset.rows)
                .map(|index| {
                    vec![
                        reference(index, dataset.paths),
                        atom(index + 10),
                        Cell::Atom(Atom::text(if index % 5 == 0 { "w" } else { "e" })),
                        Cell::Atom(Atom::text(format!("E{:04}", index % 19))),
                        Cell::Atom(Atom::text(format!(
                            "type mismatch at generic boundary {}: expected Result<T, E>",
                            index % 11
                        ))),
                    ]
                })
                .collect(),
        ),
        "tree" => (
            vec!["p", "k", "z", "h"],
            (0..dataset.rows)
                .map(|index| {
                    vec![
                        reference(index, dataset.paths),
                        Cell::Atom(Atom::text(if index % 7 == 0 { "d" } else { "f" })),
                        atom(index * 4093 + 17),
                        Cell::Atom(Atom::text(format!("{:016x}", index * 97 + 13))),
                    ]
                })
                .collect(),
        ),
        "batch" => (
            vec!["i", "o", "s", "c", "r"],
            (0..dataset.rows)
                .map(|index| {
                    vec![
                        atom(index + 1),
                        Cell::Atom(Atom::text(match index % 4 {
                            0 => "r",
                            1 => "g",
                            2 => "x",
                            _ => "p",
                        })),
                        atom(0),
                        atom(index * 5),
                        Cell::Atom(Atom::reference((index + 1) as u64)),
                    ]
                })
                .collect(),
        ),
        _ => return Err(format!("unknown dataset kind: {}", dataset.kind).into()),
    };
    fields.push(field(
        "d",
        Value::Table(Table::new(keys(&columns)?, rows)?),
    )?);
    Ok(Document::new(fields)?)
}

fn field(name: &str, value: Value) -> Result<Field, ash_protocol::ason::BuildError> {
    Ok(Field::new(Key::new(name)?, value))
}

fn keys(names: &[&str]) -> Result<Vec<Key>, ash_protocol::ason::BuildError> {
    names.iter().map(|name| Key::new(*name)).collect()
}

fn atom(value: usize) -> Cell {
    Cell::Atom(Atom::text(value.to_string()))
}

fn reference(index: usize, paths: usize) -> Cell {
    Cell::Atom(Atom::reference((index % paths + 1) as u64))
}

fn json_records(document: &Document) -> JsonValue {
    let mut root = Map::new();
    for field in document.fields() {
        let value = match field.value() {
            Value::Scalar(atom) => atom_json(atom),
            Value::Vector(atoms) => JsonValue::Array(atoms.iter().map(atom_json).collect()),
            Value::Record(record) => record_json(record),
            Value::Table(table) => JsonValue::Array(
                table
                    .rows()
                    .iter()
                    .map(|row| row_json(table.columns(), row))
                    .collect(),
            ),
        };
        root.insert(field.key().as_str().to_owned(), value);
    }
    JsonValue::Object(root)
}

fn json_columns(document: &Document) -> JsonValue {
    let mut root = Map::new();
    for field in document.fields() {
        let value = match field.value() {
            Value::Scalar(atom) => atom_json(atom),
            Value::Vector(atoms) => JsonValue::Array(atoms.iter().map(atom_json).collect()),
            Value::Record(record) => column_json(record.columns(), &[record.values().to_vec()]),
            Value::Table(table) => column_json(table.columns(), table.rows()),
        };
        root.insert(field.key().as_str().to_owned(), value);
    }
    JsonValue::Object(root)
}

fn record_json(record: &Record) -> JsonValue {
    row_json(record.columns(), record.values())
}

fn row_json(columns: &[Key], row: &[Cell]) -> JsonValue {
    JsonValue::Object(
        columns
            .iter()
            .zip(row)
            .map(|(key, cell)| (key.as_str().to_owned(), cell_json(cell)))
            .collect(),
    )
}

fn column_json(columns: &[Key], rows: &[Vec<Cell>]) -> JsonValue {
    JsonValue::Object(
        [
            (
                "c".to_owned(),
                JsonValue::Array(
                    columns
                        .iter()
                        .map(|key| JsonValue::String(key.as_str().to_owned()))
                        .collect(),
                ),
            ),
            (
                "r".to_owned(),
                JsonValue::Array(
                    rows.iter()
                        .map(|row| JsonValue::Array(row.iter().map(cell_json).collect()))
                        .collect(),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    )
}

fn cell_json(cell: &Cell) -> JsonValue {
    match cell {
        Cell::Atom(atom) => atom_json(atom),
        Cell::Vector(atoms) => JsonValue::Array(atoms.iter().map(atom_json).collect()),
    }
}

fn atom_json(atom: &Atom) -> JsonValue {
    match atom {
        Atom::Null => JsonValue::Null,
        Atom::Reference(reference) => JsonValue::String(format!("@{reference}")),
        Atom::Text(text) => JsonValue::String(text.clone()),
    }
}

fn measure(
    value: &str,
    cl100k: &tiktoken_rs::CoreBPE,
    o200k: &tiktoken_rs::CoreBPE,
) -> Measurement {
    Measurement {
        bytes: value.len(),
        cl100k_tokens: cl100k.encode_with_special_tokens(value).len(),
        o200k_tokens: o200k.encode_with_special_tokens(value).len(),
    }
}

fn add_measurements(total: &mut EncodingSet, value: &EncodingSet) {
    add_measurement(&mut total.ason, &value.ason);
    add_measurement(&mut total.json_records, &value.json_records);
    add_measurement(&mut total.json_columns, &value.json_columns);
}

fn add_measurement(total: &mut Measurement, value: &Measurement) {
    total.bytes += value.bytes;
    total.cl100k_tokens += value.cl100k_tokens;
    total.o200k_tokens += value.o200k_tokens;
}

fn percentage(value: usize, baseline: usize) -> usize {
    if baseline == 0 {
        usize::MAX
    } else {
        value.saturating_mul(100).div_ceil(baseline)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{build_formula_algebra, build_report, measure, tasks};

    #[test]
    fn corpus_round_trips_and_passes_the_token_gate() {
        let report = build_report().expect("benchmark report");
        assert_eq!(report.schema, 5);
        assert!(report.gates.semantic_round_trip);
        assert!(report.gates.passed);
        assert!(report.repeated_line_reduction.gates.passed);
        assert_eq!(report.repeated_line_reduction.source_lines, 8_192);
        assert_eq!(report.repeated_line_reduction.projected_lines, 128);
        assert!(report.repeated_block_reduction.gates.passed);
        assert_eq!(report.repeated_block_reduction.source_lines, 12_288);
        assert_eq!(report.repeated_block_reduction.projected_lines, 224);
        assert!(report.error_focus_reduction.gates.passed);
        assert_eq!(report.error_focus_reduction.source_lines, 8_192);
        assert_eq!(report.error_focus_reduction.projected_lines, 325);
        assert_eq!(report.datasets.len(), 4);
        assert!(report.aggregate.ason.bytes < report.aggregate.json_records.bytes);
    }

    #[test]
    fn reference_formulas_beat_the_sparse_union_in_both_pinned_tokenizers() {
        let sparse = [
            "a{r,m,o,n,q,f}:\n@7,0,0,4096,~,0\n",
            "a{r,m,o,n,q,f}:\n@7,1,2,32,~,0\n",
            "a{r,m,o,n,q,f}:\n@7,2,0,1048576,TODO,0\n",
            "a{r,m,o,n,q,f}:\n@7,3,0,0,~,0\n",
            "a{r,m,o,n,q,f}:\n@7,4,0,64,\"d:p,l,t\",0\n",
            "a{r,m,o,n,q,f}:\n@8,5,0,0,artifacts/out.bin,0\n",
        ]
        .concat();
        let formulas = [
            "a{b}:\n[@7,0,4096]\n",
            "a{l}:\n[@7,2,32]\n",
            "a{g}:\n[@7,0,1048576,TODO,0]\n",
            "a{d}:\n[@7]\n",
            "a{p}:\n[@7,d,0,64,p,l,t]\n",
            "a{w}:\n[@8,artifacts/out.bin]\n",
        ]
        .concat();
        let cl100k = tiktoken_rs::cl100k_base().expect("cl100k tokenizer");
        let o200k = tiktoken_rs::o200k_base().expect("o200k tokenizer");
        let sparse = measure(&sparse, &cl100k, &o200k);
        let formulas = measure(&formulas, &cl100k, &o200k);
        assert!(formulas.bytes < sparse.bytes, "{formulas:?} vs {sparse:?}");
        assert!(
            formulas.cl100k_tokens < sparse.cl100k_tokens,
            "{formulas:?} vs {sparse:?}"
        );
        assert!(
            formulas.o200k_tokens < sparse.o200k_tokens,
            "{formulas:?} vs {sparse:?}"
        );
    }

    #[test]
    fn direct_math_symbols_beat_wrappers_and_match_the_ascii_token_floor() {
        let cl100k = tiktoken_rs::cl100k_base().expect("cl100k tokenizer");
        let o200k = tiktoken_rs::o200k_base().expect("o200k tokenizer");
        let report = build_formula_algebra(&cl100k, &o200k).expect("formula report");
        assert!(report.gates.passed);
        assert!(report.gates.matches_direct_ascii_token_floor);
        assert_eq!(report.candidates.canonical_symbols.cl100k_tokens, 80);
        assert_eq!(report.candidates.canonical_symbols.o200k_tokens, 80);
        assert_eq!(report.candidates.direct_greek.cl100k_tokens, 86);
        assert_eq!(report.candidates.direct_greek.o200k_tokens, 86);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn task_corpus_baselines_are_bound_correct_and_cross_platform() {
        let report = tasks::build_report().await.expect("task corpus report");
        assert_eq!(report.schema, 2);
        assert_eq!(report.platform, std::env::consts::OS);
        assert_eq!(report.tasks.len(), 7);
        assert!(report.gates.manifest_valid);
        assert!(report.gates.all_native_shell_success);
        assert!(report.gates.all_ash_success);
        assert!(report.gates.all_initial_states_match);
        assert!(report.gates.all_final_states_match);
        assert!(report.gates.passed);
        for task in &report.tasks {
            assert!(task.native_shell.success);
            assert!(task.ash.success);
            assert_eq!(
                task.native_shell.initial_tree_sha256,
                task.declared_initial_tree_sha256
            );
            assert_eq!(
                task.ash.initial_tree_sha256,
                task.declared_initial_tree_sha256
            );
            assert_eq!(
                task.native_shell.final_tree_sha256,
                task.expected_final_tree_sha256
            );
            assert_eq!(task.ash.final_tree_sha256, task.expected_final_tree_sha256);
            assert!(task.native_shell.total.bytes > task.native_shell.stdout.bytes);
            assert!(task.ash.total.bytes > 0);
            assert_eq!(task.native_shell.stderr.bytes, 0);
        }
    }
}
