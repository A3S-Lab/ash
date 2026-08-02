use std::error::Error;
use std::io;
use std::time::{Duration, Instant};

use ash_cli::{CliError, serve_rpc};
use ash_engine::Parallelism;
use ash_protocol::Operation;
use ash_protocol::ason::{Document, Limits};
use ash_protocol::frame::FrameCodec;
use ash_protocol::handshake::{HandshakePreferences, HandshakeRequest, ServerHandshake};
use ash_protocol::request::{
    Arguments, Budget, CancelArgs, MAX_REQUEST_RECORDS, MAX_REQUEST_TOKENS, Request,
};
use ash_protocol::response::{CancelResult, CancellationState, FinalResponse, ResultData};
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf, duplex, split};
use tokio::task::JoinHandle;

use super::{RuntimeConfig, ScenarioReport, require_stable_output, runtime_run, sha256_hex};

const DISPATCH_REQUEST_ID: u64 = 2;
const DISPATCH_TARGET_ID: u64 = 999;
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(10);
const DUPLEX_BYTES: usize = 64 * 1024;

pub(super) async fn measure_rpc_dispatch_scenario(
    workspace: &str,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let (_, normalized_request, expected_response) = protocol_documents(".")?;
    let request_bytes = normalized_request.encode().into_bytes();
    let response_bytes = expected_response.encode().into_bytes();
    let work_bytes = u64::try_from(
        request_bytes
            .len()
            .checked_add(response_bytes.len())
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or_else(|| io::Error::other("RPC dispatch byte count overflow"))?,
    )?;
    let input_sha256 = sha256_hex(
        format!(
            "transport=framed-duplex\nrequest={}\n",
            String::from_utf8(request_bytes)?
        )
        .as_bytes(),
    );
    let mut runs = Vec::with_capacity(config.worker_counts.len());
    let mut expected_output = None;
    let mut baseline = None;

    for &workers in &config.worker_counts {
        let parallelism = Parallelism::for_available_cpus(workers);
        let (warm_output, observations) =
            execute_dispatch_samples(workspace, parallelism, config.samples).await?;
        require_stable_output(&mut expected_output, &warm_output)?;
        let output = expected_output
            .as_ref()
            .ok_or_else(|| io::Error::other("RPC dispatch emitted no output"))?;
        runs.push(runtime_run(
            parallelism,
            observations,
            &mut baseline,
            1,
            u128::from(work_bytes),
            output,
        ));
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing RPC dispatch output"))?;
    Ok(ScenarioReport {
        id: "rpc-warm-dispatch",
        work_items: 1,
        work_bytes,
        input_sha256,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_dispatch_samples(
    workspace: &str,
    parallelism: Parallelism,
    samples: usize,
) -> Result<(Vec<u8>, Vec<u128>), Box<dyn Error>> {
    let mut session = WarmRpcSession::open(workspace, parallelism).await?;
    let measurements: Result<_, Box<dyn Error>> = async {
        let (warm_output, _) = session.exchange().await?;
        let mut observations = Vec::with_capacity(samples);
        for _ in 0..samples {
            let (output, elapsed) = session.exchange().await?;
            if output != warm_output {
                return Err(
                    io::Error::other("warm RPC responses changed within one session").into(),
                );
            }
            observations.push(elapsed);
        }
        Ok((warm_output, observations))
    }
    .await;
    let close_result = session.close().await;
    match measurements {
        Ok(measurements) => {
            close_result?;
            Ok(measurements)
        }
        Err(error) => {
            let _ = close_result;
            Err(error)
        }
    }
}

struct WarmRpcSession {
    codec: FrameCodec,
    limits: Limits,
    reader: ReadHalf<DuplexStream>,
    writer: WriteHalf<DuplexStream>,
    request: Document,
    expected_response: Document,
    server: JoinHandle<Result<(), CliError>>,
}

impl WarmRpcSession {
    async fn open(workspace: &str, parallelism: Parallelism) -> Result<Self, Box<dyn Error>> {
        let (client, server) = duplex(DUPLEX_BYTES);
        let (mut reader, mut writer) = split(client);
        let (server_reader, server_writer) = split(server);
        let server = tokio::spawn(serve_rpc(server_reader, server_writer, parallelism));
        let bootstrap_codec = FrameCodec::default();
        let bootstrap_limits = Limits {
            max_bytes: bootstrap_codec.max_payload(),
            ..Limits::default()
        };
        let (handshake, request, expected_response) = protocol_documents(workspace)?;
        bootstrap_codec
            .write_document(&mut writer, &handshake.encode()?)
            .await?;
        writer.flush().await?;
        let actual_handshake = bootstrap_codec
            .read_document(&mut reader, &bootstrap_limits)
            .await?
            .ok_or_else(|| io::Error::other("RPC server closed during handshake"))?;
        let expected_handshake = ServerHandshake {
            operation_mask: Operation::Cancel.mask(),
            capability_mask: 0,
            ..ServerHandshake::default()
        }
        .negotiate(&handshake, 1)?;
        if actual_handshake != expected_handshake.encode()? {
            return Err(io::Error::other("RPC server negotiated an unexpected session").into());
        }
        let codec = FrameCodec::new(expected_handshake.frame_bytes() as usize)?;
        let limits = Limits {
            max_bytes: codec.max_payload(),
            ..Limits::default()
        };
        Ok(Self {
            codec,
            limits,
            reader,
            writer,
            request,
            expected_response,
            server,
        })
    }

    async fn exchange(&mut self) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
        let started = Instant::now();
        self.codec
            .write_document(&mut self.writer, &self.request)
            .await?;
        self.writer.flush().await?;
        let actual = self
            .codec
            .read_document(&mut self.reader, &self.limits)
            .await?
            .ok_or_else(|| io::Error::other("RPC server closed before its response"))?;
        let elapsed = started.elapsed().as_nanos().max(1);
        if actual != self.expected_response {
            return Err(io::Error::other("RPC dispatch response changed").into());
        }
        Ok((actual.encode().into_bytes(), elapsed))
    }

    async fn close(mut self) -> Result<(), Box<dyn Error>> {
        let shutdown = self.writer.shutdown().await;
        drop(self.writer);
        drop(self.reader);
        let mut server = self.server;
        let joined = match tokio::time::timeout(DISPATCH_TIMEOUT, &mut server).await {
            Ok(joined) => joined,
            Err(_) => {
                server.abort();
                let _ = server.await;
                return Err(io::Error::other("warm RPC server shutdown exceeded its bound").into());
            }
        };
        shutdown?;
        joined??;
        Ok(())
    }
}

fn protocol_documents(
    workspace: &str,
) -> Result<(HandshakeRequest, Document, Document), Box<dyn Error>> {
    let handshake = HandshakeRequest::new(
        1,
        workspace,
        "runtime-dispatch",
        HandshakePreferences {
            operation_mask: Operation::Cancel.mask(),
            capability_mask: 0,
            ..HandshakePreferences::default()
        },
    )?;
    let request = Request::new(
        DISPATCH_REQUEST_ID,
        Arguments::Cancel(CancelArgs::new(DISPATCH_TARGET_ID)?),
        Budget::new(MAX_REQUEST_TOKENS, MAX_REQUEST_RECORDS, 30_000)?,
    )?;
    let expected_response = FinalResponse::success(
        DISPATCH_REQUEST_ID,
        vec![],
        ResultData::Cancel(CancelResult {
            target_id: DISPATCH_TARGET_ID,
            state: CancellationState::NotActive,
        }),
        0,
        None,
    )?
    .encode()?;
    Ok((handshake, request.encode()?, expected_response))
}
