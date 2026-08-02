#![forbid(unsafe_code)]

use std::error::Error;
use std::fs;

use ash_protocol::ason::{Atom, Cell, Document, Field, Key, Record, Table, Value, decode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use sha2::{Digest, Sha256};
use tiktoken_rs::{cl100k_base, o200k_base};

mod runtime;

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
    gates: Gates,
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

#[derive(Clone, Debug, Default, Serialize)]
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
    if matches!(arguments.as_slice(), [command] if command == "--runtime") {
        let mut encoded = serde_json::to_vec_pretty(&runtime::runtime_report().await?)?;
        encoded.push(b'\n');
        print!("{}", std::str::from_utf8(&encoded)?);
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
            return Err("usage: a3s-ash-bench [--runtime|--write <path>|--check <path>]".into());
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
        schema: 1,
        corpus: "benches/corpus/v1.json".to_owned(),
        corpus_sha256: hex(&Sha256::digest(CORPUS_BYTES)),
        workspace_lock_sha256: hex(&Sha256::digest(WORKSPACE_LOCK)),
        tokenizers: [
            "tiktoken-rs/0.12.0:cl100k_base",
            "tiktoken-rs/0.12.0:o200k_base",
        ],
        datasets,
        aggregate,
        gates: Gates {
            semantic_round_trip: true,
            ason_vs_record_json_cl100k_percent: cl100k_percent,
            ason_vs_record_json_o200k_percent: o200k_percent,
            required_max_percent: REQUIRED_MAX_PERCENT,
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
    use super::{build_report, measure};

    #[test]
    fn corpus_round_trips_and_passes_the_token_gate() {
        let report = build_report().expect("benchmark report");
        assert!(report.gates.semantic_round_trip);
        assert!(report.gates.passed);
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
}
