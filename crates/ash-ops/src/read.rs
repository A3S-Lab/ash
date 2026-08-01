use ash_engine::{PermitKind, Program};
use ash_platform::Workspace;
use ash_protocol::request::{ReadArgs, ReadMode, Request};
use ash_protocol::response::{
    FinalResponse, RESULT_REDUCED, RESULT_RETAINED, ReadResult, ResultData,
};

use crate::OperationError;
use crate::projection::{charge, intern_paths, presentation_limit, temporary_paths};

const MAX_READ_FILE_BYTES: u64 = 128 * 1024 * 1024;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &ReadArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    if arguments.paths().len() > program.budget().remaining().records as usize {
        return Err(OperationError::OutputBudget);
    }
    let resolved = arguments
        .paths()
        .iter()
        .map(|path| workspace.resolve_existing(path))
        .collect::<Result<Vec<_>, _>>()?;
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let workspace = workspace.clone();
    let mode = arguments.mode();
    let offset = arguments.offset();
    let length = arguments.length();
    let cancellation = program.cancellation().clone();
    let results = program
        .compute_pool()
        .map_ordered_owned(resolved, move |path| {
            if cancellation.is_cancelled() {
                return Ok::<_, ash_platform::PlatformError>(None);
            }
            let bytes = workspace.read_limited_sync(path, MAX_READ_FILE_BYTES)?;
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let (slice, actual_offset, actual_length) = select_range(&bytes, mode, offset, length);
            Ok(Some(RawRead {
                path: path.logical().to_owned(),
                digest,
                bytes: slice.to_vec(),
                offset: actual_offset,
                length: actual_length,
            }))
        })
        .await?;
    let mut reads = Vec::with_capacity(results.len());
    for result in results {
        let Some(result) = result? else {
            return Err(OperationError::Cancelled);
        };
        reads.push(result);
    }
    check_cancelled(program)?;

    let byte_limit = presentation_limit(program);
    let mut text_budget = byte_limit / 2;
    let mut projected = Vec::with_capacity(reads.len());
    let mut needs_reference = Vec::with_capacity(reads.len());
    for read in &reads {
        let text = std::str::from_utf8(&read.bytes).ok();
        let inline = text.filter(|text| text.len() <= text_budget);
        if let Some(text) = inline {
            text_budget -= text.len();
            projected.push(Some(text.to_owned()));
            needs_reference.push(false);
        } else {
            projected.push(None);
            needs_reference.push(true);
        }
    }
    let paths: Vec<_> = reads.iter().map(|read| read.path.clone()).collect();
    let (temporary_ids, temporary_mappings) = temporary_paths(&paths);
    let temporary_data = reads
        .iter()
        .zip(&projected)
        .zip(&needs_reference)
        .zip(temporary_ids)
        .map(|(((read, text), retained), path)| ReadResult {
            path,
            offset: read.offset,
            length: read.length,
            digest: read.digest.clone(),
            text: text.clone(),
            reference: retained.then_some(u64::MAX),
        })
        .collect();
    let reduced = needs_reference.iter().any(|retained| *retained);
    let temporary_flags = if reduced {
        RESULT_REDUCED | RESULT_RETAINED
    } else {
        0
    };
    let temporary = FinalResponse::success(
        request.id(),
        temporary_mappings,
        ResultData::Read(temporary_data),
        temporary_flags,
        None,
    )?;
    if temporary.encode()?.encode().len() > byte_limit {
        return Err(OperationError::OutputBudget);
    }

    let mut references = Vec::with_capacity(reads.len());
    for (read, retained) in reads.iter().zip(&needs_reference) {
        references.push(if *retained {
            Some(program.store().retain(read.bytes.clone())?)
        } else {
            None
        });
    }
    let (ids, mappings) = intern_paths(program, &paths)?;
    let data = reads
        .into_iter()
        .zip(projected)
        .zip(references)
        .zip(ids)
        .map(|(((read, text), reference), path)| ReadResult {
            path,
            offset: read.offset,
            length: read.length,
            digest: read.digest,
            text,
            reference,
        })
        .collect();
    let flags = if reduced {
        RESULT_REDUCED | RESULT_RETAINED
    } else {
        0
    };
    let response =
        FinalResponse::success(request.id(), mappings, ResultData::Read(data), flags, None)?;
    charge(program, &response, paths.len())?;
    Ok(response)
}

struct RawRead {
    path: String,
    digest: String,
    bytes: Vec<u8>,
    offset: u64,
    length: u64,
}

fn select_range(bytes: &[u8], mode: ReadMode, offset: u64, length: u64) -> (&[u8], u64, u64) {
    match mode {
        ReadMode::Bytes => {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let requested = usize::try_from(length).unwrap_or(usize::MAX);
            let end = start.saturating_add(requested).min(bytes.len());
            (&bytes[start..end], start as u64, (end - start) as u64)
        }
        ReadMode::Lines => select_lines(bytes, offset, length),
    }
}

fn select_lines(bytes: &[u8], offset: u64, length: u64) -> (&[u8], u64, u64) {
    if bytes.is_empty() {
        return (&[], offset, 0);
    }
    let mut starts = vec![0_usize];
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && index + 1 < bytes.len() {
            starts.push(index + 1);
        }
    }
    let requested_start = usize::try_from(offset.saturating_sub(1)).unwrap_or(usize::MAX);
    if requested_start >= starts.len() {
        return (&[], offset, 0);
    }
    let count = usize::try_from(length).unwrap_or(usize::MAX);
    let end_line = requested_start.saturating_add(count).min(starts.len());
    let start_byte = starts[requested_start];
    let end_byte = starts.get(end_line).copied().unwrap_or(bytes.len());
    (
        &bytes[start_byte..end_byte],
        requested_start as u64 + 1,
        (end_line - requested_start) as u64,
    )
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
    use ash_protocol::request::ReadMode;

    use super::select_range;

    #[test]
    fn byte_and_line_ranges_are_bounded_without_panics() {
        let bytes = b"one\ntwo\nthree";
        assert_eq!(
            select_range(bytes, ReadMode::Bytes, 4, 3),
            (&b"two"[..], 4, 3)
        );
        assert_eq!(
            select_range(bytes, ReadMode::Lines, 2, 2),
            (&b"two\nthree"[..], 2, 2)
        );
        assert_eq!(
            select_range(bytes, ReadMode::Bytes, u64::MAX, u64::MAX),
            (&b""[..], bytes.len() as u64, 0)
        );
    }
}
