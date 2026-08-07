use std::path::PathBuf;

use ash_engine::{PermitKind, Program};
use ash_platform::Workspace;
use ash_protocol::request::{ReadArgs, ReadMode, Request};
use ash_protocol::response::{
    FinalResponse, RESULT_REDUCED, RESULT_RETAINED, ReadResult, ResultData,
};

use crate::OperationError;
use crate::projection::{charge, intern_paths, presentation_limit, protocol_path, temporary_paths};
use crate::semantic::{ReadQuery, SemanticReadMode, SemanticServices};

pub async fn execute(
    services: &SemanticServices<Workspace>,
    request: &Request,
    arguments: &ReadArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    if arguments.paths().len() > program.budget().remaining().records as usize {
        return Err(OperationError::OutputBudget);
    }
    let query = ReadQuery::new(
        arguments.paths().iter().map(PathBuf::from).collect(),
        match arguments.mode() {
            ReadMode::Bytes => SemanticReadMode::Bytes,
            ReadMode::Lines => SemanticReadMode::Lines,
        },
        arguments.offset(),
        arguments.length(),
    );
    let _filesystem = program.acquire(PermitKind::Filesystem).await?;
    let _compute = program.acquire(PermitKind::Compute).await?;
    let reads = services
        .read(&query, program.compute_pool(), program.cancellation())
        .await?
        .reads;
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
    let paths = reads
        .iter()
        .map(|read| protocol_path(&read.path))
        .collect::<Result<Vec<_>, _>>()?;
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

fn check_cancelled(program: &Program) -> Result<(), OperationError> {
    if program.cancellation().is_cancelled() {
        Err(OperationError::Cancelled)
    } else {
        program.budget().check_deadline()?;
        Ok(())
    }
}
