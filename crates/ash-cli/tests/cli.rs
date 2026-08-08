use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ash_ops::PortableOperations;
use ash_protocol::ason::decode;
use ash_protocol::handshake::{HandshakePreferences, HandshakeRequest, HandshakeResponse};
use ash_protocol::request::{
    Arguments, BatchArgs, BatchNode, Budget, CancelArgs, ExecArgs, FsAction, FsActionKind, FsArgs,
    InputSource, PatchArgs, PatchContent, PatchEdit, ReadArgs, ReadMode, RefArgs, Request,
    SearchArgs, SnapshotArgs, SnapshotMode,
};
use ash_protocol::{Capability, Operation};
use sha2::{Digest, Sha256};

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

#[cfg(feature = "human-shell")]
fn compile_shell_helper(directory: &TestDirectory) -> String {
    let bin_directory = directory.0.join("bin");
    fs::create_dir(&bin_directory).expect("bin directory");
    let source = directory.0.join("shell-helper.rs");
    fs::write(
        &source,
        r#"
use std::{env, io, io::Write, process};

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        Some("--emit") => {
            let value = arguments.get(1).expect("emit value");
            let mut output = io::stdout().lock();
            writeln!(output, "{value}").expect("emit stdout");
            return;
        }
        Some("--copy") => {
            let mut input = io::stdin().lock();
            let mut output = io::stdout().lock();
            io::copy(&mut input, &mut output).expect("copy stdin");
            output.flush().expect("flush stdout");
            return;
        }
        Some("--exit") => {
            let code = arguments
                .get(1)
                .expect("exit status")
                .parse::<i32>()
                .expect("numeric exit status");
            process::exit(code);
        }
        Some("--ordered") => {
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            stdout.write_all(b"stdout-a\n").expect("write stdout a");
            stdout.flush().expect("flush stdout a");
            stderr.write_all(b"stderr-a\n").expect("write stderr a");
            stderr.flush().expect("flush stderr a");
            stdout.write_all(b"stdout-b\n").expect("write stdout b");
            stdout.flush().expect("flush stdout b");
            stderr.write_all(b"stderr-b\n").expect("write stderr b");
            stderr.flush().expect("flush stderr b");
            return;
        }
        _ => {}
    }
    let cwd = env::current_dir().expect("current directory");
    println!("cwd={}", cwd.file_name().expect("directory name").to_string_lossy());
    println!("token={}", env::var("ASH_NATIVE_TOKEN").expect("environment"));
    println!("arguments={}", arguments.join("|"));
    eprintln!("native-stderr");
    process::exit(23);
}
"#,
    )
    .expect("write helper source");
    let executable_name = if cfg!(windows) {
        "shell-helper.exe"
    } else {
        "shell-helper"
    };
    let status = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(bin_directory.join(executable_name))
        .status()
        .expect("run rustc");
    assert!(status.success(), "compile shell helper");
    executable_name.to_owned()
}

fn run(arguments: &[&str], input: &[u8]) -> Output {
    run_from(None, arguments, input)
}

fn run_in(directory: &TestDirectory, arguments: &[&str], input: &[u8]) -> Output {
    run_from(Some(&directory.0), arguments, input)
}

fn run_from(directory: Option<&std::path::Path>, arguments: &[&str], input: &[u8]) -> Output {
    run_from_with_env(directory, arguments, input, &[])
}

