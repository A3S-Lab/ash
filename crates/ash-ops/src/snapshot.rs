use ash_engine::{PermitKind, Program};
use ash_platform::{EntryKind, NativeEntry, WalkOptions, Workspace};
use ash_protocol::ason::{Atom, Cell, Document, Field, Key, Record, Table, Value, decode};
use ash_protocol::request::{Request, SNAPSHOT_INCLUDE_HIDDEN, SnapshotArgs, SnapshotMode};
use ash_protocol::response::{
    FileKind, FinalResponse, RESULT_REDUCED, RESULT_RETAINED, RESULT_TRUNCATED, ResultData,
    SnapshotChange, SnapshotResult,
};

use crate::OperationError;
use crate::projection::{
    charge, intern_paths, largest_prefix, presentation_limit, temporary_paths,
};

const MAX_SNAPSHOT_ENTRIES: usize = 100_000;
const MAX_SNAPSHOT_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SNAPSHOT_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &SnapshotArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    let baseline = arguments
        .baseline()
        .map(|reference| program.store().get(reference))
        .transpose()?
        .map(|bytes| decode_manifest(&bytes))
        .transpose()?;
    let scope = Scope {
        paths: arguments.paths().to_vec(),
        depth: arguments.depth(),
        flags: arguments.flags(),
    };
    if baseline
        .as_ref()
        .is_some_and(|baseline| baseline.scope != scope)
    {
        return Err(OperationError::InvalidSnapshot);
    }

    let roots = arguments
        .paths()
        .iter()
        .map(|path| workspace.resolve_existing(path))
        .collect::<Result<Vec<_>, _>>()?;
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let workspace_for_walk = workspace.clone();
    let options = WalkOptions {
        max_depth: arguments.depth(),
        include_hidden: arguments.flags() & SNAPSHOT_INCLUDE_HIDDEN != 0,
        max_entries: MAX_SNAPSHOT_ENTRIES,
    };
    let batches = program
        .compute_pool()
        .map_ordered_owned(roots, move |root| workspace_for_walk.walk(root, options))
        .await?;
    let mut native_entries = Vec::new();
    for batch in batches {
        native_entries.extend(batch?);
        if native_entries.len() > MAX_SNAPSHOT_ENTRIES {
            return Err(OperationError::WorkLimit);
        }
    }
    native_entries
        .sort_unstable_by(|left, right| left.logical.as_bytes().cmp(right.logical.as_bytes()));
    native_entries.dedup_by(|left, right| left.logical == right.logical);
    let initial_bytes = native_entries.iter().try_fold(0_u64, |total, entry| {
        if entry.kind != EntryKind::File {
            return Ok(total);
        }
        if entry.size > MAX_SNAPSHOT_FILE_BYTES {
            return Err(OperationError::WorkLimit);
        }
        total
            .checked_add(entry.size)
            .ok_or(OperationError::WorkLimit)
    })?;
    if initial_bytes > MAX_SNAPSHOT_TOTAL_BYTES {
        return Err(OperationError::WorkLimit);
    }
    check_cancelled(program)?;

    let workspace_for_hash = workspace.clone();
    let cancellation = program.cancellation().clone();
    let entries = program
        .compute_pool()
        .map_ordered_owned(native_entries, move |entry| {
            identify_entry(&workspace_for_hash, entry, &cancellation)
        })
        .await?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    check_cancelled(program)?;
    if entries
        .iter()
        .any(|entry| !scope_covers(&scope, &entry.path))
    {
        return Err(OperationError::InvalidSnapshot);
    }

    let manifest = Manifest { scope, entries };
    let encoded_manifest = encode_manifest(&manifest)?;
    if encoded_manifest.len() > MAX_SNAPSHOT_MANIFEST_BYTES {
        return Err(OperationError::WorkLimit);
    }
    let changes = match arguments.mode() {
        SnapshotMode::Capture => manifest
            .entries
            .iter()
            .cloned()
            .map(|entry| Change {
                kind: SnapshotChange::Present,
                entry,
            })
            .collect(),
        SnapshotMode::Delta => delta(
            &baseline.ok_or(OperationError::InvalidSnapshot)?.entries,
            &manifest.entries,
        ),
    };

    let byte_limit = presentation_limit(program);
    let record_limit = program.budget().remaining().records as usize;
    let prefix = largest_prefix(
        changes.len(),
        record_limit,
        byte_limit,
        |length, truncated| temporary_response(request.id(), &changes[..length], truncated),
    )?;
    let truncated = prefix < changes.len();
    let projected = &changes[..prefix];
    let paths: Vec<_> = projected
        .iter()
        .map(|change| change.entry.path.clone())
        .collect();
    let (ids, mappings) = intern_paths(program, &paths)?;
    let reference = program.store().retain(encoded_manifest)?;
    let data = projected
        .iter()
        .zip(ids)
        .map(|(change, path)| result(change, path))
        .collect();
    let flags = RESULT_RETAINED
        | if truncated {
            RESULT_TRUNCATED | RESULT_REDUCED
        } else {
            0
        };
    let response = FinalResponse::success(
        request.id(),
        mappings,
        ResultData::Snapshot(data),
        flags,
        Some(reference),
    )?;
    charge(program, &response, prefix)?;
    Ok(response)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Scope {
    paths: Vec<String>,
    depth: u16,
    flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SnapshotEntry {
    path: String,
    kind: FileKind,
    size: u64,
    digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Manifest {
    scope: Scope,
    entries: Vec<SnapshotEntry>,
}

#[derive(Clone)]
struct Change {
    kind: SnapshotChange,
    entry: SnapshotEntry,
}

fn identify_entry(
    workspace: &Workspace,
    entry: &NativeEntry,
    cancellation: &ash_engine::CancellationToken,
) -> Result<SnapshotEntry, OperationError> {
    if cancellation.is_cancelled() {
        return Err(OperationError::Cancelled);
    }
    let (size, digest) = match entry.kind {
        EntryKind::File => {
            let path = workspace.resolve_existing(&entry.logical)?;
            let identity = workspace.hash_file_limited_sync(&path, entry.size)?;
            (identity.size, Some(identity.digest))
        }
        EntryKind::Symlink => (0, Some(workspace.symlink_digest(&entry.logical)?)),
        EntryKind::Directory | EntryKind::Other => (0, None),
    };
    Ok(SnapshotEntry {
        path: entry.logical.clone(),
        kind: file_kind(entry.kind),
        size,
        digest,
    })
}

fn file_kind(kind: EntryKind) -> FileKind {
    match kind {
        EntryKind::File => FileKind::File,
        EntryKind::Directory => FileKind::Directory,
        EntryKind::Symlink => FileKind::Symlink,
        EntryKind::Other => FileKind::Other,
    }
}

fn delta(baseline: &[SnapshotEntry], current: &[SnapshotEntry]) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut before = 0;
    let mut after = 0;
    while before < baseline.len() || after < current.len() {
        match (baseline.get(before), current.get(after)) {
            (Some(left), Some(right)) => match left.path.as_bytes().cmp(right.path.as_bytes()) {
                std::cmp::Ordering::Less => {
                    changes.push(Change {
                        kind: SnapshotChange::Removed,
                        entry: left.clone(),
                    });
                    before += 1;
                }
                std::cmp::Ordering::Greater => {
                    changes.push(Change {
                        kind: SnapshotChange::Added,
                        entry: right.clone(),
                    });
                    after += 1;
                }
                std::cmp::Ordering::Equal => {
                    if left != right {
                        changes.push(Change {
                            kind: SnapshotChange::Modified,
                            entry: right.clone(),
                        });
                    }
                    before += 1;
                    after += 1;
                }
            },
            (Some(left), None) => {
                changes.push(Change {
                    kind: SnapshotChange::Removed,
                    entry: left.clone(),
                });
                before += 1;
            }
            (None, Some(right)) => {
                changes.push(Change {
                    kind: SnapshotChange::Added,
                    entry: right.clone(),
                });
                after += 1;
            }
            (None, None) => break,
        }
    }
    changes
}

fn temporary_response(
    request_id: u64,
    changes: &[Change],
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let paths: Vec<_> = changes
        .iter()
        .map(|change| change.entry.path.clone())
        .collect();
    let (ids, mappings) = temporary_paths(&paths);
    let data = changes
        .iter()
        .zip(ids)
        .map(|(change, path)| result(change, path))
        .collect();
    Ok(FinalResponse::success(
        request_id,
        mappings,
        ResultData::Snapshot(data),
        RESULT_RETAINED
            | if truncated {
                RESULT_TRUNCATED | RESULT_REDUCED
            } else {
                0
            },
        Some(u64::MAX),
    )?)
}

fn result(change: &Change, path: u64) -> SnapshotResult {
    SnapshotResult {
        path,
        change: change.kind,
        kind: change.entry.kind,
        size: change.entry.size,
        digest: change.entry.digest.map(hex_digest),
    }
}

fn encode_manifest(manifest: &Manifest) -> Result<Vec<u8>, OperationError> {
    let scope = Record::new(
        keys(&["p", "d", "f"])?,
        vec![
            Cell::Vector(manifest.scope.paths.iter().map(Atom::text).collect()),
            text_cell(manifest.scope.depth),
            text_cell(manifest.scope.flags),
        ],
    )?;
    let rows = manifest
        .entries
        .iter()
        .map(|entry| {
            vec![
                Cell::Atom(Atom::text(&entry.path)),
                text_cell(entry.kind as u8),
                text_cell(entry.size),
                entry.digest.map_or_else(
                    || Cell::Atom(Atom::Null),
                    |digest| Cell::Atom(Atom::text(hex_digest(digest))),
                ),
            ]
        })
        .collect();
    Ok(Document::new(vec![
        Field::new(Key::new("k")?, Value::Scalar(Atom::text("s"))),
        Field::new(Key::new("v")?, Value::Scalar(Atom::text("1"))),
        Field::new(Key::new("q")?, Value::Record(scope)),
        Field::new(
            Key::new("d")?,
            Value::Table(Table::new(keys(&["p", "k", "z", "h"])?, rows)?),
        ),
    ])?
    .encode()
    .into_bytes())
}

fn decode_manifest(bytes: &[u8]) -> Result<Manifest, OperationError> {
    if bytes.len() > MAX_SNAPSHOT_MANIFEST_BYTES {
        return Err(OperationError::InvalidSnapshot);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| OperationError::InvalidSnapshot)?;
    let document = decode(text).map_err(|_| OperationError::InvalidSnapshot)?;
    if document.encode() != text
        || document
            .fields()
            .iter()
            .map(|field| field.key().as_str())
            .ne(["k", "v", "q", "d"])
        || scalar(document.get("k"))? != "s"
        || scalar(document.get("v"))? != "1"
    {
        return Err(OperationError::InvalidSnapshot);
    }
    let Value::Record(scope) = document.get("q").ok_or(OperationError::InvalidSnapshot)? else {
        return Err(OperationError::InvalidSnapshot);
    };
    if !columns(scope.columns(), &["p", "d", "f"]) {
        return Err(OperationError::InvalidSnapshot);
    }
    let Cell::Vector(path_atoms) = &scope.values()[0] else {
        return Err(OperationError::InvalidSnapshot);
    };
    let paths = path_atoms
        .iter()
        .map(|atom| match atom {
            Atom::Text(path) if valid_path(path) => Ok(path.clone()),
            _ => Err(OperationError::InvalidSnapshot),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if paths.is_empty()
        || paths.len() > 256
        || !paths
            .windows(2)
            .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
    {
        return Err(OperationError::InvalidSnapshot);
    }
    let depth = u16::try_from(unsigned_cell(&scope.values()[1])?)
        .map_err(|_| OperationError::InvalidSnapshot)?;
    let flags = u32::try_from(unsigned_cell(&scope.values()[2])?)
        .map_err(|_| OperationError::InvalidSnapshot)?;
    if depth > 64 || flags & !SNAPSHOT_INCLUDE_HIDDEN != 0 {
        return Err(OperationError::InvalidSnapshot);
    }
    let scope = Scope {
        paths,
        depth,
        flags,
    };
    let Value::Table(table) = document.get("d").ok_or(OperationError::InvalidSnapshot)? else {
        return Err(OperationError::InvalidSnapshot);
    };
    if !columns(table.columns(), &["p", "k", "z", "h"]) || table.rows().len() > MAX_SNAPSHOT_ENTRIES
    {
        return Err(OperationError::InvalidSnapshot);
    }
    let mut entries = Vec::with_capacity(table.rows().len());
    let mut file_bytes = 0_u64;
    for row in table.rows() {
        let path = text_atom(&row[0])?;
        if !valid_path(path)
            || !scope_covers(&scope, path)
            || entries
                .last()
                .is_some_and(|previous: &SnapshotEntry| previous.path.as_bytes() >= path.as_bytes())
        {
            return Err(OperationError::InvalidSnapshot);
        }
        let kind = match unsigned_cell(&row[1])? {
            0 => FileKind::File,
            1 => FileKind::Directory,
            2 => FileKind::Symlink,
            3 => FileKind::Other,
            _ => return Err(OperationError::InvalidSnapshot),
        };
        let size = unsigned_cell(&row[2])?;
        let digest = optional_digest(&row[3])?;
        let identity_valid = match kind {
            FileKind::File => digest.is_some(),
            FileKind::Symlink => digest.is_some() && size == 0,
            FileKind::Directory | FileKind::Other => digest.is_none() && size == 0,
        };
        if !identity_valid {
            return Err(OperationError::InvalidSnapshot);
        }
        if kind == FileKind::File {
            if size > MAX_SNAPSHOT_FILE_BYTES {
                return Err(OperationError::InvalidSnapshot);
            }
            file_bytes = file_bytes
                .checked_add(size)
                .ok_or(OperationError::InvalidSnapshot)?;
            if file_bytes > MAX_SNAPSHOT_TOTAL_BYTES {
                return Err(OperationError::InvalidSnapshot);
            }
        }
        entries.push(SnapshotEntry {
            path: path.to_owned(),
            kind,
            size,
            digest,
        });
    }
    Ok(Manifest { scope, entries })
}

fn scalar(value: Option<&Value>) -> Result<&str, OperationError> {
    match value {
        Some(Value::Scalar(Atom::Text(value))) => Ok(value),
        _ => Err(OperationError::InvalidSnapshot),
    }
}

fn columns(actual: &[Key], expected: &[&str]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.as_str() == *expected)
}

fn text_atom(cell: &Cell) -> Result<&str, OperationError> {
    match cell {
        Cell::Atom(Atom::Text(value)) => Ok(value),
        _ => Err(OperationError::InvalidSnapshot),
    }
}

fn unsigned_cell(cell: &Cell) -> Result<u64, OperationError> {
    let value = text_atom(cell)?;
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(OperationError::InvalidSnapshot);
    }
    value.parse().map_err(|_| OperationError::InvalidSnapshot)
}

fn optional_digest(cell: &Cell) -> Result<Option<[u8; 32]>, OperationError> {
    match cell {
        Cell::Atom(Atom::Null) => Ok(None),
        Cell::Atom(Atom::Text(value)) => parse_digest(value)
            .map(Some)
            .ok_or(OperationError::InvalidSnapshot),
        Cell::Atom(Atom::Reference(_)) | Cell::Vector(_) => Err(OperationError::InvalidSnapshot),
    }
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = hex_nibble(pair[0])? * 16 + hex_nibble(pair[1])?;
    }
    Some(digest)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\0', '\\', ':'])
        && !path.starts_with('/')
        && !path.ends_with('/')
        && (path == "."
            || !path
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == ".."))
}

