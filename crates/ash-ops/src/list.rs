use ash_engine::{PermitKind, Program};
use ash_platform::{EntryKind, NativeEntry, WalkOptions, Workspace};
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
    charge, intern_paths, largest_prefix, presentation_limit, temporary_paths,
};

const MAX_LIST_ENTRIES: usize = 1_000_000;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &ListArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    let roots = arguments
        .paths()
        .iter()
        .map(|path| workspace.resolve_existing(path))
        .collect::<Result<Vec<_>, _>>()?;
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let workspace = workspace.clone();
    let options = WalkOptions {
        max_depth: arguments.depth(),
        include_hidden: arguments.flags() & LIST_INCLUDE_HIDDEN != 0,
        max_entries: MAX_LIST_ENTRIES,
    };
    let batches = program
        .compute_pool()
        .map_ordered_owned(roots, move |root| workspace.walk(root, options))
        .await?;
    let mut entries = Vec::new();
    for batch in batches {
        entries.extend(batch?);
        if entries.len() > MAX_LIST_ENTRIES {
            return Err(OperationError::WorkLimit);
        }
    }
    check_cancelled(program)?;
    entries.retain(|entry| selected(entry, arguments.flags()));
    entries.sort_unstable_by(|left, right| left.logical.as_bytes().cmp(right.logical.as_bytes()));
    entries.dedup_by(|left, right| left.logical == right.logical);

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
    let paths: Vec<_> = projected
        .iter()
        .map(|entry| entry.logical.clone())
        .collect();
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
    entries: &[NativeEntry],
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let paths: Vec<_> = entries.iter().map(|entry| entry.logical.clone()).collect();
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

fn result_entry(entry: &NativeEntry, path: u64) -> ListEntry {
    ListEntry {
        path,
        kind: match entry.kind {
            EntryKind::File => FileKind::File,
            EntryKind::Directory => FileKind::Directory,
            EntryKind::Symlink => FileKind::Symlink,
            EntryKind::Other => FileKind::Other,
        },
        size: entry.size,
    }
}

fn selected(entry: &NativeEntry, flags: u32) -> bool {
    if flags & LIST_FILES_ONLY != 0 {
        entry.kind == EntryKind::File
    } else if flags & LIST_DIRECTORIES_ONLY != 0 {
        entry.kind == EntryKind::Directory
    } else {
        true
    }
}

fn encode_evidence(entries: &[NativeEntry]) -> Result<Vec<u8>, OperationError> {
    let rows = entries
        .iter()
        .map(|entry| {
            vec![
                Cell::Atom(Atom::text(&entry.logical)),
                Cell::Atom(Atom::text((result_entry(entry, 1).kind as u8).to_string())),
                Cell::Atom(Atom::text(entry.size.to_string())),
            ]
        })
        .collect();
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
