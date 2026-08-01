use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use ash_engine::Parallelism;
use ash_protocol::ason::Limits;
use ash_protocol::frame::FrameCodec;
use ash_protocol::handshake::{HandshakeRequest, ServerHandshake};
use ash_protocol::request::{Arguments, Request};
use ash_protocol::response::FinalResponse;
use tokio::io::{AsyncWriteExt, stdin, stdout};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;

use crate::cli_error::CliError;
use crate::execution::{ExecutionSession, capacity_exceeded, invalid_request};

pub async fn run() -> Result<(), CliError> {
    let bootstrap_codec = FrameCodec::default();
    let limits = Limits {
        max_bytes: bootstrap_codec.max_payload(),
        ..Limits::default()
    };
    let mut stdin = stdin();
    let mut stdout = stdout();

    let document = bootstrap_codec
        .read_document(&mut stdin, &limits)
        .await?
        .ok_or(CliError::MissingHandshake)?;
    let request = HandshakeRequest::decode(&document)?;
    let parallelism = Parallelism::detected();
    let response = ServerHandshake {
        operation_mask: ExecutionSession::operation_mask(),
        capability_mask: ExecutionSession::capability_mask(),
        ..ServerHandshake::default()
    }
    .negotiate(&request, 1)?;
    let execution = Arc::new(ExecutionSession::open(
        response.session_id(),
        request.workspace(),
        u64::from(response.output_bytes()),
        parallelism,
        response.capability_mask(),
    )?);
    let response_document = response.encode()?;
    bootstrap_codec
        .write_document(&mut stdout, &response_document)
        .await?;
    stdout.flush().await?;

    let session_codec = FrameCodec::new(response.frame_bytes() as usize)?;
    let session_limits = Limits {
        max_bytes: session_codec.max_payload(),
        ..Limits::default()
    };
    let max_in_flight = parallelism.io_workers().get().saturating_mul(4).max(1);
    let max_pending = max_in_flight.saturating_mul(4).max(max_in_flight);
    let slots = Arc::new(Semaphore::new(max_in_flight));
    let mut tasks = JoinSet::new();
    let mut buffered = BTreeMap::new();
    let mut next_sequence = 0_u64;
    let mut next_output = 0_u64;
    let mut input_open = true;
    let mut fatal = None;

    loop {
        if fatal.is_none() {
            flush_ready(&session_codec, &mut stdout, &mut buffered, &mut next_output).await?;
        }
        if !input_open && tasks.is_empty() {
            break;
        }
        let can_read = input_open
            && next_sequence.saturating_sub(next_output)
                < u64::try_from(max_pending).unwrap_or(u64::MAX);

        tokio::select! {
            document = session_codec.read_document(&mut stdin, &session_limits), if can_read => {
                match document {
                    Ok(Some(document)) => {
                        let sequence = next_sequence;
                        next_sequence = next_sequence.checked_add(1).ok_or_else(|| {
                            CliError::Io(io::Error::other("RPC request sequence exhausted"))
                        })?;
                        match Request::decode(&document) {
                            Ok(request) => {
                                if let Arguments::Cancel(arguments) = request.arguments() {
                                    let response = execution.cancel(&request, *arguments)?;
                                    buffered.insert(sequence, response);
                                } else if let Ok(slot) = Arc::clone(&slots).try_acquire_owned() {
                                    let task_execution = Arc::clone(&execution);
                                    let (registered, registration) = oneshot::channel();
                                    tasks.spawn(async move {
                                        let response = task_execution
                                            .execute_registered(request, registered)
                                            .await;
                                        drop(slot);
                                        (sequence, response)
                                    });
                                    if registration.await.is_err() {
                                        let _ = execution.close();
                                        fatal = Some(CliError::Io(io::Error::other(
                                            "request task ended before registration",
                                        )));
                                        input_open = false;
                                    }
                                } else {
                                    buffered.insert(sequence, capacity_exceeded(request.id())?);
                                }
                            }
                            Err(error) => match Request::id_hint(&document) {
                                Some(request_id) => {
                                    buffered.insert(sequence, invalid_request(request_id)?);
                                }
                                None => {
                                    let _ = execution.close();
                                    fatal = Some(error.into());
                                    input_open = false;
                                }
                            },
                        }
                    }
                    Ok(None) => input_open = false,
                    Err(error) => {
                        let _ = execution.close();
                        fatal = Some(error.into());
                        input_open = false;
                    }
                }
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok((sequence, Ok(response)))) if fatal.is_none() => {
                        buffered.insert(sequence, response);
                    }
                    Some(Ok((_sequence, Ok(_response)))) => {}
                    Some(Ok((_sequence, Err(error)))) => {
                        let _ = execution.close();
                        fatal = Some(error);
                        input_open = false;
                    }
                    Some(Err(error)) => {
                        let _ = execution.close();
                        fatal = Some(error.into());
                        input_open = false;
                    }
                    None => {}
                }
            }
        }
    }

    execution.close()?;
    if let Some(error) = fatal {
        Err(error)
    } else {
        flush_ready(&session_codec, &mut stdout, &mut buffered, &mut next_output).await?;
        Ok(())
    }
}

async fn flush_ready<W>(
    codec: &FrameCodec,
    writer: &mut W,
    buffered: &mut BTreeMap<u64, FinalResponse>,
    next_output: &mut u64,
) -> Result<(), CliError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut wrote = false;
    while let Some(response) = buffered.remove(next_output) {
        codec.write_document(writer, &response.encode()?).await?;
        *next_output = next_output
            .checked_add(1)
            .ok_or_else(|| CliError::Io(io::Error::other("RPC output sequence exhausted")))?;
        wrote = true;
    }
    if wrote {
        writer.flush().await?;
    }
    Ok(())
}
