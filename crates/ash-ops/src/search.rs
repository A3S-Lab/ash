use ash_engine::{PermitKind, Program};
use ash_platform::{EntryKind, WalkOptions, Workspace};
use ash_protocol::ason::{Atom, Cell, Document, Field, Key, Table, Value};
use ash_protocol::request::{
    Request, SEARCH_CASE_INSENSITIVE, SEARCH_INCLUDE_HIDDEN, SEARCH_REGEX, SearchArgs,
};
use ash_protocol::response::{
    FinalResponse, RESULT_NORMALIZED_TEXT, RESULT_PARTIAL, RESULT_REDUCED, RESULT_RETAINED,
    RESULT_TRUNCATED, ResultData, SearchMatch,
};
use regex::{Regex, RegexBuilder};

use crate::OperationError;
use crate::projection::{
    charge, intern_paths, largest_prefix, presentation_limit, temporary_paths,
};

const MAX_SEARCH_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEARCH_MATCHES: usize = 1_000_000;
const MAX_MATCHES_PER_FILE: usize = 100_000;
const MAX_SEARCH_ENTRIES: usize = 1_000_000;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &SearchArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    let matcher = Matcher::new(arguments)?;
    let roots = arguments
        .paths()
        .iter()
        .map(|path| workspace.resolve_existing(path))
        .collect::<Result<Vec<_>, _>>()?;
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let workspace_for_walk = workspace.clone();
    let options = WalkOptions {
        max_depth: 64,
        include_hidden: arguments.flags() & SEARCH_INCLUDE_HIDDEN != 0,
        max_entries: MAX_SEARCH_ENTRIES,
    };
    let batches = program
        .compute_pool()
        .map_ordered_owned(roots, move |root| workspace_for_walk.walk(root, options))
        .await?;
    let mut paths = Vec::new();
    for batch in batches {
        paths.extend(
            batch?
                .into_iter()
                .filter(|entry| entry.kind == EntryKind::File)
                .map(|entry| entry.logical),
        );
    }
    paths.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    paths.dedup();
    check_cancelled(program)?;

    let resolved = paths
        .iter()
        .map(|path| workspace.resolve_existing(path))
        .collect::<Result<Vec<_>, _>>()?;
    let workspace = workspace.clone();
    let cancellation = program.cancellation().clone();
    let scanned = program
        .compute_pool()
        .map_ordered_owned(resolved, move |path| {
            if cancellation.is_cancelled() {
                return Ok::<_, ash_platform::PlatformError>(FileMatches::cancelled());
            }
            let bytes = workspace.read_limited_sync(path, MAX_SEARCH_FILE_BYTES)?;
            Ok(scan_file(path.logical(), &bytes, &matcher))
        })
        .await?;
    let mut matches = Vec::new();
    let mut normalized = false;
    let mut partial = false;
    for result in scanned {
        let result: FileMatches = result?;
        if result.cancelled {
            return Err(OperationError::Cancelled);
        }
        if result.overflowed {
            return Err(OperationError::WorkLimit);
        }
        normalized |= result.normalized;
        partial |= result.binary;
        matches.extend(result.matches);
        if matches.len() > MAX_SEARCH_MATCHES {
            return Err(OperationError::WorkLimit);
        }
    }
    matches.sort_unstable_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
    });
    matches.dedup();
    check_cancelled(program)?;

    let base_flags = (if normalized {
        RESULT_NORMALIZED_TEXT
    } else {
        0
    }) | (if partial { RESULT_PARTIAL } else { 0 });
    let byte_limit = presentation_limit(program);
    let record_limit = program.budget().remaining().records as usize;
    let prefix = largest_prefix(
        matches.len(),
        record_limit,
        byte_limit,
        |length, truncated| {
            temporary_response(request.id(), &matches[..length], base_flags, truncated)
        },
    )?;
    let truncated = prefix < matches.len();
    let reference = if truncated {
        Some(program.store().retain(encode_evidence(&matches)?)?)
    } else {
        None
    };
    let projected = &matches[..prefix];
    let paths: Vec<_> = projected.iter().map(|entry| entry.path.clone()).collect();
    let (ids, mappings) = intern_paths(program, &paths)?;
    let data = projected
        .iter()
        .zip(ids)
        .map(|(entry, path)| SearchMatch {
            path,
            line: entry.line,
            column: entry.column,
            text: entry.text.clone(),
        })
        .collect();
    let flags = base_flags
        | if truncated {
            RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED
        } else {
            0
        };
    let response = FinalResponse::success(
        request.id(),
        mappings,
        ResultData::Search(data),
        flags,
        reference,
    )?;
    charge(program, &response, prefix)?;
    Ok(response)
}