fn run_from_with_env(
    directory: Option<&std::path::Path>,
    arguments: &[&str],
    input: &[u8],
    environment: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ash"));
    command
        .args(arguments)
        .current_dir(directory.unwrap_or_else(|| std::path::Path::new(".")))
        .env_remove("ASH_PROFILE")
        .env_remove("ASH_PROMPT")
        .env_remove("ASH_HISTORY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in environment {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("spawn ash");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("write child stdin");
    child.wait_with_output().expect("wait for ash")
}

fn installed_fixture(directory: &TestDirectory) -> PathBuf {
    let prefix = directory.0.join("installed-ash");
    let version = env!("CARGO_PKG_VERSION");
    #[cfg(windows)]
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_ash"));
    let build = run(&["--build-info"], b"");
    let build = String::from_utf8(build.stdout).expect("build info");
    let target = build
        .lines()
        .find_map(|line| line.strip_prefix("t:"))
        .expect("target");
    let binary_name = if cfg!(windows) { "ash.exe" } else { "ash" };
    let version_root = prefix.join("versions").join(version);
    fs::create_dir_all(&version_root).expect("version root");
    let version_binary = version_root.join(binary_name);
    #[cfg(windows)]
    fs::copy(&executable, &version_binary).expect("version binary");
    #[cfg(unix)]
    fs::write(&version_binary, b"fixture").expect("version binary");
    let digest = Sha256::digest(fs::read(&version_binary).expect("read binary"));
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    fs::write(
        version_root.join("release.json"),
        format!(
            "{{\"schema\":1,\"product\":\"ash\",\"version\":\"{version}\",\"target\":\"{target}\",\"protocol\":\"1\",\"ason\":\"1\",\"commit\":\"{}\",\"build\":\"test\",\"binary_sha256\":\"{digest}\"}}\n",
            "a".repeat(40)
        ),
    )
    .expect("release metadata");

    #[cfg(unix)]
    let launcher = {
        use std::os::unix::fs::symlink;
        symlink(
            PathBuf::from("versions").join(version),
            prefix.join("active"),
        )
        .expect("active link");
        let bin = directory.0.join("bin");
        fs::create_dir(&bin).expect("bin directory");
        let launcher = bin.join("ash");
        symlink(prefix.join("active").join("ash"), &launcher).expect("launcher link");
        launcher
    };
    #[cfg(windows)]
    let launcher = {
        let active = prefix.join("active");
        fs::create_dir(&active).expect("active directory");
        let launcher = active.join("ash.exe");
        fs::copy(&version_binary, &launcher).expect("launcher");
        launcher
    };
    let receipt = serde_json::json!({
        "schema": 1,
        "repository": "A3S-Lab/ash",
        "version": version,
        "target": target,
        "prefix": prefix,
        "launcher": launcher,
        "path_added": false,
    });
    let mut receipt = serde_json::to_vec(&receipt).expect("receipt JSON");
    receipt.push(b'\n');
    fs::write(prefix.join("install-receipt.json"), receipt).expect("receipt");
    prefix
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

fn exchange_frame(stdin: &mut impl Write, stdout: &mut impl Read, payload: &str) -> Vec<u8> {
    stdin
        .write_all(&frame(payload.as_bytes()))
        .expect("write frame");
    stdin.flush().expect("flush frame");
    let mut prefix = [0_u8; 4];
    stdout.read_exact(&mut prefix).expect("read frame prefix");
    let mut payload = vec![0_u8; u32::from_be_bytes(prefix) as usize];
    stdout.read_exact(&mut payload).expect("read frame payload");
    payload
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
fn build_info_is_canonical_machine_metadata() {
    let output = run(&["--build-info"], b"");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let metadata = String::from_utf8(output.stdout).expect("UTF-8");
    assert_eq!(decode(&metadata).expect("ASON").encode(), metadata);
    assert!(metadata.starts_with("v:0.1.0\nt:"));
    assert!(metadata.contains("\np:1\na:1\nk:"));
    assert!(metadata.contains("\nc:"));
    assert!(metadata.ends_with('\n'));
    assert!(!metadata.contains("unsupported"));
}

#[test]
fn self_status_and_candidate_check_are_canonical() {
    let directory = TestDirectory::new();
    let prefix = installed_fixture(&directory);
    let prefix = prefix.to_str().expect("UTF-8 test path");
    let status = run(&["self", "status", "--prefix", prefix], b"");
    assert!(status.status.success(), "stderr={:?}", status.stderr);
    let status = String::from_utf8(status.stdout).expect("status UTF-8");
    assert_eq!(decode(&status).expect("status ASON").encode(), status);
    assert!(status.starts_with("s:0\na:status\nv:0.1.0\nt:"), "{status}");
    assert!(status.contains("\nq:0\nh:~\nk:~\n"), "{status}");

    let candidate = env!("CARGO_BIN_EXE_ash");
    let check = run(&["self", "check", "--candidate", candidate], b"");
    assert!(check.status.success(), "stderr={:?}", check.stderr);
    let check = String::from_utf8(check.stdout).expect("check UTF-8");
    assert_eq!(decode(&check).expect("check ASON").encode(), check);
    assert!(check.starts_with("s:0\na:healthy\nv:0.1.0\nt:"), "{check}");
}

#[test]
fn self_update_fails_closed_without_embedded_release_trust() {
    let directory = TestDirectory::new();
    let prefix = installed_fixture(&directory);
    let source = directory.0.join("empty-release");
    fs::create_dir(&source).expect("release source");
    let output = run(
        &[
            "self",
            "update",
            "--prefix",
            prefix.to_str().expect("prefix"),
            "--from",
            source.to_str().expect("source"),
        ],
        b"",
    );
    assert_eq!(output.status.code(), Some(70));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"s:1\ne{c}:\n11\n");
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
fn run_executes_one_bare_durable_fs_request() {
    let directory = TestDirectory::new();
    let request = Request::new(
        34,
        Arguments::Fs(
            FsArgs::new(vec![
                FsAction::new(
                    1,
                    FsActionKind::Create,
                    "created.txt",
                    None,
                    None,
                    Some(PatchContent::Inline("created\n".to_owned())),
                )
                .expect("create"),
            ])
            .expect("fs"),
        ),
        Budget::new(1024, 8, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let output = run_in(&directory, &["run"], request.as_bytes());

    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let response = std::str::from_utf8(&output.stdout).expect("UTF-8 response");
    assert_eq!(decode(response).expect("ASON response").encode(), response);
    assert!(response.starts_with("t:3\ni:34\ns:0\n"), "{response}");
    assert!(response.contains("d[1]{i,k,p,q,s,h}:"), "{response}");
    assert_eq!(
        fs::read(directory.0.join("created.txt")).expect("created"),
        b"created\n"
    );
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
fn run_executes_a_batch_and_returns_only_compact_child_references() {
    let nodes = vec![
        BatchNode::new(
            1,
            vec![],
            Arguments::Read(
                ReadArgs::new(vec!["Cargo.toml".to_owned()], ReadMode::Bytes, 0, 16).expect("read"),
            ),
        )
        .expect("node"),
        BatchNode::new(
            2,
            vec![1],
            Arguments::Read(
                ReadArgs::new(vec!["Cargo.toml".to_owned()], ReadMode::Bytes, 16, 16)
                    .expect("read"),
            ),
        )
        .expect("node"),
    ];
    let request = Request::new(
        33,
        Arguments::Batch(BatchArgs::new(nodes).expect("batch")),
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
    assert!(response.starts_with("t:3\ni:33\ns:0\n"), "{response}");
    assert!(response.contains("d[2]{i,o,s,c,r}:\n1,r,0,0,@1\n2,r,0,0,@2\n"));
    assert!(response.ends_with("z:8\nr:~\n"), "{response}");
    assert!(
        !response.contains("[workspace]"),
        "child data leaked inline"
    );
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

#[cfg(feature = "human-shell")]
#[test]
fn shell_command_executes_stateful_sequence_without_machine_framing() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.0.join("child")).expect("create child");
    fs::write(directory.0.join("b.txt"), b"b").expect("write b");
    fs::write(directory.0.join("a.txt"), b"a").expect("write a");
    fs::write(directory.0.join("child/payload.bin"), b"payload").expect("write payload");
    let output = run_in(
        &directory,
        &[
            "shell",
            "--no-profile",
            "-c",
            "cd .; pwd; echo \"hello world\"; ls -1; cd child; pwd; export HOME=..; cd; pwd; unset HOME; cd child; grep -n pay payload.bin; cat payload.bin; echo -n done",
        ],
        b"",
    );

    let root = fs::canonicalize(&directory.0).expect("canonical root");
    let child = fs::canonicalize(directory.0.join("child")).expect("canonical child");
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert_eq!(
        output.stdout,
        format!(
            "{}\nhello world\na.txt\nb.txt\nchild\n{}\n{}\n1:payload\npayloaddone",
            root.display(),
            child.display(),
            root.display()
        )
        .as_bytes()
    );
    assert!(output.stderr.is_empty());

    fs::write(
        directory.0.join("script.ash"),
        b"cd .; echo from-file; cd child; pwd",
    )
    .expect("script file");
    let file = run_in(&directory, &["shell", "--no-profile", "script.ash"], b"");
    assert!(file.status.success(), "stderr={:?}", file.stderr);
    assert_eq!(
        file.stdout,
        format!("from-file\n{}\n", child.display()).as_bytes()
    );
    assert!(file.stderr.is_empty());

    fs::write(directory.0.join("-script.ash"), b"echo leading-dash").expect("leading-dash script");
    let leading_dash = run_in(&directory, &["shell", "--", "-script.ash"], b"");
    assert!(
        leading_dash.status.success(),
        "stderr={:?}",
        leading_dash.stderr
    );
    assert_eq!(leading_dash.stdout, b"leading-dash\n");
    assert!(leading_dash.stderr.is_empty());
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_command_executes_journaled_portable_mutations() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("source file"), b"payload\n").expect("source file");

    let output = run_in(
        &directory,
        &[
            "shell",
            "--no-profile",
            "-c",
            "touch created; cp 'source file' copied; mv copied moved; rm 'source file'; echo done",
        ],
        b"",
    );

    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert_eq!(output.stdout, b"done\n");
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read(directory.0.join("created")).expect("created file"),
        b""
    );
    assert_eq!(
        fs::read(directory.0.join("moved")).expect("moved file"),
        b"payload\n"
    );
    assert!(!directory.0.join("source file").exists());
    assert!(!directory.0.join("copied").exists());

    let conflict = run_in(
        &directory,
        &["shell", "--no-profile", "-c", "touch moved"],
        b"",
    );
    assert_eq!(conflict.status.code(), Some(1));
    assert!(conflict.stdout.is_empty());
    assert!(
        std::str::from_utf8(&conflict.stderr)
            .expect("human diagnostic")
            .contains("no-overwrite destination already exists")
    );
    assert_eq!(
        fs::read(directory.0.join("moved")).expect("conflicting target remains"),
        b"payload\n"
    );
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_command_executes_short_circuit_conditional_lists() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("input.txt"), b"hit\n").expect("conditional input");

    let output = run_in(
        &directory,
        &[
            "shell",
            "--no-profile",
            "-c",
            "grep -F missing input.txt && touch skipped >opened || echo fallback &&\n echo continued; grep -F hit input.txt && echo matched",
        ],
        b"",
    );

    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert_eq!(output.stdout, b"fallback\ncontinued\nhit\nmatched\n");
    assert!(output.stderr.is_empty());
    assert!(!directory.0.join("skipped").exists());
    assert!(!directory.0.join("opened").exists());

    let malformed = run_in(
        &directory,
        &[
            "shell",
            "--no-profile",
            "-c",
            "touch parse-blocked; echo done &&",
        ],
        b"",
    );
    assert_eq!(malformed.status.code(), Some(2));
    assert!(malformed.stdout.is_empty());
    assert!(
        std::str::from_utf8(&malformed.stderr)
            .expect("human parse diagnostic")
            .contains("a conditional operator must be followed by a pipeline")
    );
    assert!(!directory.0.join("parse-blocked").exists());
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_command_executes_nested_command_substitutions() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.0.join("child")).expect("child directory");
    fs::write(directory.0.join("input.txt"), b"hit\n").expect("substitution input");

    let output = run_in(
        &directory,
        &[
            "shell",
            "--no-profile",
            "-c",
            "export TOKEN=parent; echo \"value=$(echo one; echo two)\" $(echo \"split fields\"); echo \"$(cd child; pwd)\"; pwd; grep -F hit input.txt || echo \"$(touch skipped)\"; echo payload >$(echo output.txt); echo \"$(cat output.txt)\"; echo \"$(exit 7)\" after",
        ],
        b"",
    );

    let root = fs::canonicalize(&directory.0).expect("canonical root");
    let child = fs::canonicalize(directory.0.join("child")).expect("canonical child");
    let expected = format!(
        "value=one\ntwo split fields\n{}\n{}\nhit\npayload\n after\n",
        child.display(),
        root.display()
    );
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert_eq!(output.stdout, expected.as_bytes());
    assert!(output.stderr.is_empty());
    assert!(!directory.0.join("skipped").exists());
    assert_eq!(
        fs::read(directory.0.join("output.txt")).expect("expanded redirection target"),
        b"payload\n"
    );

    let malformed = run_in(
        &directory,
        &[
            "shell",
            "--no-profile",
            "-c",
            "touch parse-blocked; echo \"$(echo",
        ],
        b"",
    );
    assert_eq!(malformed.status.code(), Some(2));
    assert!(malformed.stdout.is_empty());
    assert!(!directory.0.join("parse-blocked").exists());
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_profiles_are_opt_in_share_state_and_keep_cli_paths_anchored() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.0.join("child")).expect("child directory");
    fs::write(
        directory.0.join("profile.ash"),
        b"export TOKEN='profile value'; cd child; echo profile-loaded",
    )
    .expect("profile");
    fs::write(directory.0.join("script.ash"), b"echo script-loaded").expect("script");

    let configured = run_from_with_env(
        Some(&directory.0),
        &["shell", "-c", "echo \"$TOKEN\"; pwd"],
        b"",
        &[("ASH_PROFILE", "profile.ash")],
    );
    let child = fs::canonicalize(directory.0.join("child")).expect("child path");
    assert!(
        configured.status.success(),
        "stderr={:?}",
        configured.stderr
    );
    assert_eq!(
        configured.stdout,
        format!("profile-loaded\nprofile value\n{}\n", child.display()).as_bytes()
    );
    assert!(configured.stderr.is_empty());

    let disabled = run_from_with_env(
        Some(&directory.0),
        &["shell", "--no-profile", "-c", "echo \"$TOKEN\""],
        b"",
        &[("ASH_PROFILE", "profile.ash")],
    );
    assert!(disabled.status.success(), "stderr={:?}", disabled.stderr);
    assert_eq!(disabled.stdout, b"\n");
    assert!(disabled.stderr.is_empty());

    let explicit_after_input = run_in(
        &directory,
        &["shell", "-c", "echo \"$TOKEN\"", "--profile", "profile.ash"],
        b"",
    );
    assert!(
        explicit_after_input.status.success(),
        "stderr={:?}",
        explicit_after_input.stderr
    );
    assert_eq!(
        explicit_after_input.stdout,
        b"profile-loaded\nprofile value\n"
    );
    assert!(explicit_after_input.stderr.is_empty());

    let anchored_script = run_in(
        &directory,
        &["shell", "--profile", "profile.ash", "script.ash"],
        b"",
    );
    assert!(
        anchored_script.status.success(),
        "stderr={:?}",
        anchored_script.stderr
    );
    assert_eq!(anchored_script.stdout, b"profile-loaded\nscript-loaded\n");
    assert!(anchored_script.stderr.is_empty());
}

#[cfg(feature = "human-shell")]
#[test]
fn malformed_profiles_do_not_partially_execute_and_profile_exit_stops_startup() {
    let directory = TestDirectory::new();
    fs::write(
        directory.0.join("profile.ash"),
        b"echo partial; echo 'unterminated",
    )
    .expect("malformed profile");

    let malformed = run_in(
        &directory,
        &["shell", "--profile", "profile.ash", "-c", "echo main"],
        b"",
    );
    assert_eq!(malformed.status.code(), Some(2));
    assert!(malformed.stdout.is_empty());
    assert_eq!(
        malformed.stderr,
        b"ash: single-quoted text is missing its closing quote at bytes 19..32\n"
    );

    fs::write(
        directory.0.join("profile.ash"),
        b"echo profile; exit 19; echo skipped",
    )
    .expect("exiting profile");
    let exited = run_in(
        &directory,
        &["shell", "--profile", "profile.ash", "-c", "echo main"],
        b"",
    );
    assert_eq!(exited.status.code(), Some(19));
    assert_eq!(exited.stdout, b"profile\n");
    assert!(exited.stderr.is_empty());
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_exit_stops_remaining_commands_and_sets_process_status() {
    let output = run(
        &[
            "shell",
            "--no-profile",
            "-c",
            "echo before; exit 7; echo after",
        ],
        b"",
    );

    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"before\n");
    assert!(output.stderr.is_empty());
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_command_launches_native_argv_with_persistent_cwd_and_environment() {
    let directory = TestDirectory::new();
    let executable = compile_shell_helper(&directory);
    fs::create_dir(directory.0.join("work")).expect("work directory");
    let source = format!(
        "cd work; export PATH=../bin; export ASH_NATIVE_TOKEN=present; {executable} 'alpha beta' plain"
    );

    let output = run_in(&directory, &["shell", "--no-profile", "-c", &source], b"");

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        output.stdout,
        b"cwd=work\ntoken=present\narguments=alpha beta|plain\n"
    );
    assert_eq!(output.stderr, b"native-stderr\n");

    let expanded_source = format!(
        "cd work; export PATH=../bin; export TOKEN='alpha beta'; export ASH_NATIVE_TOKEN=\"$TOKEN\"; {executable} pre${{TOKEN}}post \"$TOKEN\"; echo $?"
    );
    let expanded = run_in(
        &directory,
        &["shell", "--no-profile", "-c", &expanded_source],
        b"",
    );
    assert!(expanded.status.success(), "stderr={:?}", expanded.stderr);
    assert_eq!(
        expanded.stdout,
        b"cwd=work\ntoken=alpha beta\narguments=prealpha|betapost|alpha beta\n23\n"
    );
    assert_eq!(expanded.stderr, b"native-stderr\n");

    let pipeline_source =
        format!("export PATH=bin; {executable} --emit pipeline-data | {executable} --copy");
    let pipeline = run_in(
        &directory,
        &["shell", "--no-profile", "-c", &pipeline_source],
        b"",
    );
    assert!(pipeline.status.success(), "stderr={:?}", pipeline.stderr);
    assert_eq!(pipeline.stdout, b"pipeline-data\n");
    assert!(pipeline.stderr.is_empty());

    let mixed_pipeline_source = format!(
        "export PATH=bin; echo cli-portable | {executable} --copy | grep -F cli-portable -"
    );
    let mixed_pipeline = run_in(
        &directory,
        &["shell", "--no-profile", "-c", &mixed_pipeline_source],
        b"",
    );
    assert!(
        mixed_pipeline.status.success(),
        "stderr={:?}",
        mixed_pipeline.stderr
    );
    assert_eq!(mixed_pipeline.stdout, b"cli-portable\n");
    assert!(mixed_pipeline.stderr.is_empty());

    let stateful_pipeline_source = format!(
        "export PATH=bin; export PIPE_VALUE=parent; export PIPE_VALUE=child | echo \"$PIPE_VALUE\"; unset PIPE_VALUE | echo \"$PIPE_VALUE\"; set -o pipefail | exit 17; echo $?; export PIPE_VALUE=child | {executable} --copy; echo \"$PIPE_VALUE\""
    );
    let stateful_pipeline = run_in(
        &directory,
        &["shell", "--no-profile", "-c", &stateful_pipeline_source],
        b"",
    );
    assert!(
        stateful_pipeline.status.success(),
        "stderr={:?}",
        stateful_pipeline.stderr
    );
    assert_eq!(stateful_pipeline.stdout, b"parent\nparent\n17\nparent\n");
    assert!(stateful_pipeline.stderr.is_empty());

    let pipefail_source = format!(
        "export PATH=bin; {executable} --exit 9 | {executable} --exit 7 | {executable} --copy; echo $?; set -o pipefail; {executable} --exit 9 | {executable} --exit 7 | {executable} --copy; echo $?; set +o pipefail; {executable} --exit 9 | {executable} --exit 7 | {executable} --copy; echo $?"
    );
    let pipefail = run_in(
        &directory,
        &["shell", "--no-profile", "-c", &pipefail_source],
        b"",
    );
    assert!(pipefail.status.success(), "stderr={:?}", pipefail.stderr);
    assert_eq!(pipefail.stdout, b"0\n7\n0\n");
    assert!(pipefail.stderr.is_empty());

    fs::write(directory.0.join("input.bin"), b"redirected-cli-input")
        .expect("write redirection input");
    fs::write(directory.0.join("first.log"), b"stale").expect("seed first redirection target");
    let redirection_source = format!(
        "export PATH=bin; {executable} --ordered >first.log >combined.log 2>&1; {executable} --ordered 2>&1 >stdout.log; {executable} --copy <input.bin >copied.bin; {executable} --emit producer >producer-only.log | {executable} --copy >eof.log; {executable} --ordered 2>&1 >descriptor-stdout.log | {executable} --copy >descriptor-pipe.log; {executable} --emit both >both-producer.log | {executable} --copy <input.bin >both-consumer.log"
    );
    let redirected = run_in(
        &directory,
        &["shell", "--no-profile", "-c", &redirection_source],
        b"",
    );
    assert!(
        redirected.status.success(),
        "stderr={:?}",
        redirected.stderr
    );
    assert_eq!(redirected.stdout, b"stderr-a\nstderr-b\n");
    assert!(redirected.stderr.is_empty());
    assert_eq!(fs::read(directory.0.join("first.log")).expect("first"), b"");
    assert_eq!(
        fs::read(directory.0.join("combined.log")).expect("combined"),
        b"stdout-a\nstderr-a\nstdout-b\nstderr-b\n"
    );
    assert_eq!(
        fs::read(directory.0.join("stdout.log")).expect("stdout"),
        b"stdout-a\nstdout-b\n"
    );
    assert_eq!(
        fs::read(directory.0.join("copied.bin")).expect("copied"),
        b"redirected-cli-input"
    );
    assert_eq!(
        fs::read(directory.0.join("producer-only.log")).expect("producer-only"),
        b"producer\n"
    );
    assert!(
        fs::read(directory.0.join("eof.log"))
            .expect("EOF")
            .is_empty()
    );
    assert_eq!(
        fs::read(directory.0.join("descriptor-stdout.log")).expect("descriptor stdout"),
        b"stdout-a\nstdout-b\n"
    );
    assert_eq!(
        fs::read(directory.0.join("descriptor-pipe.log")).expect("descriptor pipe"),
        b"stderr-a\nstderr-b\n"
    );
    assert_eq!(
        fs::read(directory.0.join("both-producer.log")).expect("both producer"),
        b"both\n"
    );
    assert_eq!(
        fs::read(directory.0.join("both-consumer.log")).expect("both consumer"),
        b"redirected-cli-input"
    );

    let portable_redirection_source = "echo first >portable.log; echo second >>portable.log; \
         cat - <input.bin >portable-copy.bin; echo portable-stderr 1>&2; \
         echo portable-file 2>portable-stderr.log 1>&2; \
         echo closed >portable-side.log | cat - >portable-eof.log";
    let portable_redirected = run_in(
        &directory,
        &["shell", "--no-profile", "-c", portable_redirection_source],
        b"",
    );
    assert!(
        portable_redirected.status.success(),
        "stderr={:?}",
        portable_redirected.stderr
    );
    assert!(portable_redirected.stdout.is_empty());
    assert_eq!(portable_redirected.stderr, b"portable-stderr\n");
    assert_eq!(
        fs::read(directory.0.join("portable.log")).expect("portable output"),
        b"first\nsecond\n"
    );
    assert_eq!(
        fs::read(directory.0.join("portable-copy.bin")).expect("portable copy"),
        b"redirected-cli-input"
    );
    assert_eq!(
        fs::read(directory.0.join("portable-stderr.log")).expect("portable stderr file"),
        b"portable-file\n"
    );
    assert_eq!(
        fs::read(directory.0.join("portable-side.log")).expect("portable side output"),
        b"closed\n"
    );
    assert!(
        fs::read(directory.0.join("portable-eof.log"))
            .expect("portable EOF output")
            .is_empty()
    );

    fs::write(directory.0.join("stateful-append.log"), b"preserved")
        .expect("seed stateful append target");
    let stateful_redirection_source = format!(
        "export PATH=bin; \
         export CLI_STATE=parent >stateful-first.log >stateful-simple.log; \
         export CLI_STATE=child 2>stateful-stage.log 1>&2 | {executable} --copy >stateful-eof.log; \
         set -o pipefail >>stateful-append.log; echo \"$CLI_STATE\""
    );
    let stateful_redirected = run_in(
        &directory,
        &["shell", "--no-profile", "-c", &stateful_redirection_source],
        b"",
    );
    assert!(
        stateful_redirected.status.success(),
        "stderr={:?}",
        stateful_redirected.stderr
    );
    assert_eq!(stateful_redirected.stdout, b"parent\n");
    assert!(stateful_redirected.stderr.is_empty());
    for target in [
        "stateful-first.log",
        "stateful-simple.log",
        "stateful-stage.log",
        "stateful-eof.log",
    ] {
        assert!(
            fs::read(directory.0.join(target))
                .unwrap_or_else(|error| panic!("read {target}: {error}"))
                .is_empty(),
            "stateful target {target} must contain no raw command output"
        );
    }
    assert_eq!(
        fs::read(directory.0.join("stateful-append.log")).expect("stateful append output"),
        b"preserved"
    );

    let stateful_exit = run_in(
        &directory,
        &[
            "shell",
            "--no-profile",
            "-c",
            "exit 9 >stateful-exit.log; echo unreachable",
        ],
        b"",
    );
    assert_eq!(stateful_exit.status.code(), Some(9));
    assert!(stateful_exit.stdout.is_empty());
    assert!(stateful_exit.stderr.is_empty());
    assert!(
        fs::read(directory.0.join("stateful-exit.log"))
            .expect("stateful exit output")
            .is_empty()
    );
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_command_accepts_bounded_stdin_and_file_sources() {
    let output = run(&["shell"], b"echo from-stdin\n");

    assert!(output.status.success(), "stderr={:?}", output.stderr);
    assert_eq!(output.stdout, b"from-stdin\n");
    assert!(output.stderr.is_empty());

    let at_limit = run(&["shell"], &vec![b'#'; 1024 * 1024]);
    assert!(at_limit.status.success(), "stderr={:?}", at_limit.stderr);
    assert!(at_limit.stdout.is_empty());
    assert!(at_limit.stderr.is_empty());

    let over_limit = run(&["shell"], &vec![b'#'; 1024 * 1024 + 1]);
    assert_eq!(over_limit.status.code(), Some(2));
    assert!(over_limit.stdout.is_empty());
    assert_eq!(
        over_limit.stderr,
        b"ash: shell source exceeds the 1 MiB input ceiling\n"
    );

    let directory = TestDirectory::new();
    fs::write(directory.0.join("at-limit.ash"), vec![b'#'; 1024 * 1024]).expect("at-limit script");
    let file_at_limit = run_in(&directory, &["shell", "at-limit.ash"], b"");
    assert!(
        file_at_limit.status.success(),
        "stderr={:?}",
        file_at_limit.stderr
    );
    assert!(file_at_limit.stdout.is_empty());
    assert!(file_at_limit.stderr.is_empty());

    fs::write(
        directory.0.join("over-limit.ash"),
        vec![b'#'; 1024 * 1024 + 1],
    )
    .expect("over-limit script");
    let file_over_limit = run_in(&directory, &["shell", "over-limit.ash"], b"");
    assert_eq!(file_over_limit.status.code(), Some(2));
    assert!(file_over_limit.stdout.is_empty());
    assert_eq!(
        file_over_limit.stderr,
        b"ash: shell source exceeds the 1 MiB input ceiling\n"
    );

    fs::write(directory.0.join("invalid.ash"), [0xff]).expect("invalid script");
    let invalid = run_in(&directory, &["shell", "invalid.ash"], b"");
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert_eq!(invalid.stderr, b"ash: shell source must be valid UTF-8\n");

    let missing = run_in(&directory, &["shell", "missing.ash"], b"");
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    assert!(
        missing
            .stderr
            .starts_with(b"ash: cannot open shell script: ")
    );

    fs::write(
        directory.0.join("profile-at-limit.ash"),
        vec![b'#'; 1024 * 1024],
    )
    .expect("at-limit profile");
    let profile_at_limit = run_in(
        &directory,
        &[
            "shell",
            "--profile",
            "profile-at-limit.ash",
            "-c",
            "echo ready",
        ],
        b"",
    );
    assert!(
        profile_at_limit.status.success(),
        "stderr={:?}",
        profile_at_limit.stderr
    );
    assert_eq!(profile_at_limit.stdout, b"ready\n");
    assert!(profile_at_limit.stderr.is_empty());

    fs::write(
        directory.0.join("profile-over-limit.ash"),
        vec![b'#'; 1024 * 1024 + 1],
    )
    .expect("over-limit profile");
    let profile_over_limit = run_in(
        &directory,
        &[
            "shell",
            "--profile",
            "profile-over-limit.ash",
            "-c",
            "echo skipped",
        ],
        b"",
    );
    assert_eq!(profile_over_limit.status.code(), Some(2));
    assert!(profile_over_limit.stdout.is_empty());
    assert_eq!(
        profile_over_limit.stderr,
        b"ash: shell profile exceeds the 1 MiB input ceiling\n"
    );

    fs::write(directory.0.join("profile-invalid.ash"), [0xff]).expect("invalid profile");
    let profile_invalid = run_in(
        &directory,
        &[
            "shell",
            "--profile",
            "profile-invalid.ash",
            "-c",
            "echo skipped",
        ],
        b"",
    );
    assert_eq!(profile_invalid.status.code(), Some(2));
    assert!(profile_invalid.stdout.is_empty());
    assert_eq!(
        profile_invalid.stderr,
        b"ash: shell profile must be valid UTF-8\n"
    );
}

#[cfg(feature = "human-shell")]
#[test]
fn shell_usage_and_parse_failures_are_human_diagnostics() {
    let usage = run(&["shell", "--unknown"], b"");
    assert_eq!(usage.status.code(), Some(2));
    assert!(usage.stdout.is_empty());
    assert_eq!(
        usage.stderr,
        b"ash: usage: ash shell [--no-profile | --profile FILE] [-c SOURCE | FILE]\n"
    );

    for arguments in [
        &["shell", "--profile"][..],
        &["shell", "--profile", "one", "--no-profile"][..],
        &["shell", "-c", "one", "two"][..],
    ] {
        let conflict = run(arguments, b"");
        assert_eq!(conflict.status.code(), Some(2));
        assert_eq!(
            conflict.stderr,
            b"ash: usage: ash shell [--no-profile | --profile FILE] [-c SOURCE | FILE]\n"
        );
    }

    let parse = run(&["shell", "-c", "echo 'unterminated"], b"");
    assert_eq!(parse.status.code(), Some(2));
    assert!(parse.stdout.is_empty());
    assert_eq!(
        parse.stderr,
        b"ash: single-quoted text is missing its closing quote at bytes 5..18\n"
    );
    assert!(!parse.stderr.starts_with(b"s:1\n"));

    let parameter = run(&["shell", "-c", "echo ${NAME"], b"");
    assert_eq!(parameter.status.code(), Some(2));
    assert!(parameter.stdout.is_empty());
    assert_eq!(
        parameter.stderr,
        b"ash: braced parameter expansion is missing its closing brace at bytes 5..11\n"
    );
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
            capability_mask: u64::MAX,
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
    assert_eq!(
        response.capability_mask(),
        PortableOperations::capability_mask()
    );
}

#[test]
fn rpc_enforces_the_negotiated_capability_mask() {
    let handshake = HandshakeRequest::new(
        30,
        ".",
        "integration-30",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: Capability::WorkspaceRead.mask(),
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode handshake")
    .encode();
    let request = Request::new(
        31,
        Arguments::Exec(
            ExecArgs::new(
                "unreachable-program",
                vec![],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
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
    assert!(result.starts_with("t:3\ni:31\ns:3\n"), "{result}");
    assert!(result.contains("e{c,q,p,x,a}:\n300,0,3,~,~\n"), "{result}");
}

#[test]
fn rpc_executes_a_request_after_the_handshake() {
    let handshake = HandshakeRequest::new(
        40,
        ".",
        "integration-40",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
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
fn rpc_executes_a_compare_and_swap_patch() {
    let directory = TestDirectory::new();
    let target = directory.0.join("target.txt");
    fs::write(&target, b"before\n").expect("write target");
    let handshake = HandshakeRequest::new(
        45,
        directory.0.to_string_lossy(),
        "integration-45",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode handshake")
    .encode();
    let request = Request::new(
        46,
        Arguments::Patch(
            PatchArgs::new(
                vec!["target.txt".to_owned()],
                vec![blake3::hash(b"before\n").to_hex().to_string()],
                vec![
                    PatchEdit::new(0, 0, 6, PatchContent::Inline("after".to_owned()))
                        .expect("edit"),
                ],
                0,
            )
            .expect("patch"),
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
    assert!(result.starts_with("t:3\ni:46\ns:0\n"), "{result}");
    assert!(result.contains("d[1]{p,s,h}:"), "{result}");
    assert_eq!(fs::read(target).expect("read target"), b"after\n");
}

#[test]
fn rpc_executes_a_durable_fs_transaction() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("source.txt"), b"source\n").expect("source");
    let handshake = HandshakeRequest::new(
        47,
        directory.0.to_string_lossy(),
        "integration-47",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode handshake")
    .encode();
    let request = Request::new(
        48,
        Arguments::Fs(
            FsArgs::new(vec![
                FsAction::new(
                    1,
                    FsActionKind::Copy,
                    "source.txt",
                    Some("copied.txt".to_owned()),
                    Some(blake3::hash(b"source\n").to_hex().to_string()),
                    None,
                )
                .expect("copy"),
            ])
            .expect("fs"),
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
    assert!(result.starts_with("t:3\ni:48\ns:0\n"), "{result}");
    assert!(result.contains("d[1]{i,k,p,q,s,h}:"), "{result}");
    assert_eq!(
        fs::read(directory.0.join("copied.txt")).expect("copied"),
        b"source\n"
    );
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
            capability_mask: u64::MAX,
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
            capability_mask: u64::MAX,
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

#[test]
fn rpc_cancel_propagates_through_a_batch_and_skips_its_descendants() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("target.txt"), b"never read\n").expect("write target");
    let executable = compile_rpc_helper(&directory);
    let handshake = HandshakeRequest::new(
        63,
        directory.0.to_string_lossy(),
        "integration-63",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode handshake")
    .encode();
    let nodes = vec![
        BatchNode::new(
            1,
            vec![],
            Arguments::Exec(
                ExecArgs::new(
                    executable,
                    vec![
                        "gate".to_owned(),
                        "batch-node-ready".to_owned(),
                        "batch-node-release".to_owned(),
                    ],
                    ".",
                    vec![],
                    InputSource::None,
                    0,
                )
                .expect("exec"),
            ),
        )
        .expect("node"),
        BatchNode::new(
            2,
            vec![1],
            Arguments::Read(
                ReadArgs::new(vec!["target.txt".to_owned()], ReadMode::Bytes, 0, 16).expect("read"),
            ),
        )
        .expect("node"),
    ];
    let batch = Request::new(
        64,
        Arguments::Batch(BatchArgs::new(nodes).expect("batch")),
        Budget::new(4_096, 16, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let cancel = Request::new(
        65,
        Arguments::Cancel(CancelArgs::new(64).expect("cancel")),
        Budget::new(64, 1, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ash"))
        .arg("rpc")
        .current_dir(&directory.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ash");
    let mut stdin = child.stdin.take().expect("ash stdin");
    let mut input = frame(handshake.as_bytes());
    input.extend(frame(batch.as_bytes()));
    let started = Instant::now();
    stdin.write_all(&input).expect("write handshake and batch");
    stdin.flush().expect("flush handshake and batch");
    let marker = directory.0.join("batch-node-ready");
    while !marker.is_file() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "batch node did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    stdin
        .write_all(&frame(cancel.as_bytes()))
        .expect("write cancel");
    drop(stdin);
    let output = child.wait_with_output().expect("wait for ash");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(output.status.success(), "stderr={:?}", output.stderr);
    let frames = split_frames(&output.stdout);
    assert_eq!(frames.len(), 3);
    let batch = std::str::from_utf8(frames[1]).expect("batch response");
    let cancel = std::str::from_utf8(frames[2]).expect("cancel response");
    assert!(batch.starts_with("t:3\ni:64\ns:5\n"), "{batch}");
    assert!(batch.contains("1,x,3,7,@1\n2,r,2,~,~\n"), "{batch}");
    assert!(batch.contains("e{c,q,p,x,a}:\n800,0,4,~,~\n"), "{batch}");
    assert!(batch.ends_with("z:24\nr:~\n"), "{batch}");
    assert!(cancel.starts_with("t:3\ni:65\ns:0\n"), "{cancel}");
    assert!(cancel.contains("d{i,z}:\n64,1\n"), "{cancel}");
}

#[test]
fn rpc_retrieves_and_releases_retained_evidence_across_frames() {
    let directory = TestDirectory::new();
    fs::write(
        directory.0.join("many.txt"),
        (0..20)
            .map(|index| format!("needle-{index:02}\n"))
            .collect::<String>(),
    )
    .expect("write fixture");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ash"))
        .arg("rpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ash");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");

    let handshake = HandshakeRequest::new(
        70,
        directory.0.to_string_lossy(),
        "integration-70",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode")
    .encode();
    let handshake_response = exchange_frame(&mut stdin, &mut stdout, &handshake);
    assert!(
        std::str::from_utf8(&handshake_response)
            .expect("handshake response")
            .starts_with("t:0\ni:70\n")
    );

    let search = Request::new(
        71,
        Arguments::Search(SearchArgs::new("needle", vec![".".to_owned()], 0).expect("search")),
        Budget::new(32, 20, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let search_response = exchange_frame(&mut stdin, &mut stdout, &search);
    let search_response = std::str::from_utf8(&search_response).expect("search response");
    let reference = search_response
        .lines()
        .find_map(|line| line.strip_prefix("r:@"))
        .expect("retained reference")
        .parse::<u64>()
        .expect("reference number");

    let inspect = Request::new(
        72,
        Arguments::Ref(
            RefArgs::search(reference, 0, 1024 * 1024, "needle-19", 0).expect("reference search"),
        ),
        Budget::new(256, 8, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let inspect_response = exchange_frame(&mut stdin, &mut stdout, &inspect);
    let inspect_response = std::str::from_utf8(&inspect_response).expect("inspect response");
    assert!(inspect_response.starts_with("t:3\ni:72\ns:0\n"));
    assert!(inspect_response.contains("needle-19"));

    let release = Request::new(
        73,
        Arguments::Ref(RefArgs::release(reference).expect("release")),
        Budget::new(64, 1, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let release_response = exchange_frame(&mut stdin, &mut stdout, &release);
    let release_response = std::str::from_utf8(&release_response).expect("release response");
    assert!(release_response.starts_with("t:3\ni:73\ns:0\n"));
    assert!(release_response.contains(&format!("d{{r,z}}:\n@{reference},1\n")));

    drop(stdin);
    assert!(child.wait().expect("wait").success());
    let mut diagnostics = Vec::new();
    stderr.read_to_end(&mut diagnostics).expect("stderr");
    assert!(diagnostics.is_empty(), "stderr={diagnostics:?}");
}

#[test]
fn rpc_chains_a_snapshot_reference_into_a_workspace_delta() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("tracked.txt"), b"before").expect("write");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ash"))
        .arg("rpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ash");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");

    let handshake = HandshakeRequest::new(
        90,
        directory.0.to_string_lossy(),
        "integration-90",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("handshake")
    .encode()
    .expect("encode")
    .encode();
    exchange_frame(&mut stdin, &mut stdout, &handshake);
    let capture = Request::new(
        91,
        Arguments::Snapshot(
            SnapshotArgs::new(vec![".".to_owned()], 64, SnapshotMode::Capture, None, 0)
                .expect("snapshot"),
        ),
        Budget::new(2048, 16, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let captured = exchange_frame(&mut stdin, &mut stdout, &capture);
    let captured = std::str::from_utf8(&captured).expect("capture response");
    let baseline = captured
        .lines()
        .find_map(|line| line.strip_prefix("r:@"))
        .expect("snapshot reference")
        .parse::<u64>()
        .expect("reference number");

    fs::write(directory.0.join("tracked.txt"), b"after").expect("modify");
    let delta = Request::new(
        92,
        Arguments::Snapshot(
            SnapshotArgs::new(
                vec![".".to_owned()],
                64,
                SnapshotMode::Delta,
                Some(baseline),
                0,
            )
            .expect("delta"),
        ),
        Budget::new(2048, 16, 30_000).expect("budget"),
    )
    .expect("request")
    .encode()
    .expect("encode")
    .encode();
    let changed = exchange_frame(&mut stdin, &mut stdout, &delta);
    let changed = std::str::from_utf8(&changed).expect("delta response");
    assert!(changed.starts_with("t:3\ni:92\ns:0\n"), "{changed}");
    assert!(changed.contains("d[1]{p,c,k,z,h}:"), "{changed}");
    assert!(changed.contains(",2,0,5,"), "{changed}");

    drop(stdin);
    assert!(child.wait().expect("wait").success());
    let mut diagnostics = Vec::new();
    stderr.read_to_end(&mut diagnostics).expect("stderr");
    assert!(diagnostics.is_empty(), "stderr={diagnostics:?}");
}
