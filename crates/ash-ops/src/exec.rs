use std::ffi::OsString;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Instant;

use ash_engine::{PermitKind, Program};
use ash_platform::{EnvironmentChange, ProcessExit, ProcessSpec, Workspace};
use ash_protocol::request::{EXEC_CLEAR_ENVIRONMENT, ExecArgs, InputSource, Request};
use ash_protocol::response::{
    ErrorCode, ErrorRecord, ErrorStage, FinalResponse, ProcessResult, RESULT_NORMALIZED_TEXT,
    RESULT_REDUCED, RESULT_RETAINED, RESULT_TRUNCATED, ResultData, RetryClass, Status,
    StreamResult, TerminationKind,
};
use ash_store::{
    CapturedContent, CapturedView, DEFAULT_CAPTURE_MEMORY_BYTES, ResultStore, StoreError,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::projection::{charge, presentation_limit};
use crate::{OperationError, collapse_repeated_blocks, collapse_repeated_lines};

const MAX_STDIN_BYTES: usize = 64 * 1024 * 1024;

pub async fn execute(
    workspace: &Workspace,
    request: &Request,
    arguments: &ExecArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    let stdin = stdin_bytes(arguments.stdin(), program).await?;
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
    let stdout_task = tokio::spawn(capture(stdout, Arc::clone(program.store())));
    let stderr_task = tokio::spawn(capture(stderr, Arc::clone(program.store())));
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
    let (stdout, stderr) = tokio::join!(stdout_task, stderr_task);
    let stdout = stdout??;
    let stderr = stderr??;
    let elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    build_response(request.id(), stop, stdout, stderr, elapsed_millis, program).await
}

enum Stop {
    Exited(ProcessExit),
    TimedOut,
    Cancelled,
}

struct Capture {
    content: Option<CapturedContent>,
    quota_error: Option<StoreError>,
    text: StreamText,
}

impl Capture {
    fn content(&self) -> Result<&CapturedContent, StoreError> {
        self.content.as_ref().ok_or(StoreError::Invariant)
    }

    fn project(&self, limit: usize) -> Result<Projection, StoreError> {
        let content = self.content()?;
        Ok(match content.view() {
            CapturedView::Complete(bytes) => project(bytes, limit),
            CapturedView::Sampled {
                head,
                head_next,
                tail,
            } => project_sampled(head, head_next, tail, limit, &self.text),
        })
    }

    fn into_retained(self, retained: bool) -> Option<CapturedContent> {
        if retained { self.content } else { None }
    }
}

async fn capture(
    mut reader: impl AsyncRead + Unpin,
    store: Arc<ResultStore>,
) -> Result<Capture, OperationError> {
    let mut content = store.capture(DEFAULT_CAPTURE_MEMORY_BYTES);
    let mut buffer = [0_u8; 16 * 1024];
    let mut quota_error = None;
    let mut text = StreamText::default();
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if quota_error.is_some() {
            continue;
        }
        if let Err(error) = content.append(&buffer[..read]).await {
            quota_error = Some(error);
            continue;
        }
        text.observe(&buffer[..read]);
    }
    let content = if quota_error.is_none() {
        Some(content.finish().await?)
    } else {
        None
    };
    Ok(Capture {
        content,
        quota_error,
        text,
    })
}