#[derive(Clone)]
enum Matcher {
    Literal(String),
    Regex(Regex),
}

impl Matcher {
    fn new(arguments: &SearchArgs) -> Result<Self, OperationError> {
        let regex = arguments.flags() & SEARCH_REGEX != 0;
        let insensitive = arguments.flags() & SEARCH_CASE_INSENSITIVE != 0;
        if regex || insensitive {
            let pattern = if regex {
                arguments.query().to_owned()
            } else {
                regex::escape(arguments.query())
            };
            Ok(Self::Regex(
                RegexBuilder::new(&pattern)
                    .case_insensitive(insensitive)
                    .size_limit(16 * 1024 * 1024)
                    .build()?,
            ))
        } else {
            Ok(Self::Literal(arguments.query().to_owned()))
        }
    }

    fn find(&self, line: &str) -> Option<usize> {
        match self {
            Self::Literal(query) => line.find(query),
            Self::Regex(regex) => regex.find(line).map(|matched| matched.start()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawMatch {
    path: String,
    line: u64,
    column: u64,
    text: String,
}

struct FileMatches {
    matches: Vec<RawMatch>,
    normalized: bool,
    binary: bool,
    overflowed: bool,
    cancelled: bool,
}

impl FileMatches {
    const fn cancelled() -> Self {
        Self {
            matches: Vec::new(),
            normalized: false,
            binary: false,
            overflowed: false,
            cancelled: true,
        }
    }
}

fn scan_file(path: &str, bytes: &[u8], matcher: &Matcher) -> FileMatches {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return FileMatches {
            matches: Vec::new(),
            normalized: false,
            binary: true,
            overflowed: false,
            cancelled: false,
        };
    };
    let mut matches = Vec::new();
    let mut normalized = false;
    for (index, raw_line) in text.split_terminator('\n').enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        normalized |= line.len() != raw_line.len();
        if let Some(column) = matcher.find(line) {
            matches.push(RawMatch {
                path: path.to_owned(),
                line: index as u64 + 1,
                column: column as u64 + 1,
                text: line.to_owned(),
            });
            if matches.len() > MAX_MATCHES_PER_FILE {
                return FileMatches {
                    matches,
                    normalized,
                    binary: false,
                    overflowed: true,
                    cancelled: false,
                };
            }
        }
    }
    FileMatches {
        matches,
        normalized,
        binary: false,
        overflowed: false,
        cancelled: false,
    }
}

fn temporary_response(
    request_id: u64,
    matches: &[RawMatch],
    base_flags: u32,
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let paths: Vec<_> = matches.iter().map(|entry| entry.path.clone()).collect();
    let (ids, mappings) = temporary_paths(&paths);
    let data = matches
        .iter()
        .zip(ids)
        .map(|(entry, path)| SearchMatch {
            path,
            line: entry.line,
            column: entry.column,
            text: entry.text.clone(),
        })
        .collect();
    let flags = base_flags
        | if truncated {
            RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED
        } else {
            0
        };
    Ok(FinalResponse::success(
        request_id,
        mappings,
        ResultData::Search(data),
        flags,
        truncated.then_some(u64::MAX),
    )?)
}

fn encode_evidence(matches: &[RawMatch]) -> Result<Vec<u8>, OperationError> {
    let rows = matches
        .iter()
        .map(|entry| {
            vec![
                Cell::Atom(Atom::text(&entry.path)),
                Cell::Atom(Atom::text(entry.line.to_string())),
                Cell::Atom(Atom::text(entry.column.to_string())),
                Cell::Atom(Atom::text(&entry.text)),
            ]
        })
        .collect();
    let document = Document::new(vec![
        Field::new(Key::new("k")?, Value::Scalar(Atom::text("g"))),
        Field::new(
            Key::new("d")?,
            Value::Table(Table::new(
                ["p", "l", "c", "t"]
                    .into_iter()
                    .map(Key::new)
                    .collect::<Result<_, _>>()?,
                rows,
            )?),
        ),
    ])?;
    Ok(document.encode().into_bytes())
}

fn check_cancelled(program: &Program) -> Result<(), OperationError> {
    if program.cancellation().is_cancelled() {
        Err(OperationError::Cancelled)
    } else {
        program.budget().check_deadline()?;
        Ok(())
    }
}
