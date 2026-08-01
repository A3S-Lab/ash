use std::io::Write;
use std::process::{Command, Output, Stdio};

use ash_protocol::ason::decode;
use ash_protocol::handshake::{HandshakePreferences, HandshakeRequest, HandshakeResponse};

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
    assert_eq!(response.operation_mask(), 0);
}
