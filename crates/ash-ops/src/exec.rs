use std::ffi::OsString;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Instant;

use ash_engine::{PermitKind, Program};
use ash_platform::{EnvironmentChange, ProcessExit, ProcessSpec, Workspace};
use ash_protocol::request::{EXEC_CLEAR_ENVIRONMENT, ExecArgs, InputSource, Request};
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, ProcessResult, RESULT_NORMALIZED_TEXT,
    RESULT_PARTIAL, RESULT_REDUCED, RESULT_RETAINED, RESULT_TRUNCATED, ResultData, RetryClass,
    Status, StreamResult, TerminationKind,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::OperationError;
use crate::projection::{charge, presentation_limit};

const MAX_CAPTURE_BYTES_PER_STREAM: usize = 4 * 1024 * 1024;
const MAX_STDIN_BYTES: usize = 64 * 1024 * 1024;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &ExecArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    let stdin = stdin_bytes(arguments.stdin(), program)?;
    let environment = environment(arguments.environment())?;
    let _process_permit = program.acquire(PermitKind::Process).await?;
    let started = Instant::now();
    let mut process = workspace.spawn(&ProcessSpec {
        executable: arguments.executable().to_owned(),
        argv: arguments.argv().to_vec(),
        cwd: arguments.cwd().to_owned(),
        environment,
        clear_environment: arguments.flags() & EXEC_CLEAR_ENVIRONMENT != 0,
        pipe_stdin: stdin.is_some(),
    })?;
    let stdout = process.take_stdout().ok_or(OperationError::WorkLimit)?;
    let stderr = process.take_stderr().ok_or(OperationError::WorkLimit)?;
    let stdout_task = tokio::spawn(capture(stdout));
    let stderr_task = tokio::spawn(capture(stderr));
    let stdin_task = match (process.take_stdin(), stdin) {
        (Some(mut writer), Some(bytes)) => Some(tokio::spawn(async move {
            let result = writer.write_all(&bytes).await;
            let _ = writer.shutdown().await;
            result
        })),
        _ => None,
    };

    let stop = tokio::select! {
        biased;
        () = program.cancellation().cancelled() => Stop::Cancelled,
        () = tokio::time::sleep_until(program.budget().deadline()) => Stop::TimedOut,
        result = process.wait() => Stop::Exited(result?),
    };
    if !matches!(stop, Stop::Exited(_)) {
        process.terminate().await?;
        let _ = process.wait().await;
    }
    if let Some(task) = stdin_task {
        match task.await? {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error.into()),
        }
    }
    let stdout = stdout_task.await??;
    let stderr = stderr_task.await??;
    let elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    build_response(request.id(), stop, stdout, stderr, elapsed_millis, program)
}

enum Stop {
    Exited(ProcessExit),
    TimedOut,
    Cancelled,
}

struct Capture {
    bytes: Vec<u8>,
    overflowed: bool,
}

async fn capture(mut reader: impl AsyncRead + Unpin) -> Result<Capture, std::io::Error> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut overflowed = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CAPTURE_BYTES_PER_STREAM.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        overflowed |= retained < read;
    }
    Ok(Capture { bytes, overflowed })
}

fn build_response(
    request_id: u64,
    stop: Stop,
    stdout: Capture,
    stderr: Capture,
    elapsed_millis: u64,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let failed = !matches!(stop, Stop::Exited(ProcessExit { success: true, .. }));
    let limit = presentation_limit(program);
    let (stdout_budget, stderr_budget) = if failed {
        (limit / 8, limit / 2)
    } else {
        (limit / 2, limit / 8)
    };
    let stdout_projection = project(&stdout.bytes, stdout_budget);
    let stderr_projection = project(&stderr.bytes, stderr_budget);
    let stdout_retained = stdout.overflowed
        || stdout_projection.reduced
        || stdout_projection.normalized
        || stdout_projection.text.is_none() && !stdout.bytes.is_empty();
    let stderr_retained = stderr.overflowed
        || stderr_projection.reduced
        || stderr_projection.normalized
        || stderr_projection.text.is_none() && !stderr.bytes.is_empty();
    let result_flags = flags(
        &stdout,
        &stderr,
        &stdout_projection,
        &stderr_projection,
        stdout_retained || stderr_retained,
    );

    let temporary = response(
        request_id,
        &stop,
        elapsed_millis,
        stream_result(&stdout_projection, stdout_retained.then_some(u64::MAX)),
        stream_result(&stderr_projection, stderr_retained.then_some(u64::MAX)),
        result_flags,
    )?;
    if temporary.encode()?.encode().len() > limit {
        return Err(OperationError::OutputBudget);
    }

    let stdout_reference = if stdout_retained {
        Some(program.store().retain(stdout.bytes)?)
    } else {
        None
    };
    let stderr_reference = if stderr_retained {
        Some(program.store().retain(stderr.bytes)?)
    } else {
        None
    };
    let response = response(
        request_id,
        &stop,
        elapsed_millis,
        stream_result(&stdout_projection, stdout_reference),
        stream_result(&stderr_projection, stderr_reference),
        result_flags,
    )?;
    charge(program, &response, 1)?;
    Ok(response)
}

