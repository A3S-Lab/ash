use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use ash_engine::Parallelism;
use ash_protocol::request::{
    Arguments, Budget, CancelArgs, MAX_REQUEST_RECORDS, MAX_REQUEST_TOKENS, Request,
};
use ash_protocol::response::{CancelResult, CancellationState, FinalResponse, ResultData};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

#[cfg(test)]
use super::hex;
use super::{
    ProcessFixture, RuntimeConfig, ScenarioReport, require_stable_output, runtime_run, sha256_hex,
};

const COLD_REQUEST_ID: u64 = 1;
const COLD_TARGET_ID: u64 = 999;
const COLD_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct ColdFixture {
    executable: PathBuf,
    arguments: Vec<OsString>,
    binary_sha256: String,
}

pub(super) fn prepare_fixture(
    ash_binary: Option<&Path>,
    process_fixture: &ProcessFixture,
) -> Result<ColdFixture, Box<dyn Error>> {
    #[cfg(test)]
    {
        let _ = ash_binary;
        let (request, expected) = protocol_documents()?;
        let executable = process_fixture
            .directory
            .path()
            .join(&process_fixture.executable)
            .canonicalize()?;
        let binary_sha256 = sha256_hex(&fs::read(&executable)?);
        Ok(ColdFixture {
            executable,
            arguments: vec![
                OsString::from("respond"),
                OsString::from(hex(request.encode().as_bytes())),
                OsString::from(hex(expected.encode().as_bytes())),
            ],
            binary_sha256,
        })
    }

    #[cfg(not(test))]
    {
        let _ = process_fixture;
        let executable = resolve_ash_binary(ash_binary)?;
        let binary_sha256 = sha256_hex(&fs::read(&executable)?);
        Ok(ColdFixture {
            executable,
            arguments: vec![OsString::from("run")],
            binary_sha256,
        })
    }
}

pub(super) async fn measure_cold_startup_scenario(
    fixture: &ColdFixture,
    workspace: &str,
    config: &RuntimeConfig,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let (request, expected_response) = protocol_documents()?;
    let request_bytes = request.encode().into_bytes();
    let response_bytes = expected_response.encode().into_bytes();
    let work_bytes = u64::try_from(
        request_bytes
            .len()
            .checked_add(response_bytes.len())
            .ok_or_else(|| io::Error::other("cold startup byte count overflow"))?,
    )?;
    let arguments = fixture
        .arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\0");
    let input_sha256 = sha256_hex(
        format!(
            "binary_sha256={}\narguments={}\nrequest={}\n",
            fixture.binary_sha256,
            arguments,
            String::from_utf8(request_bytes.clone())?
        )
        .as_bytes(),
    );

    let (warm_output, _) =
        execute_once(fixture, workspace, &request_bytes, &response_bytes).await?;
    let mut expected_output = None;
    require_stable_output(&mut expected_output, &warm_output)?;
    let mut observations = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let (output, elapsed) =
            execute_once(fixture, workspace, &request_bytes, &response_bytes).await?;
        require_stable_output(&mut expected_output, &output)?;
        observations.push(elapsed);
    }

    let output = expected_output.ok_or_else(|| io::Error::other("missing cold startup output"))?;
    let parallelism = Parallelism::detected();
    let mut baseline = None;
    let mut run = runtime_run(
        parallelism,
        observations,
        &mut baseline,
        1,
        u128::from(work_bytes),
        &output,
    );
    run.speedup_basis_points = None;
    run.parallel_efficiency_basis_points = None;
    let runs = vec![run];
    Ok(ScenarioReport {
        id: "cli-cold-startup",
        work_items: 1,
        work_bytes,
        input_sha256,
        output_bytes: output.len(),
        output_sha256: sha256_hex(&output),
        runs,
    })
}

