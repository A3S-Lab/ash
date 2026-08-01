use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ash_ops::PortableOperations;
use ash_protocol::Operation;
use ash_protocol::ason::decode;
use ash_protocol::handshake::{HandshakePreferences, HandshakeRequest, HandshakeResponse};
use ash_protocol::request::{
    Arguments, Budget, CancelArgs, ExecArgs, InputSource, ReadArgs, ReadMode, Request,
};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ash-cli-{}-{id}", std::process::id()));
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn compile_rpc_helper(directory: &TestDirectory) -> String {
    let bin_directory = directory.0.join("bin");
    fs::create_dir(&bin_directory).expect("create bin directory");
    let source = directory.0.join("rpc-helper.rs");
    fs::write(
        &source,
        r#"
use std::{env, fs, process, thread, time::{Duration, Instant}};

fn main() {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        Some("sleep") => {
            let millis = arguments.next().expect("milliseconds").parse().expect("number");
            thread::sleep(Duration::from_millis(millis));
        }
        Some("gate") => {
            let own = arguments.next().expect("own marker");
            let peer = arguments.next().expect("peer marker");
            fs::write(own, b"ready").expect("write marker");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !std::path::Path::new(&peer).is_file() {
                if Instant::now() >= deadline {
                    process::exit(9);
                }
                thread::sleep(Duration::from_millis(10));
            }
            println!("ready");
        }
        _ => process::exit(2),
    }
}
"#,
    )
    .expect("write helper source");
    let executable_name = if cfg!(windows) {
        "rpc-helper.exe"
    } else {
        "rpc-helper"
    };
    let status = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(bin_directory.join(executable_name))
        .status()
        .expect("run rustc");
    assert!(status.success(), "compile RPC helper");
    format!("bin/{executable_name}")
}

fn run(arguments: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ash"))
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ash");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("write child stdin");
    child.wait_with_output().expect("wait for ash")
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(payload);
    framed
}

fn split_frames(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    while !bytes.is_empty() {
        assert!(bytes.len() >= 4, "truncated frame prefix");
        let length = u32::from_be_bytes(bytes[..4].try_into().expect("prefix")) as usize;
        assert!(bytes.len() >= length + 4, "truncated frame payload");
        frames.push(&bytes[4..length + 4]);
        bytes = &bytes[length + 4..];
    }
    frames
}

#[test]
fn version_is_stable_and_script_friendly() {
    let output = run(&["--version"], b"");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8"),
        "ash 0.1.0\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn ason_command_canonicalizes_stdin_without_decoration() {
    let output = run(&["ason"], b"v:\"safe\"\n");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"v:safe\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn ason_command_returns_a_machine_only_error() {
    let output = run(&["ason"], b"invalid");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"s:1\ne{c}:\n4\n");
}