fn response(
    request_id: u64,
    stop: &Stop,
    elapsed_millis: u64,
    stdout: StreamResult,
    stderr: StreamResult,
    flags: u32,
) -> Result<FinalResponse, OperationError> {
    let (termination, code) = match stop {
        Stop::Exited(exit) if exit.signal.is_some() => (TerminationKind::Signaled, exit.signal),
        Stop::Exited(exit) => (TerminationKind::Exited, exit.code),
        Stop::TimedOut => (TerminationKind::TimedOut, None),
        Stop::Cancelled => (TerminationKind::Cancelled, None),
    };
    let data = ResultData::Exec(ProcessResult {
        termination,
        code,
        elapsed_millis,
        stdout,
        stderr,
    });
    let response = if flags & RESULT_PARTIAL != 0 {
        FinalResponse::failure(
            request_id,
            Status::BudgetExceeded,
            error(ErrorCode::StorageBudget, RetryClass::CorrectRequest),
            vec![],
            Some(data),
            flags,
            None,
        )?
    } else {
        match stop {
            Stop::Exited(exit) if exit.success => {
                FinalResponse::success(request_id, vec![], data, flags, None)?
            }
            Stop::Exited(_) => FinalResponse::failure(
                request_id,
                Status::Failed,
                error(ErrorCode::ProcessFailed, RetryClass::Never),
                vec![],
                Some(data),
                flags,
                None,
            )?,
            Stop::TimedOut => FinalResponse::failure(
                request_id,
                Status::TimedOut,
                error(ErrorCode::ProcessTimedOut, RetryClass::RetrySame),
                vec![],
                Some(data),
                flags,
                None,
            )?,
            Stop::Cancelled => FinalResponse::failure(
                request_id,
                Status::Cancelled,
                error(ErrorCode::ProcessCancelled, RetryClass::Never),
                vec![],
                Some(data),
                flags,
                None,
            )?,
        }
    };
    Ok(response)
}

fn error(code: ErrorCode, retry: RetryClass) -> ErrorRecord {
    ErrorRecord {
        code,
        retry,
        stage: ErrorStage::Execute,
        evidence: None,
        argument: None,
    }
}

struct Projection {
    text: Option<String>,
    reduced: bool,
    normalized: bool,
}

fn project(bytes: &[u8], limit: usize) -> Projection {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Projection {
            text: None,
            reduced: !bytes.is_empty(),
            normalized: false,
        };
    };
    let normalized = text.contains("\r\n");
    let normalized_text = if normalized {
        text.replace("\r\n", "\n")
    } else {
        text.to_owned()
    };
    if normalized_text.len() <= limit {
        return Projection {
            text: (!normalized_text.is_empty()).then_some(normalized_text),
            reduced: false,
            normalized,
        };
    }
    if limit < 8 {
        return Projection {
            text: None,
            reduced: true,
            normalized,
        };
    }
    let separator = "\n...\n";
    let content_limit = limit.saturating_sub(separator.len());
    let head_end = floor_boundary(&normalized_text, content_limit / 2);
    let tail_start = ceil_boundary(
        &normalized_text,
        normalized_text
            .len()
            .saturating_sub(content_limit - head_end),
    );
    Projection {
        text: Some(format!(
            "{}{}{}",
            &normalized_text[..head_end],
            separator,
            &normalized_text[tail_start..]
        )),
        reduced: true,
        normalized,
    }
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn stream_result(projection: &Projection, reference: Option<u64>) -> StreamResult {
    StreamResult {
        projection: projection.text.clone(),
        reference,
    }
}

fn flags(
    stdout: &Capture,
    stderr: &Capture,
    stdout_projection: &Projection,
    stderr_projection: &Projection,
    retained: bool,
) -> u32 {
    let reduced = stdout_projection.reduced || stderr_projection.reduced;
    (if reduced { RESULT_REDUCED } else { 0 })
        | (if reduced && !stdout.overflowed && !stderr.overflowed {
            RESULT_TRUNCATED
        } else {
            0
        })
        | (if stdout_projection.normalized || stderr_projection.normalized {
            RESULT_NORMALIZED_TEXT
        } else {
            0
        })
        | (if retained { RESULT_RETAINED } else { 0 })
        | (if stdout.overflowed || stderr.overflowed {
            RESULT_PARTIAL
        } else {
            0
        })
}

fn stdin_bytes(
    source: &InputSource,
    program: &Program,
) -> Result<Option<Arc<[u8]>>, OperationError> {
    let bytes = match source {
        InputSource::None => return Ok(None),
        InputSource::Inline(value) => Arc::<[u8]>::from(value.as_bytes()),
        InputSource::Reference(reference) => program.store().get(*reference)?,
    };
    if bytes.len() > MAX_STDIN_BYTES {
        return Err(OperationError::WorkLimit);
    }
    Ok(Some(bytes))
}

fn environment(entries: &[String]) -> Result<Vec<EnvironmentChange>, OperationError> {
    entries
        .iter()
        .map(|entry| {
            if let Some(name) = entry.strip_prefix('-') {
                Ok(EnvironmentChange::Remove(OsString::from(name)))
            } else if let Some((name, value)) = entry.split_once('=') {
                Ok(EnvironmentChange::Set(
                    OsString::from(name),
                    OsString::from(value),
                ))
            } else {
                Err(OperationError::WorkLimit)
            }
        })
        .collect()
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
    use super::{ceil_boundary, floor_boundary, project};

    #[test]
    fn projection_keeps_utf8_boundaries_and_both_ends() {
        let projection = project("头部-0123456789-尾部".as_bytes(), 16);
        let text = projection.text.expect("excerpt");
        assert!(projection.reduced);
        assert!(text.contains("..."));
        assert!(std::str::from_utf8(text.as_bytes()).is_ok());
        assert!(floor_boundary("中", 1) == 0);
        assert!(ceil_boundary("中", 1) == "中".len());
    }
}
