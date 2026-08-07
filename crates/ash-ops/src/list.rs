use std::path::PathBuf;

use ash_engine::{PermitKind, Program};
use ash_platform::Workspace;
use ash_protocol::ason::{Atom, Cell, Document, Field, Key, Table, Value};
use ash_protocol::request::{
    LIST_DIRECTORIES_ONLY, LIST_FILES_ONLY, LIST_INCLUDE_HIDDEN, ListArgs, Request,
};
use ash_protocol::response::{
    FileKind, FinalResponse, ListEntry, RESULT_REDUCED, RESULT_RETAINED, RESULT_TRUNCATED,
    ResultData,
};

use crate::OperationError;
use crate::projection::{
    charge, intern_paths, largest_prefix, presentation_limit, protocol_path, temporary_paths,
};
use crate::semantic::{
    ListQuery, SemanticEntry, SemanticEntryKind, SemanticListFilter, SemanticServices,
};

pub async fn execute(
    services: &SemanticServices<Workspace>,
    request: &Request,
    arguments: &ListArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    let filter = if arguments.flags() & LIST_FILES_ONLY != 0 {
        SemanticListFilter::Files
    } else if arguments.flags() & LIST_DIRECTORIES_ONLY != 0 {
        SemanticListFilter::Directories
    } else {
        SemanticListFilter::All
    };
    let query = ListQuery::new(
        arguments.paths().iter().map(PathBuf::from).collect(),
        arguments.depth(),
        arguments.flags() & LIST_INCLUDE_HIDDEN != 0,
        filter,
    );
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let entries = services
        .list(&query, program.compute_pool(), program.cancellation())
        .await?
        .entries;
    check_cancelled(program)?;

    let byte_limit = presentation_limit(program);
    let record_limit = program.budget().remaining().records as usize;
    let prefix = largest_prefix(
        entries.len(),
        record_limit,
        byte_limit,
        |length, truncated| temporary_response(request.id(), &entries[..length], truncated),
    )?;
    let truncated = prefix < entries.len();
    let reference = if truncated {
        Some(program.store().retain(encode_evidence(&entries)?)?)
    } else {
        None
    };
    let projected = &entries[..prefix];
    let paths = projected
        .iter()
        .map(|entry| protocol_path(&entry.path))
        .collect::<Result<Vec<_>, _>>()?;
    let (ids, mappings) = intern_paths(program, &paths)?;
    let data = projected
        .iter()
        .zip(ids)
        .map(|(entry, path)| result_entry(entry, path))
        .collect();
    let flags = if truncated {
        RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED
    } else {
        0
    };
    let response = FinalResponse::success(
        request.id(),
        mappings,
        ResultData::List(data),
        flags,
        reference,
    )?;
    charge(program, &response, prefix)?;
    Ok(response)
}

fn temporary_response(
    request_id: u64,
    entries: &[SemanticEntry],
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let paths = entries
        .iter()
        .map(|entry| protocol_path(&entry.path))
        .collect::<Result<Vec<_>, _>>()?;
    let (ids, mappings) = temporary_paths(&paths);
    let data = entries
        .iter()
        .zip(ids)
        .map(|(entry, path)| result_entry(entry, path))
        .collect();
    let (flags, reference) = if truncated {
        (
            RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED,
            Some(u64::MAX),
        )
    } else {
        (0, None)
    };
    Ok(FinalResponse::success(
        request_id,
        mappings,
        ResultData::List(data),
        flags,
        reference,
    )?)
}

fn result_entry(entry: &SemanticEntry, path: u64) -> ListEntry {
    ListEntry {
        path,
        kind: match entry.kind {
            SemanticEntryKind::File => FileKind::File,
            SemanticEntryKind::Directory => FileKind::Directory,
            SemanticEntryKind::Symlink => FileKind::Symlink,
            SemanticEntryKind::Other => FileKind::Other,
        },
        size: entry.size,
    }
}

fn encode_evidence(entries: &[SemanticEntry]) -> Result<Vec<u8>, OperationError> {
    let rows = entries
        .iter()
        .map(|entry| {
            Ok(vec![
                Cell::Atom(Atom::text(protocol_path(&entry.path)?)),
                Cell::Atom(Atom::text((result_entry(entry, 1).kind as u8).to_string())),
                Cell::Atom(Atom::text(entry.size.to_string())),
            ])
        })
        .collect::<Result<Vec<_>, OperationError>>()?;
    let document = Document::new(vec![
        Field::new(Key::new("k")?, Value::Scalar(Atom::text("l"))),
        Field::new(
            Key::new("d")?,
            Value::Table(Table::new(
                ["p", "k", "z"]
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