#[test]
fn run_executes_one_bare_typed_request() {
    let request = Request::new(
        31,
        Arguments::Read(
            ReadArgs::new(vec!["Cargo.toml".to_owned()], ReadMode::Bytes, 0, 32).expect("read"),
        ),
        Budget::new(1024, 8, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let output = run(&["run"], request.as_bytes());
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let response = std::str::from_utf8(&output.stdout).expect("UTF-8 response");
    assert_eq!(decode(response).expect("ASON response").encode(), response);
    assert!(response.starts_with("t:3\ni:31\ns:0\n"));
    assert!(response.contains("Cargo.toml"));
    assert!(response.contains("d[1]{p,o,n,h,t,r}:"));
}

#[test]
fn run_executes_a_direct_process_with_typed_output() {
    let request = Request::new(
        32,
        Arguments::Exec(
            ExecArgs::new(
                "rustc",
                vec!["--version".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        Budget::new(4_096, 16, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let output = run(&["run"], request.as_bytes());
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let response = std::str::from_utf8(&output.stdout).expect("UTF-8 response");
    assert_eq!(decode(response).expect("ASON response").encode(), response);
    assert!(response.starts_with("t:3\ni:32\ns:0\n"));
    assert!(response.contains("d{k,c,ms,o,e,ro,re}:"));
    assert!(response.contains("rustc "));
}

#[test]
fn usage_and_missing_handshake_are_machine_only() {
    let usage = run(&[], b"");
    assert_eq!(usage.status.code(), Some(2));
    assert_eq!(usage.stderr, b"s:1\ne{c}:\n1\n");

    let handshake = run(&["rpc"], b"");
    assert_eq!(handshake.status.code(), Some(4));
    assert_eq!(handshake.stderr, b"s:1\ne{c}:\n7\n");
}

#[test]
fn rpc_rejects_noncanonical_framed_ason() {
    let output = run(&["rpc"], &frame(b"v:\"safe\"\n"));
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"s:1\ne{c}:\n5\n");
}

#[test]
fn rpc_preserves_utf8_and_ason_diagnostic_classes() {
    let invalid_utf8 = run(&["rpc"], &frame(&[0xff, b'\n']));
    assert_eq!(invalid_utf8.status.code(), Some(4));
    assert!(invalid_utf8.stdout.is_empty());
    assert_eq!(invalid_utf8.stderr, b"s:1\ne{c}:\n3\n");

    let invalid_ason = run(&["rpc"], &frame(b"invalid\n"));
    assert_eq!(invalid_ason.status.code(), Some(4));
    assert!(invalid_ason.stdout.is_empty());
    assert_eq!(invalid_ason.stderr, b"s:1\ne{c}:\n4\n");
}

#[test]
fn rpc_negotiates_one_canonical_framed_session() {
    let request = HandshakeRequest::new(
        17,
        ".",
        "integration-17",
        HandshakePreferences {
            operation_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("request")
    .encode()
    .expect("encode request")
    .encode();
    let output = run(&["rpc"], &frame(request.as_bytes()));
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert!(output.stderr.is_empty());
    assert!(output.stdout.len() >= 4);
    let declared = u32::from_be_bytes(output.stdout[..4].try_into().expect("prefix")) as usize;
    assert_eq!(output.stdout.len(), declared + 4);
    let payload = std::str::from_utf8(&output.stdout[4..]).expect("response UTF-8");
    let response = HandshakeResponse::decode(&decode(payload).expect("response ASON"))
        .expect("response schema");
    assert_eq!(response.session_id(), 1);
    assert_eq!(response.nonce(), "integration-17");
    assert_eq!(
        response.operation_mask(),
        PortableOperations::operation_mask() | Operation::Cancel.mask()
    );
}

#[test]
fn rpc_executes_a_request_after_the_handshake() {
    let handshake = HandshakeRequest::new(
        40,
        ".",
        "integration-40",
        HandshakePreferences {
            operation_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode handshake")
    .encode();
    let request = Request::new(
        41,
        Arguments::Read(
            ReadArgs::new(vec!["Cargo.toml".to_owned()], ReadMode::Bytes, 0, 16).expect("read"),
        ),
        Budget::new(1024, 8, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode request")
    .encode();
    let mut input = frame(handshake.as_bytes());
    input.extend(frame(request.as_bytes()));
    let output = run(&["rpc"], &input);
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let frames = split_frames(&output.stdout);
    assert_eq!(frames.len(), 2);
    let result = std::str::from_utf8(frames[1]).expect("result UTF-8");
    assert_eq!(decode(result).expect("result ASON").encode(), result);
    assert!(result.starts_with("t:3\ni:41\ns:0\n"));
    assert!(result.contains("Cargo.toml"));
}

#[test]
fn rpc_executes_independent_requests_concurrently_in_input_order() {
    let directory = TestDirectory::new();
    let executable = compile_rpc_helper(&directory);
    let handshake = HandshakeRequest::new(
        50,
        directory.0.to_string_lossy(),
        "integration-50",
        HandshakePreferences {
            operation_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode handshake")
    .encode();
    let request = |id, own: &str, peer: &str| {
        Request::new(
            id,
            Arguments::Exec(
                ExecArgs::new(
                    &executable,
                    vec!["gate".to_owned(), own.to_owned(), peer.to_owned()],
                    ".",
                    vec![],
                    InputSource::None,
                    0,
                )
                .expect("exec"),
            ),
            Budget::new(1_024, 8, 10_000).expect("budget"),
        )
        .expect("request")
        .encode()
        .expect("encode")
        .encode()
    };
    let first = request(51, "first", "second");
    let second = request(52, "second", "first");
    let mut input = frame(handshake.as_bytes());
    input.extend(frame(first.as_bytes()));
    input.extend(frame(second.as_bytes()));

    let output = run(&["rpc"], &input);
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    let frames = split_frames(&output.stdout);
    assert_eq!(frames.len(), 3);
    let first = std::str::from_utf8(frames[1]).expect("first response");
    let second = std::str::from_utf8(frames[2]).expect("second response");
    assert!(first.starts_with("t:3\ni:51\ns:0\n"), "{first}");
    assert!(second.starts_with("t:3\ni:52\ns:0\n"), "{second}");
}

#[test]
fn rpc_cancel_preempts_an_active_process() {
    let directory = TestDirectory::new();
    let executable = compile_rpc_helper(&directory);
    let handshake = HandshakeRequest::new(
        60,
        directory.0.to_string_lossy(),
        "integration-60",
        HandshakePreferences {
            operation_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode handshake")
    .encode();
    let exec = Request::new(
        61,
        Arguments::Exec(
            ExecArgs::new(
                executable,
                vec!["sleep".to_owned(), "10000".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        Budget::new(1_024, 8, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let cancel = Request::new(
        62,
        Arguments::Cancel(CancelArgs::new(61).expect("cancel")),
        Budget::new(64, 1, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let mut input = frame(handshake.as_bytes());
    input.extend(frame(exec.as_bytes()));
    input.extend(frame(cancel.as_bytes()));

    let started = Instant::now();
    let output = run(&["rpc"], &input);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    let frames = split_frames(&output.stdout);
    assert_eq!(frames.len(), 3);
    let exec = std::str::from_utf8(frames[1]).expect("exec response");
    let cancel = std::str::from_utf8(frames[2]).expect("cancel response");
    assert!(exec.starts_with("t:3\ni:61\ns:7\n"), "{exec}");
    assert!(exec.contains("e{c,q,p,x,a}:\n403,0,4,~,~\n"), "{exec}");
    assert!(cancel.starts_with("t:3\ni:62\ns:0\n"), "{cancel}");
    assert!(cancel.contains("d{i,z}:\n61,1\n"), "{cancel}");
}
