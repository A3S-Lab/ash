use std::collections::BTreeMap;

use ash_engine::Program;
use ash_platform::PlatformError;
use ash_protocol::response::{FinalResponse, PathMapping};

use crate::{OperationError, SemanticPath};

pub fn protocol_path(path: &SemanticPath) -> Result<String, OperationError> {
    path.as_path()
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| PlatformError::NonUtf8Path.into())
}

pub fn presentation_limit(program: &Program) -> usize {
    let remaining = program.budget().remaining();
    let token_estimate = u64::from(remaining.tokens).saturating_mul(4);
    usize::try_from(remaining.output_bytes.min(token_estimate)).unwrap_or(usize::MAX)
}

pub fn largest_prefix<F>(
    total: usize,
    record_limit: usize,
    byte_limit: usize,
    mut candidate: F,
) -> Result<usize, OperationError>
where
    F: FnMut(usize, bool) -> Result<FinalResponse, OperationError>,
{
    let upper = total.min(record_limit);
    let mut low = 0;
    let mut high = upper;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let response = candidate(middle, middle < total)?;
        if response.encode()?.encode().len() <= byte_limit {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let response = candidate(low, low < total)?;
    if response.encode()?.encode().len() > byte_limit {
        Err(OperationError::OutputBudget)
    } else {
        Ok(low)
    }
}

pub fn temporary_paths(paths: &[String]) -> (Vec<u64>, Vec<PathMapping>) {
    let mut unique = BTreeMap::new();
    for path in paths {
        unique.insert(path.clone(), 0_u64);
    }
    let base = u64::MAX - unique.len() as u64;
    let mut mappings = Vec::with_capacity(unique.len());
    for (index, (path, id)) in unique.iter_mut().enumerate() {
        *id = base + index as u64 + 1;
        mappings.push(PathMapping {
            id: *id,
            value: path.clone(),
        });
    }
    let ids = paths.iter().map(|path| unique[path]).collect();
    (ids, mappings)
}

pub fn intern_paths(
    program: &Program,
    paths: &[String],
) -> Result<(Vec<u64>, Vec<PathMapping>), OperationError> {
    let interned = program.paths().intern(paths)?;
    let mappings = interned
        .introduced
        .into_iter()
        .map(|entry| PathMapping {
            id: entry.id,
            value: entry.value,
        })
        .collect();
    Ok((interned.ids, mappings))
}

pub fn charge(
    program: &Program,
    response: &FinalResponse,
    records: usize,
) -> Result<(), OperationError> {
    let encoded = response.encode()?.encode();
    let records = u32::try_from(records).map_err(|_| OperationError::WorkLimit)?;
    program.budget().reserve_records(records)?;
    program.budget().reserve_output(encoded.len() as u64)?;
    Ok(())
}