async fn execute_once(
    fixture: &ColdFixture,
    workspace: &str,
    request: &[u8],
    expected_response: &[u8],
) -> Result<(Vec<u8>, u128), Box<dyn Error>> {
    let mut command = Command::new(&fixture.executable);
    command
        .args(&fixture.arguments)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let started = Instant::now();
    let mut child = command.spawn()?;
    let streams = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (Some(mut stdin), Some(stdout), Some(stderr)) = streams else {
        terminate(&mut child).await;
        return Err(io::Error::other("cold child pipes were not configured").into());
    };
    let mut stdout_task = tokio::spawn(read_all(stdout));
    let mut stderr_task = tokio::spawn(read_all(stderr));

    if let Err(error) = async {
        stdin.write_all(request).await?;
        stdin.shutdown().await
    }
    .await
    {
        terminate(&mut child).await;
        finish_reader(&mut stdout_task).await;
        finish_reader(&mut stderr_task).await;
        return Err(error.into());
    }
    drop(stdin);

    let status = match tokio::time::timeout(COLD_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            terminate(&mut child).await;
            finish_reader(&mut stdout_task).await;
            finish_reader(&mut stderr_task).await;
            return Err(io::Error::other("cold ash process exceeded its shutdown bound").into());
        }
    };
    let stdout = reader_result(&mut stdout_task, "stdout").await;
    let stderr = reader_result(&mut stderr_task, "stderr").await;
    let stdout = stdout?;
    let stderr = stderr?;
    let elapsed = started.elapsed().as_nanos().max(1);
    if !status.success() {
        return Err(
            io::Error::other(format!("cold ash process exited unsuccessfully: {status}")).into(),
        );
    }
    if !stderr.is_empty() {
        return Err(io::Error::other("cold ash process emitted stderr").into());
    }
    if stdout != expected_response {
        return Err(io::Error::other("cold ash process emitted non-canonical evidence").into());
    }
    Ok((stdout, elapsed))
}

async fn read_all(mut reader: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, io::Error> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output).await?;
    Ok(output)
}

async fn reader_result(
    task: &mut JoinHandle<Result<Vec<u8>, io::Error>>,
    stream: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    match tokio::time::timeout(COLD_TIMEOUT, &mut *task).await {
        Ok(result) => Ok(result??),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(io::Error::other(format!("cold ash {stream} drain exceeded its bound")).into())
        }
    }
}

async fn finish_reader(task: &mut JoinHandle<Result<Vec<u8>, io::Error>>) {
    if tokio::time::timeout(COLD_TIMEOUT, &mut *task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

async fn terminate(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(COLD_TIMEOUT, child.wait()).await;
}

fn protocol_documents()
-> Result<(ash_protocol::ason::Document, ash_protocol::ason::Document), Box<dyn Error>> {
    let request = Request::new(
        COLD_REQUEST_ID,
        Arguments::Cancel(CancelArgs::new(COLD_TARGET_ID)?),
        Budget::new(MAX_REQUEST_TOKENS, MAX_REQUEST_RECORDS, 30_000)?,
    )?
    .encode()?;
    let response = FinalResponse::success(
        COLD_REQUEST_ID,
        vec![],
        ResultData::Cancel(CancelResult {
            target_id: COLD_TARGET_ID,
            state: CancellationState::NotActive,
        }),
        0,
        None,
    )?
    .encode()?;
    Ok((request, response))
}

#[cfg(not(test))]
fn resolve_ash_binary(override_path: Option<&Path>) -> Result<PathBuf, io::Error> {
    let path = if let Some(path) = override_path {
        path.to_path_buf()
    } else {
        let current = std::env::current_exe()?;
        let directory = current
            .parent()
            .ok_or_else(|| io::Error::other("benchmark executable has no parent directory"))?;
        let profile = if directory.file_name().is_some_and(|name| name == "deps") {
            directory
                .parent()
                .ok_or_else(|| io::Error::other("benchmark deps directory has no parent"))?
        } else {
            directory
        };
        profile.join(format!("ash{}", std::env::consts::EXE_SUFFIX))
    };
    let path = path.canonicalize().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot resolve ash binary at {} (build it with `cargo build --release --locked -p a3s-ash` or pass `--runtime <path>`): {error}",
                path.display()
            ),
        )
    })?;
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("ash binary is not a file: {}", path.display()),
        ));
    }
    Ok(path)
}