async fn build_response(
    request_id: u64,
    stop: Stop,
    mut stdout: Capture,
    mut stderr: Capture,
    elapsed_millis: u64,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    if let Some(error) = stdout.quota_error.take().or(stderr.quota_error.take()) {
        return Err(error.into());
    }
    let failed = !matches!(stop, Stop::Exited(ProcessExit { success: true, .. }));
    let limit = presentation_limit(program);
    let (stdout_budget, stderr_budget) = if failed {
        (limit / 8, limit / 2)
    } else {
        (limit / 2, limit / 8)
    };
    let finalizing = matches!(stop, Stop::TimedOut | Stop::Cancelled);
    let (stdout, stderr, stdout_projection, stderr_projection) = if finalizing {
        // Cancellation and deadline finalization must still produce typed
        // termination evidence. The fixed compute pool bounds this cleanup;
        // acquiring through the cancelled program would reject it outright.
        program
            .compute_pool()
            .run(move || project_captures(stdout, stderr, stdout_budget, stderr_budget))
            .await??
    } else {
        let _compute = program.acquire(PermitKind::Compute).await?;
        program
            .compute_pool()
            .run(move || project_captures(stdout, stderr, stdout_budget, stderr_budget))
            .await??
    };
    let stdout_retained = stdout_projection.reduced
        || stdout_projection.normalized
        || stdout_projection.text.is_none() && !stdout.content()?.is_empty();
    let stderr_retained = stderr_projection.reduced
        || stderr_projection.normalized
        || stderr_projection.text.is_none() && !stderr.content()?.is_empty();
    let result_flags = flags(
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

    let mut captures =
        Vec::with_capacity(usize::from(stdout_retained) + usize::from(stderr_retained));
    if let Some(capture) = stdout.into_retained(stdout_retained) {
        captures.push(capture);
    }
    if let Some(capture) = stderr.into_retained(stderr_retained) {
        captures.push(capture);
    }
    let aliases = if captures.is_empty() {
        Vec::new()
    } else {
        let _compute = program.acquire(PermitKind::Compute).await?;
        let store = Arc::clone(program.store());
        program
            .compute_pool()
            .run(move || store.retain_captures(captures))
            .await??
    };
    let mut aliases = aliases.into_iter();
    let stdout_reference = if stdout_retained {
        Some(aliases.next().ok_or(StoreError::Invariant)?)
    } else {
        None
    };
    let stderr_reference = if stderr_retained {
        Some(aliases.next().ok_or(StoreError::Invariant)?)
    } else {
        None
    };
    if aliases.next().is_some() {
        return Err(StoreError::Invariant.into());
    }
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

fn project_captures(
    stdout: Capture,
    stderr: Capture,
    stdout_budget: usize,
    stderr_budget: usize,
) -> Result<(Capture, Capture, Projection, Projection), StoreError> {
    let stdout_projection = stdout.project(stdout_budget)?;
    let stderr_projection = stderr.project(stderr_budget)?;
    Ok((stdout, stderr, stdout_projection, stderr_projection))
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
    let response = match stop {
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
    let line_reduction = collapse_repeated_lines(&normalized_text);
    let line_reduced = line_reduction.reduced();
    let block_reduction = collapse_repeated_blocks(line_reduction.text());
    let reduced = line_reduced || block_reduction.reduced();
    let normalized_text = block_reduction.into_text();
    if normalized_text.len() <= limit {
        return Projection {
            text: (!normalized_text.is_empty()).then_some(normalized_text),
            reduced,
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

fn project_sampled(
    head: &[u8],
    head_next: Option<u8>,
    tail: &[u8],
    limit: usize,
    text: &StreamText,
) -> Projection {
    if !text.is_text() {
        return Projection {
            text: None,
            reduced: true,
            normalized: false,
        };
    }
    if limit < 8 {
        return Projection {
            text: None,
            reduced: true,
            normalized: text.normalized,
        };
    }
    let head = complete_utf8_head(head);
    let tail = complete_utf8_tail(tail);
    let normalized_head = normalize_head(head, head_next, text.normalized);
    let normalized_tail = normalize(tail, text.normalized);
    let separator = "\n...\n";
    let content_limit = limit.saturating_sub(separator.len());
    let head_end = floor_boundary(&normalized_head, content_limit / 2);
    let tail_bytes = content_limit.saturating_sub(head_end);
    let tail_start = ceil_boundary(
        &normalized_tail,
        normalized_tail.len().saturating_sub(tail_bytes),
    );
    Projection {
        text: Some(format!(
            "{}{}{}",
            &normalized_head[..head_end],
            separator,
            &normalized_tail[tail_start..]
        )),
        reduced: true,
        normalized: text.normalized,
    }
}

fn complete_utf8_head(bytes: &[u8]) -> &str {
    let mut end = bytes.len();
    while end > 0 {
        if let Ok(text) = std::str::from_utf8(&bytes[..end]) {
            return text;
        }
        end -= 1;
    }
    ""
}

fn complete_utf8_tail(bytes: &[u8]) -> &str {
    for start in 0..bytes.len().min(4) {
        if let Ok(text) = std::str::from_utf8(&bytes[start..]) {
            return text;
        }
    }
    ""
}

fn normalize(text: &str, normalized: bool) -> String {
    if normalized {
        text.replace("\r\n", "\n")
    } else {
        text.to_owned()
    }
}

fn normalize_head(text: &str, next: Option<u8>, normalized: bool) -> String {
    let mut text = normalize(text, normalized);
    if normalized && text.ends_with('\r') && next == Some(b'\n') {
        text.pop();
        text.push('\n');
    }
    text
}

struct StreamText {
    valid: bool,
    pending: Vec<u8>,
    normalized: bool,
    previous_cr: bool,
}

impl Default for StreamText {
    fn default() -> Self {
        Self {
            valid: true,
            pending: Vec::with_capacity(3),
            normalized: false,
            previous_cr: false,
        }
    }
}

impl StreamText {
    fn observe(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.previous_cr && bytes[0] == b'\n' || bytes.windows(2).any(|pair| pair == b"\r\n") {
            self.normalized = true;
        }
        self.previous_cr = bytes.last() == Some(&b'\r');
        if !self.valid {
            return;
        }
        if self.pending.is_empty() {
            self.validate(bytes);
        } else {
            let mut combined = std::mem::take(&mut self.pending);
            combined.extend_from_slice(bytes);
            self.validate(&combined);
        }
    }

    fn validate(&mut self, bytes: &[u8]) {
        match std::str::from_utf8(bytes) {
            Ok(_) => self.pending.clear(),
            Err(error) if error.error_len().is_none() => {
                self.pending.clear();
                self.pending
                    .extend_from_slice(&bytes[error.valid_up_to()..]);
                if self.pending.len() > 3 {
                    self.valid = false;
                    self.pending.clear();
                }
            }
            Err(_) => {
                self.valid = false;
                self.pending.clear();
            }
        }
    }

    fn is_text(&self) -> bool {
        self.valid && self.pending.is_empty()
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

fn flags(stdout_projection: &Projection, stderr_projection: &Projection, retained: bool) -> u32 {
    let reduced = stdout_projection.reduced || stderr_projection.reduced;
    (if reduced { RESULT_REDUCED } else { 0 })
        | (if reduced { RESULT_TRUNCATED } else { 0 })
        | (if stdout_projection.normalized || stderr_projection.normalized {
            RESULT_NORMALIZED_TEXT
        } else {
            0
        })
        | (if retained { RESULT_RETAINED } else { 0 })
}

async fn stdin_bytes(
    source: &InputSource,
    program: &Program,
) -> Result<Option<Arc<[u8]>>, OperationError> {
    let bytes = match source {
        InputSource::None => return Ok(None),
        InputSource::Inline(value) => Arc::<[u8]>::from(value.as_bytes()),
        InputSource::Reference(reference) => {
            let lease = program.store().get(*reference)?;
            lease.read_all(MAX_STDIN_BYTES as u64).await?
        }
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
    use super::{StreamText, ceil_boundary, floor_boundary, project, project_sampled};

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

    #[test]
    fn projection_collapses_only_byte_saving_repeated_lines() {
        let repeated = project("alpha\n".repeat(5).as_bytes(), 1_024);
        assert_eq!(repeated.text.as_deref(), Some("alpha\n×5\n"));
        assert!(repeated.reduced);
        assert!(!repeated.normalized);

        let too_short_to_save = project(b"x\nx\n", 1_024);
        assert_eq!(too_short_to_save.text.as_deref(), Some("x\nx\n"));
        assert!(!too_short_to_save.reduced);

        let normalized = project("same\r\n".repeat(4).as_bytes(), 1_024);
        assert_eq!(normalized.text.as_deref(), Some("same\n×4\n"));
        assert!(normalized.reduced);
        assert!(normalized.normalized);
    }

    #[test]
    fn projection_collapses_repeated_blocks_after_line_reduction() {
        let block = "compile crate-a\nlink crate-a\n".repeat(6);
        let projection = project(block.as_bytes(), 1_024);
        assert_eq!(
            projection.text.as_deref(),
            Some("compile crate-a\nlink crate-a\n×6#2\n")
        );
        assert!(projection.reduced);

        let no_saving = project(b"a\nb\na\nb\n", 1_024);
        assert_eq!(no_saving.text.as_deref(), Some("a\nb\na\nb\n"));
        assert!(!no_saving.reduced);
    }

    #[test]
    fn sampled_projection_tracks_split_utf8_invalid_middle_and_crlf_boundaries() {
        let mut utf8 = StreamText::default();
        utf8.observe(b"h\xe2");
        utf8.observe(b"\x82\xac!");
        let projected = project_sampled(b"h\xe2", Some(0x82), b"\x82\xac!", 64, &utf8);
        assert_eq!(projected.text.as_deref(), Some("h\n...\n!"));
        assert!(projected.reduced);

        let mut invalid = StreamText::default();
        invalid.observe(b"head");
        invalid.observe(&[0xff]);
        invalid.observe(b"tail");
        assert!(
            project_sampled(b"head", None, b"tail", 64, &invalid)
                .text
                .is_none()
        );

        let mut crlf = StreamText::default();
        crlf.observe(b"head\r");
        crlf.observe(b"\ntail");
        let projected = project_sampled(b"head\r", Some(b'\n'), b"tail", 64, &crlf);
        assert!(projected.normalized);
        assert!(!projected.text.expect("text projection").contains('\r'));
    }
}
