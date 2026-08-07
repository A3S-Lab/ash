use std::path::PathBuf;

use ash_engine::{PermitKind, Program};
use ash_platform::Workspace;
use ash_protocol::ason::{Atom, Cell, Document, Field, Key, Table, Value};
use ash_protocol::request::{
    Request, SEARCH_CASE_INSENSITIVE, SEARCH_INCLUDE_HIDDEN, SEARCH_REGEX, SearchArgs,
};
use ash_protocol::response::{
    FinalResponse, RESULT_NORMALIZED_TEXT, RESULT_PARTIAL, RESULT_REDUCED, RESULT_RETAINED,
    RESULT_TRUNCATED, ResultData, SearchMatch,
};

use crate::OperationError;
use crate::projection::{
    charge, intern_paths, largest_prefix, presentation_limit, protocol_path, temporary_paths,
};
use crate::semantic::{
    SearchQuery, SemanticSearchMatch, SemanticSearchPattern, SemanticSearchResult, SemanticServices,
};

pub async fn execute(
    services: &SemanticServices<Workspace>,
    request: &Request,
    arguments: &SearchArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    let query = SearchQuery::new(
        arguments.query().to_owned(),
        arguments.paths().iter().map(PathBuf::from).collect(),
        if arguments.flags() & SEARCH_REGEX != 0 {
            SemanticSearchPattern::Regex
        } else {
            SemanticSearchPattern::Literal
        },
        arguments.flags() & SEARCH_CASE_INSENSITIVE != 0,
        arguments.flags() & SEARCH_INCLUDE_HIDDEN != 0,
    );
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let SemanticSearchResult {
        matches,
        normalized_text,
        partial,
    } = services
        .search(&query, program.compute_pool(), program.cancellation())
        .await?;
    check_cancelled(program)?;

    let base_flags = (if normalized_text {
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
    let paths = projected
        .iter()
        .map(|entry| protocol_path(&entry.path))
        .collect::<Result<Vec<_>, _>>()?;
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

fn temporary_response(
    request_id: u64,
    matches: &[SemanticSearchMatch],
    base_flags: u32,
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let paths = matches
        .iter()
        .map(|entry| protocol_path(&entry.path))
        .collect::<Result<Vec<_>, _>>()?;
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

fn encode_evidence(matches: &[SemanticSearchMatch]) -> Result<Vec<u8>, OperationError> {
    let rows = matches
        .iter()
        .map(|entry| {
            Ok(vec![
                Cell::Atom(Atom::text(protocol_path(&entry.path)?)),
                Cell::Atom(Atom::text(entry.line.to_string())),
                Cell::Atom(Atom::text(entry.column.to_string())),
                Cell::Atom(Atom::text(&entry.text)),
            ])
        })
        .collect::<Result<Vec<_>, OperationError>>()?;
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