fn scope_covers(scope: &Scope, path: &str) -> bool {
    scope.paths.iter().any(|root| {
        if path == root {
            return true;
        }
        let relative = if root == "." {
            path
        } else {
            let Some(relative) = path
                .strip_prefix(root)
                .and_then(|path| path.strip_prefix('/'))
            else {
                return false;
            };
            relative
        };
        relative.split('/').count() <= usize::from(scope.depth)
    })
}

fn keys(values: &[&str]) -> Result<Vec<Key>, ash_protocol::ason::BuildError> {
    values.iter().map(|value| Key::new(*value)).collect()
}

fn text_cell(value: impl ToString) -> Cell {
    Cell::Atom(Atom::text(value.to_string()))
}

fn hex_digest(digest: [u8; 32]) -> String {
    blake3::Hash::from_bytes(digest).to_hex().to_string()
}

fn check_cancelled(program: &Program) -> Result<(), OperationError> {
    if program.cancellation().is_cancelled() {
        Err(OperationError::Cancelled)
    } else {
        program.budget().check_deadline()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ash_protocol::response::{FileKind, SnapshotChange};

    use super::{Change, Manifest, Scope, SnapshotEntry, decode_manifest, delta, encode_manifest};

    fn entry(path: &str, bytes: &[u8]) -> SnapshotEntry {
        SnapshotEntry {
            path: path.to_owned(),
            kind: FileKind::File,
            size: bytes.len() as u64,
            digest: Some(*blake3::hash(bytes).as_bytes()),
        }
    }

    #[test]
    fn manifest_round_trips_and_delta_is_stable() {
        let manifest = Manifest {
            scope: Scope {
                paths: vec![".".to_owned()],
                depth: 64,
                flags: 0,
            },
            entries: vec![entry("a", b"old"), entry("removed", b"gone")],
        };
        let encoded = encode_manifest(&manifest).expect("encode");
        assert_eq!(decode_manifest(&encoded).expect("decode"), manifest);

        let current = vec![entry("a", b"new"), entry("added", b"here")];
        let changes = delta(&manifest.entries, &current);
        assert_eq!(changes.len(), 3);
        assert!(matches!(
            changes.as_slice(),
            [
                Change {
                    kind: SnapshotChange::Modified,
                    ..
                },
                Change {
                    kind: SnapshotChange::Added,
                    ..
                },
                Change {
                    kind: SnapshotChange::Removed,
                    ..
                }
            ]
        ));
    }
}
