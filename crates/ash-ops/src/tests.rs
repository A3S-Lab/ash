use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ash_engine::{Engine, Parallelism, SessionConfig};
use ash_platform::Workspace;
use ash_protocol::ason::decode;
use ash_protocol::request::{
    Arguments, BatchArgs, BatchNode, Budget, EXEC_CLEAR_ENVIRONMENT, ExecArgs, FsAction,
    FsActionKind, FsArgs, InputSource, LIST_FILES_ONLY, ListArgs, PatchArgs, PatchContent,
    PatchEdit, REF_CASE_INSENSITIVE, ReadArgs, ReadMode, RefArgs, Request, SEARCH_CASE_INSENSITIVE,
    SNAPSHOT_INCLUDE_HIDDEN, SearchArgs, SnapshotArgs, SnapshotMode,
};
use ash_protocol::response::{
    ErrorCode, RESULT_PARTIAL, RESULT_REDUCED, RESULT_RETAINED, RESULT_TRUNCATED, ResultData,
    Status,
};
use ash_protocol::{ApprovalChallenge, Capability};
use ash_store::{StoreLimits, StoreResidency};

use super::{AuthorizationPolicy, PermitAuthority, PortableOperations};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ash-ops-{}-{id}", std::process::id()));
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runtime(directory: &TestDirectory) -> (ash_engine::Session, PortableOperations) {
    let parallelism = Parallelism::for_available_cpus(4);
    let engine = Engine::new(parallelism).expect("engine");
    let session = engine
        .open_session(SessionConfig::new(1, ".", 64 * 1024, parallelism))
        .expect("session");
    let operations = PortableOperations::new(Workspace::new(&directory.0).expect("workspace"));
    (session, operations)
}

fn budget(tokens: u32, records: u32) -> Budget {
    Budget::new(tokens, records, 30_000).expect("budget")
}

#[tokio::test]
async fn capability_policy_requires_one_time_action_bound_approval() {
    let directory = TestDirectory::new();
    let parallelism = Parallelism::for_available_cpus(2);
    let engine = Engine::new(parallelism).expect("engine");
    let session = engine
        .open_session(SessionConfig::new(1, ".", 64 * 1024, parallelism))
        .expect("session");

    let denied_arguments = Arguments::Fs(
        FsArgs::new(vec![
            FsAction::new(
                1,
                FsActionKind::Create,
                "denied.txt",
                None,
                None,
                Some(PatchContent::Inline("denied".to_owned())),
            )
            .expect("create"),
        ])
        .expect("filesystem arguments"),
    );
    let denied = Request::new(91, denied_arguments, budget(1024, 8)).expect("request");
    let operations = PortableOperations::with_authorization(
        Workspace::new(&directory.0).expect("workspace"),
        AuthorizationPolicy::allow(Capability::WorkspaceRead.mask()).expect("read-only policy"),
    );
    let program = session.begin(&denied).await.expect("program");
    let response = operations
        .execute(&denied, &program)
        .await
        .expect("denied response");
    assert_eq!(response.status(), Status::Denied);
    assert_eq!(
        response.error().expect("error").code,
        ErrorCode::CapabilityDenied
    );
    assert!(!directory.0.join("denied.txt").exists());
    drop(program);

    let authority =
        PermitAuthority::new([7; 32], [8; 16], "test-policy").expect("permit authority");
    let policy = AuthorizationPolicy::with_approvals(
        "test-policy",
        Capability::RetainedResult.mask(),
        Capability::WorkspaceWrite.mask(),
        authority.clone(),
    )
    .expect("approval policy");
    let operations = PortableOperations::with_authorization(
        Workspace::new(&directory.0).expect("workspace"),
        policy,
    );
    let approved_arguments = Arguments::Fs(
        FsArgs::new(vec![
            FsAction::new(
                1,
                FsActionKind::Create,
                "approved.txt",
                None,
                None,
                Some(PatchContent::Inline("approved".to_owned())),
            )
            .expect("create"),
        ])
        .expect("filesystem arguments"),
    );
    let first =
        Request::new(92, approved_arguments.clone(), budget(1024, 8)).expect("first request");
    let program = session.begin(&first).await.expect("program");
    let response = operations
        .execute(&first, &program)
        .await
        .expect("approval response");
    assert_eq!(response.status(), Status::Denied);
    assert_eq!(response.flags(), RESULT_RETAINED);
    let error = response.error().expect("approval error");
    assert_eq!(error.code, ErrorCode::PermitRequired);
    let evidence = program
        .store()
        .get(error.evidence.expect("challenge reference"))
        .expect("challenge evidence")
        .read_all(8 * 1024 * 1024)
        .await
        .expect("read challenge evidence");
    let challenge = ApprovalChallenge::decode(
        &decode(std::str::from_utf8(&evidence).expect("challenge UTF-8")).expect("challenge ASON"),
    )
    .expect("challenge schema");
    let token = authority.issue(&challenge).expect("approved token");
    assert!(!directory.0.join("approved.txt").exists());
    drop(program);

    let retry = Request::new(93, approved_arguments.clone(), budget(2048, 16))
        .expect("retry")
        .with_permit(token.clone());
    let program = session.begin(&retry).await.expect("program");
    let response = operations
        .execute(&retry, &program)
        .await
        .expect("successful retry");
    assert_eq!(response.status(), Status::Success);
    assert_eq!(
        fs::read(directory.0.join("approved.txt")).expect("created file"),
        b"approved"
    );
    drop(program);

    let replay = Request::new(94, approved_arguments, budget(1024, 8))
        .expect("replay")
        .with_permit(token);
    let program = session.begin(&replay).await.expect("program");
    let response = operations
        .execute(&replay, &program)
        .await
        .expect("replay response");
    assert_eq!(response.status(), Status::Denied);
    assert_eq!(
        response.error().expect("replay error").code,
        ErrorCode::PermitInvalid
    );
}

fn compile_helper(directory: &TestDirectory) -> String {
    let bin = directory.0.join("bin");
    fs::create_dir(&bin).expect("create bin");
    let source = directory.0.join("helper.rs");
    fs::write(
        &source,
        r#"
use std::io::{self, Read, Write};
use std::time::Duration;

fn main() {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("fail") => {
            eprintln!("bad");
            std::process::exit(7);
        }
        Some("diagnostic-fail") => {
            let mut output = io::BufWriter::new(io::stderr().lock());
            output.write_all(b"setup\ncommand\n").expect("write diagnostic header");
            for line in 0..20 {
                writeln!(output, "build noise-{line:02}").expect("write leading noise");
            }
            output
                .write_all(b"error[E0609]: no field `missing`\n")
                .expect("write diagnostic anchor");
            for line in 0..6 {
                writeln!(output, "detail-{line:02}").expect("write diagnostic detail");
            }
            for line in 0..24 {
                writeln!(output, "trailing noise-{line:02}").expect("write trailing noise");
            }
            output.write_all(b"summary\ndone\n").expect("write diagnostic footer");
            output.flush().expect("flush diagnostic output");
            std::process::exit(7);
        }
        Some("wait") => std::thread::sleep(Duration::from_secs(5)),
        Some("flood") => {
            let mut output = io::BufWriter::new(io::stdout().lock());
            let block = [b'x'; 16 * 1024];
            for _ in 0..256 {
                output.write_all(&block).expect("write flood block");
            }
            output
                .write_all(b"ASH_CAPTURE_TAIL")
                .expect("write flood tail");
            output.flush().expect("flush flood output");
        }
        Some("repeat") => {
            let mut output = io::BufWriter::new(io::stdout().lock());
            for _ in 0..4_096 {
                output
                    .write_all(b"same diagnostic\n")
                    .expect("write repeated line");
            }
            output.flush().expect("flush repeated output");
        }
        Some("block-repeat") => {
            let mut output = io::BufWriter::new(io::stdout().lock());
            for _ in 0..1_024 {
                output
                    .write_all(b"compile crate-a\nlink crate-a\nfinish crate-a\n")
                    .expect("write repeated block");
            }
            output.flush().expect("flush repeated block output");
        }
        Some("rendezvous") => {
            let own = arguments.next().expect("own marker");
            let peer = arguments.next().expect("peer marker");
            std::fs::write(own, b"ready").expect("write marker");
            for _ in 0..200 {
                if std::path::Path::new(&peer).exists() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            eprintln!("peer did not start concurrently");
            std::process::exit(9);
        }
        _ => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).expect("stdin");
            print!("{}:{input}", std::env::var("ASH_TEST").unwrap_or_default());
        }
    }
}

"#,
    )
    .expect("write helper");
    let executable_name = if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    };
    let output = bin.join(executable_name);
    let status = std::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .status()
        .expect("run rustc");
    assert!(status.success(), "compile helper");
    format!("bin/{executable_name}")
}

#[tokio::test]
async fn batch_runs_ready_nodes_concurrently_and_skips_only_failed_descendants() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("target.txt"), b"needle\nsecond\n").expect("write target");
    let executable = compile_helper(&directory);
    let (session, operations) = runtime(&directory);

    let exec = |arguments: Vec<&str>| {
        Arguments::Exec(
            ExecArgs::new(
                &executable,
                arguments.into_iter().map(str::to_owned).collect(),
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        )
    };
    let nodes = vec![
        BatchNode::new(
            1,
            vec![],
            exec(vec!["rendezvous", "left.ready", "right.ready"]),
        )
        .expect("node"),
        BatchNode::new(
            2,
            vec![],
            exec(vec!["rendezvous", "right.ready", "left.ready"]),
        )
        .expect("node"),
        BatchNode::new(
            3,
            vec![],
            Arguments::Search(
                SearchArgs::new("needle", vec!["target.txt".to_owned()], 0).expect("search"),
            ),
        )
        .expect("node"),
        BatchNode::new(4, vec![], exec(vec!["fail"])).expect("node"),
        BatchNode::new(
            5,
            vec![3],
            Arguments::Read(
                ReadArgs::new(vec!["target.txt".to_owned()], ReadMode::Lines, 1, 1).expect("read"),
            ),
        )
        .expect("node"),
        BatchNode::new(
            6,
            vec![4],
            Arguments::Read(
                ReadArgs::new(vec!["target.txt".to_owned()], ReadMode::Lines, 2, 1).expect("read"),
            ),
        )
        .expect("node"),
    ];
    let request = Request::new(
        90,
        Arguments::Batch(BatchArgs::new(nodes).expect("batch")),
        budget(8192, 48),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let response = operations
        .execute(&request, &program)
        .await
        .expect("batch response");
    let encoded = response.encode().expect("encode").encode();

    assert!(encoded.starts_with("t:3\ni:90\ns:5\n"), "{encoded}");
    assert!(encoded.contains("d[6]{i,o,s,c,r}:"), "{encoded}");
    assert!(encoded.contains("1,x,0,0,@1\n"), "{encoded}");
    assert!(encoded.contains("2,x,0,0,@2\n"), "{encoded}");
    assert!(encoded.contains("3,g,0,0,@3\n"), "{encoded}");
    assert!(encoded.contains("4,x,1,5,@4\n"), "{encoded}");
    assert!(encoded.contains("5,r,0,0,@5\n"), "{encoded}");
    assert!(encoded.contains("6,r,2,~,~\n"), "{encoded}");
    assert!(
        encoded.contains("e{c,q,p,x,a}:\n800,0,4,~,~\n"),
        "{encoded}"
    );
    assert!(encoded.ends_with("z:24\nr:~\n"), "{encoded}");

    let failed = program
        .store()
        .get(4)
        .expect("failed child response")
        .read_all(8 * 1024 * 1024)
        .await
        .expect("read child response");
    let failed = std::str::from_utf8(&failed).expect("ASON response");
    assert!(failed.starts_with("t:3\ni:4\ns:5\n"), "{failed}");
    assert_eq!(program.store().usage().expect("usage").entries, 5);
}

#[tokio::test]
async fn exec_handles_environment_stdin_failure_and_timeout_without_a_shell() {
    let directory = TestDirectory::new();
    let executable = compile_helper(&directory);
    let (session, operations) = runtime(&directory);

    let success = Request::new(
        50,
        Arguments::Exec(
            ExecArgs::new(
                &executable,
                vec![],
                ".",
                vec!["ASH_TEST=value".to_owned()],
                InputSource::Inline("payload".to_owned()),
                EXEC_CLEAR_ENVIRONMENT,
            )
            .expect("exec"),
        ),
        budget(1024, 8),
    )
    .expect("request");
    let program = session.begin(&success).await.expect("program");
    let response = operations
        .execute(&success, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(response.starts_with("t:3\ni:50\ns:0\n"));
    assert!(response.contains("d{k,c,ms,o,e,ro,re}:\n0,0,"));
    assert!(response.contains("value:payload"));
    drop(program);

    let failure = Request::new(
        51,
        Arguments::Exec(
            ExecArgs::new(
                &executable,
                vec!["fail".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        budget(1024, 8),
    )
    .expect("request");
    let program = session.begin(&failure).await.expect("program");
    let response = operations
        .execute(&failure, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(response.starts_with("t:3\ni:51\ns:5\n"));
    assert!(response.contains("d{k,c,ms,o,e,ro,re}:\n0,7,"));
    assert!(response.contains("e{c,q,p,x,a}:\n401,0,4,~,~\n"));
    drop(program);

    let timeout = Request::new(
        52,
        Arguments::Exec(
            ExecArgs::new(
                executable,
                vec!["wait".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        Budget::new(1024, 8, 50).expect("budget"),
    )
    .expect("request");
    let program = session.begin(&timeout).await.expect("program");
    let response = operations
        .execute(&timeout, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(response.starts_with("t:3\ni:52\ns:6\n"));
    assert!(response.contains("d{k,c,ms,o,e,ro,re}:\n2,~,"));
    assert!(response.contains("e{c,q,p,x,a}:\n402,2,4,~,~\n"));
}

#[tokio::test]
async fn failed_exec_focuses_diagnostics_and_retains_exact_stderr() {
    let directory = TestDirectory::new();
    let executable = compile_helper(&directory);
    let (session, operations) = runtime(&directory);
    let request = Request::new(
        57,
        Arguments::Exec(
            ExecArgs::new(
                executable,
                vec!["diagnostic-fail".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        budget(4_096, 8),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let response = operations
        .execute(&request, &program)
        .await
        .expect("response");

    assert_eq!(response.status(), Status::Failed);
    assert_eq!(
        response.flags(),
        RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED
    );
    let Some(ResultData::Exec(result)) = response.data() else {
        panic!("expected exec result");
    };
    assert_eq!(
        result.stderr.projection.as_deref(),
        Some(concat!(
            "setup\n",
            "command\n",
            "⋯18\n",
            "build noise-18\n",
            "build noise-19\n",
            "error[E0609]: no field `missing`\n",
            "detail-00\n",
            "detail-01\n",
            "detail-02\n",
            "detail-03\n",
            "detail-04\n",
            "detail-05\n",
            "⋯24\n",
            "summary\n",
            "done\n",
        ))
    );
    assert!(result.stdout.projection.is_none());
    assert!(result.stdout.reference.is_none());
    assert_eq!(result.stderr.reference, Some(1));

    let mut retained_source = "setup\ncommand\n".to_owned();
    for line in 0..20 {
        retained_source.push_str(&format!("build noise-{line:02}\n"));
    }
    retained_source.push_str("error[E0609]: no field `missing`\n");
    for line in 0..6 {
        retained_source.push_str(&format!("detail-{line:02}\n"));
    }
    for line in 0..24 {
        retained_source.push_str(&format!("trailing noise-{line:02}\n"));
    }
    retained_source.push_str("summary\ndone\n");
    let retained = program
        .store()
        .get(1)
        .expect("retained stderr")
        .read_all(128 * 1_024)
        .await
        .expect("read retained stderr");
    assert_eq!(retained.as_ref(), retained_source.as_bytes());
}

#[tokio::test]
async fn exec_retained_reference_preserves_bytes_beyond_the_projection_window() {
    let directory = TestDirectory::new();
    let executable = compile_helper(&directory);
    let (session, operations) = runtime(&directory);
    let request = Request::new(
        53,
        Arguments::Exec(
            ExecArgs::new(
                executable,
                vec!["flood".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        budget(1024, 8),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let response = operations
        .execute(&request, &program)
        .await
        .expect("response");

    assert_eq!(response.status(), Status::Success);
    assert_eq!(
        response.flags() & (RESULT_RETAINED | RESULT_TRUNCATED),
        RESULT_RETAINED | RESULT_TRUNCATED
    );
    assert_eq!(response.flags() & RESULT_PARTIAL, 0);
    assert!(
        response
            .encode()
            .expect("encode")
            .encode()
            .contains("ASH_CAPTURE_TAIL")
    );
    let retained = program.store().get(1).expect("complete stdout reference");
    assert_eq!(retained.len(), 4 * 1024 * 1024 + 16);
    assert_eq!(retained.residency(), StoreResidency::Disk);
    let retained = retained
        .read_all(8 * 1024 * 1024)
        .await
        .expect("read complete stdout");
    assert!(retained.ends_with(b"ASH_CAPTURE_TAIL"));
}

#[tokio::test]
async fn exec_collapses_repeated_lines_and_retains_exact_source() {
    let directory = TestDirectory::new();
    let executable = compile_helper(&directory);
    let (session, operations) = runtime(&directory);
    let request = Request::new(
        55,
        Arguments::Exec(
            ExecArgs::new(
                executable,
                vec!["repeat".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        budget(1_024, 8),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let response = operations
        .execute(&request, &program)
        .await
        .expect("response");

    assert_eq!(response.status(), Status::Success);
    assert_eq!(
        response.flags(),
        RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED
    );
    let Some(ResultData::Exec(result)) = response.data() else {
        panic!("expected exec result");
    };
    assert_eq!(
        result.stdout.projection.as_deref(),
        Some("same diagnostic\n×4096\n")
    );
    assert_eq!(result.stdout.reference, Some(1));
    let retained = program
        .store()
        .get(1)
        .expect("retained stdout")
        .read_all(128 * 1024)
        .await
        .expect("read retained stdout");
    assert_eq!(
        retained.as_ref(),
        "same diagnostic\n".repeat(4_096).as_bytes()
    );
}

#[tokio::test]
async fn exec_collapses_repeated_blocks_and_retains_exact_source() {
    let directory = TestDirectory::new();
    let executable = compile_helper(&directory);
    let (session, operations) = runtime(&directory);
    let request = Request::new(
        56,
        Arguments::Exec(
            ExecArgs::new(
                executable,
                vec!["block-repeat".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        budget(1_024, 8),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let response = operations
        .execute(&request, &program)
        .await
        .expect("response");

    assert_eq!(response.status(), Status::Success);
    assert_eq!(
        response.flags(),
        RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED
    );
    let Some(ResultData::Exec(result)) = response.data() else {
        panic!("expected exec result");
    };
    assert_eq!(
        result.stdout.projection.as_deref(),
        Some("compile crate-a\nlink crate-a\nfinish crate-a\n×1024#3\n")
    );
    assert_eq!(result.stdout.reference, Some(1));
    let retained = program
        .store()
        .get(1)
        .expect("retained stdout")
        .read_all(128 * 1_024)
        .await
        .expect("read retained stdout");
    assert_eq!(
        retained.as_ref(),
        "compile crate-a\nlink crate-a\nfinish crate-a\n"
            .repeat(1_024)
            .as_bytes()
    );
}

#[tokio::test]
async fn exec_output_beyond_retention_quota_fails_without_a_partial_reference() {
    let directory = TestDirectory::new();
    let executable = compile_helper(&directory);
    let parallelism = Parallelism::for_available_cpus(2);
    let engine = Engine::new(parallelism).expect("engine");
    let mut config = SessionConfig::new(2, ".", 64 * 1024, parallelism);
    config.store_limits = StoreLimits {
        max_bytes: 4 * 1024 * 1024,
        max_entries: 8,
    };
    let session = engine.open_session(config).expect("limited session");
    let operations = PortableOperations::new(Workspace::new(&directory.0).expect("workspace"));
    let request = Request::new(
        54,
        Arguments::Exec(
            ExecArgs::new(
                executable,
                vec!["flood".to_owned()],
                ".",
                vec![],
                InputSource::None,
                0,
            )
            .expect("exec"),
        ),
        budget(1024, 8),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let response = operations
        .execute(&request, &program)
        .await
        .expect("typed quota response");

    assert_eq!(response.status(), Status::BudgetExceeded);
    assert_eq!(
        response.error().expect("storage error").code,
        ErrorCode::StorageBudget
    );
    assert_eq!(response.flags(), 0);
    let usage = program.store().usage().expect("store usage");
    assert_eq!(usage.bytes, 0);
    assert_eq!(usage.entries, 0);
}

#[tokio::test]
async fn read_list_and_search_execute_through_one_session() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.0.join("src")).expect("mkdir");
    fs::write(
        directory.0.join("src").join("a.rs"),
        b"one\nAlpha NEEDLE\nthree\n",
    )
    .expect("write");
    fs::write(directory.0.join("src").join("b.rs"), b"needle lower\n").expect("write");
    let (session, operations) = runtime(&directory);

    let read = Request::new(
        1,
        Arguments::Read(
            ReadArgs::new(vec!["src/a.rs".to_owned()], ReadMode::Lines, 2, 1).expect("read"),
        ),
        budget(1024, 16),
    )
    .expect("request");
    let program = session.begin(&read).await.expect("program");
    let response = operations
        .execute(&read, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(response.contains("p[1]{i,v}:\n1,src/a.rs\n"));
    assert!(response.contains("d[1]{p,o,n,h,t,r}:\n1,2,1,"));
    assert!(response.contains("\"Alpha NEEDLE\\n\",~"));
    drop(program);

    let list = Request::new(
        2,
        Arguments::List(ListArgs::new(vec!["src".to_owned()], 1, LIST_FILES_ONLY).expect("list")),
        budget(1024, 16),
    )
    .expect("request");
    let program = session.begin(&list).await.expect("program");
    let response = operations
        .execute(&list, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(!response.contains("src/a.rs"));
    assert!(response.contains("p[1]{i,v}:\n2,src/b.rs\n"));
    assert!(response.contains("d[2]{p,k,z,m}:"));
    drop(program);

    let search = Request::new(
        3,
        Arguments::Search(
            SearchArgs::new("needle", vec!["src".to_owned()], SEARCH_CASE_INSENSITIVE)
                .expect("search"),
        ),
        budget(1024, 16),
    )
    .expect("request");
    let program = session.begin(&search).await.expect("program");
    let response = operations
        .execute(&search, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(response.contains("d[2]{p,l,c,t}:"));
    assert!(response.contains("Alpha NEEDLE"));
    assert!(response.contains("needle lower"));
}

#[tokio::test]
async fn low_projection_budget_retains_complete_search_evidence() {
    let directory = TestDirectory::new();
    fs::write(
        directory.0.join("many.txt"),
        (0..20)
            .map(|index| format!("needle-{index:02}\n"))
            .collect::<String>(),
    )
    .expect("write");
    let (session, operations) = runtime(&directory);
    let search = Request::new(
        7,
        Arguments::Search(SearchArgs::new("needle", vec![".".to_owned()], 0).expect("search")),
        budget(32, 20),
    )
    .expect("request");
    let program = session.begin(&search).await.expect("program");
    let response = operations
        .execute(&search, &program)
        .await
        .expect("response");
    let reference = response.reference().expect("retained reference");
    assert_eq!(response.flags() & 0b1011, 0b1011);
    let evidence = program
        .store()
        .get(reference)
        .expect("evidence")
        .read_all(8 * 1024 * 1024)
        .await
        .expect("read evidence");
    let evidence = std::str::from_utf8(&evidence).expect("ASON evidence");
    assert!(evidence.contains("d[20]{p,l,c,t}:"));
    assert!(evidence.contains("needle-19"));
}

#[tokio::test]
async fn retained_results_support_slice_search_binary_and_release() {
    let directory = TestDirectory::new();
    let (session, operations) = runtime(&directory);
    let reference = session
        .store()
        .retain(b"zero\r\nNeedle alpha\nlast\n".to_vec())
        .expect("retain text");

    let lines = Request::new(
        20,
        Arguments::Ref(RefArgs::lines(reference, 2, 1).expect("lines")),
        budget(256, 8),
    )
    .expect("request");
    let program = session.begin(&lines).await.expect("program");
    let response = operations
        .execute(&lines, &program)
        .await
        .expect("response");
    assert_eq!(response.reference(), Some(reference));
    let encoded = response.encode().expect("encode").encode();
    assert!(encoded.contains("d{o,n,p,h,t,b}:\n2,13,13,"), "{encoded}");
    assert!(encoded.contains("\"Needle alpha\\n\",~"), "{encoded}");
    drop(program);

    let search = Request::new(
        21,
        Arguments::Ref(
            RefArgs::search(reference, 0, 1024, "needle", REF_CASE_INSENSITIVE).expect("search"),
        ),
        budget(256, 8),
    )
    .expect("request");
    let program = session.begin(&search).await.expect("program");
    let encoded = operations
        .execute(&search, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(
        encoded.contains("d[1]{o,l,c,t}:\n6,2,1,\"Needle alpha\"\n"),
        "{encoded}"
    );
    assert!(encoded.ends_with(&format!("z:8\nr:@{reference}\n")));
    drop(program);

    let binary_reference = session
        .store()
        .retain(vec![0xff, 0x00, 0x10])
        .expect("retain binary");
    let binary = Request::new(
        22,
        Arguments::Ref(RefArgs::bytes(binary_reference, 0, 3).expect("bytes")),
        budget(256, 8),
    )
    .expect("request");
    let program = session.begin(&binary).await.expect("program");
    let encoded = operations
        .execute(&binary, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(encoded.contains(",~,ff0010\n"), "{encoded}");
    drop(program);

    let long_reference = session
        .store()
        .retain("澶氭牳".repeat(5_000).into_bytes())
        .expect("retain long text");
    let projected = Request::new(
        24,
        Arguments::Ref(RefArgs::bytes(long_reference, 0, 128 * 1024).expect("bytes")),
        budget(128, 1),
    )
    .expect("request");
    let program = session.begin(&projected).await.expect("program");
    let response = operations
        .execute(&projected, &program)
        .await
        .expect("response");
    assert_eq!(response.flags() & 0b1011, 0b1011);
    assert_eq!(response.reference(), Some(long_reference));
    assert!(response.encode().expect("encode").encode().len() <= 512);
    drop(program);

    let release = Request::new(
        23,
        Arguments::Ref(RefArgs::release(reference).expect("release")),
        budget(64, 1),
    )
    .expect("request");
    let program = session.begin(&release).await.expect("program");
    let encoded = operations
        .execute(&release, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(encoded.contains(&format!("d{{r,z}}:\n@{reference},1\n")));
    drop(program);
    assert!(session.store().get(reference).is_err());

    let program = session.begin(&release).await.expect("program");
    let encoded = operations
        .execute(&release, &program)
        .await
        .expect("typed failure")
        .encode()
        .expect("encode")
        .encode();
    assert!(encoded.starts_with("t:3\ni:23\ns:4\n"));
    assert!(encoded.contains("e{c,q,p,x,a}:\n700,1,2,~,~\n"));
}

#[tokio::test]
async fn retained_ason_projects_relations_and_materializes_binary_without_overwrite() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.0.join("artifacts")).expect("artifact directory");
    let (session, operations) = runtime(&directory);
    let structured = session
        .store()
        .retain(
            b"k:g\nd[3]{p,l,c,t}:\nsrc/a.rs,1,2,alpha\nsrc/b.rs,2,3,beta\nsrc/c.rs,3,4,gamma\n"
                .to_vec(),
        )
        .expect("retain structured result");

    let project = Request::new(
        25,
        Arguments::Ref(
            RefArgs::project(structured, "d", 1, 2, vec!["p".to_owned(), "t".to_owned()])
                .expect("projection formula"),
        ),
        budget(256, 8),
    )
    .expect("project request");
    let program = session.begin(&project).await.expect("program");
    let response = operations
        .execute(&project, &program)
        .await
        .expect("projection response");
    assert_eq!(response.status(), Status::Success);
    assert_eq!(response.reference(), Some(structured));
    assert_eq!(response.flags(), RESULT_REDUCED | RESULT_RETAINED);
    let encoded = response.encode().expect("encode").encode();
    assert!(
        encoded.contains("d[2]{p,t}:\nsrc/b.rs,beta\nsrc/c.rs,gamma\n"),
        "{encoded}"
    );
    drop(program);

    let bounded = Request::new(
        26,
        Arguments::Ref(
            RefArgs::project(structured, "d", 0, 3, vec!["p".to_owned(), "t".to_owned()])
                .expect("bounded projection formula"),
        ),
        budget(256, 1),
    )
    .expect("bounded project request");
    let program = session.begin(&bounded).await.expect("program");
    let response = operations
        .execute(&bounded, &program)
        .await
        .expect("bounded projection response");
    assert_eq!(
        response.flags(),
        RESULT_TRUNCATED | RESULT_REDUCED | RESULT_RETAINED
    );
    assert_eq!(response.reference(), Some(structured));
    let encoded = response.encode().expect("encode").encode();
    assert!(
        encoded.contains("d[1]{p,t}:\nsrc/a.rs,alpha\n"),
        "{encoded}"
    );
    drop(program);

    let unknown_column = Request::new(
        27,
        Arguments::Ref(
            RefArgs::project(structured, "d", 0, 1, vec!["missing".to_owned()])
                .expect("valid formula shape"),
        ),
        budget(128, 4),
    )
    .expect("unknown column request");
    let program = session.begin(&unknown_column).await.expect("program");
    let response = operations
        .execute(&unknown_column, &program)
        .await
        .expect("typed error");
    assert_eq!(response.status(), Status::InvalidRequest);
    assert_eq!(
        response.error().expect("error").code,
        ErrorCode::InvalidArgument
    );
    drop(program);

    let binary = session
        .store()
        .retain(vec![0xff, 0x00, 0x10])
        .expect("retain binary");
    let denied = Request::new(
        28,
        Arguments::Ref(
            RefArgs::materialize(binary, "artifacts/denied.bin")
                .expect("denied materialize formula"),
        ),
        budget(256, 4),
    )
    .expect("denied materialize request");
    let restricted = PortableOperations::with_authorization(
        Workspace::new(&directory.0).expect("workspace"),
        AuthorizationPolicy::allow(Capability::RetainedResult.mask())
            .expect("retained-only policy"),
    );
    let program = session.begin(&denied).await.expect("program");
    let response = restricted
        .execute(&denied, &program)
        .await
        .expect("denied response");
    assert_eq!(response.status(), Status::Denied);
    assert_eq!(
        response.error().expect("denied error").code,
        ErrorCode::CapabilityDenied
    );
    assert!(!directory.0.join("artifacts/denied.bin").exists());
    drop(program);

    let materialize = |id| {
        Request::new(
            id,
            Arguments::Ref(
                RefArgs::materialize(binary, "artifacts/out.bin").expect("materialize formula"),
            ),
            budget(256, 4),
        )
        .expect("materialize request")
    };
    let first = materialize(29);
    assert_eq!(
        first.required_capabilities(),
        Capability::RetainedResult.mask() | Capability::WorkspaceWrite.mask()
    );
    let program = session.begin(&first).await.expect("program");
    let response = operations
        .execute(&first, &program)
        .await
        .expect("materialize response");
    assert_eq!(response.status(), Status::Success);
    assert_eq!(response.reference(), Some(binary));
    assert_eq!(
        fs::read(directory.0.join("artifacts/out.bin")).expect("artifact"),
        [0xff, 0x00, 0x10]
    );
    let encoded = response.encode().expect("encode").encode();
    assert!(encoded.contains("d{p,s,z,h}:\n1,0,3,"), "{encoded}");
    drop(program);

    let second = materialize(30);
    let program = session.begin(&second).await.expect("program");
    let response = operations
        .execute(&second, &program)
        .await
        .expect("conflict response");
    assert_eq!(response.status(), Status::Conflict);
    assert_eq!(
        response.error().expect("conflict").code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        fs::read(directory.0.join("artifacts/out.bin")).expect("unchanged artifact"),
        [0xff, 0x00, 0x10]
    );
}

#[tokio::test]
async fn workspace_escape_becomes_a_typed_protocol_error() {
    let directory = TestDirectory::new();
    let (session, operations) = runtime(&directory);
    let search = Request::new(
        9,
        Arguments::Search(
            SearchArgs::new("secret", vec!["../outside".to_owned()], 0).expect("search"),
        ),
        budget(256, 16),
    )
    .expect("request");
    let program = session.begin(&search).await.expect("program");
    let response = operations
        .execute(&search, &program)
        .await
        .expect("typed failure")
        .encode()
        .expect("encode")
        .encode();
    assert!(response.contains("s:1\n"));
    assert!(response.contains("e{c,q,p,x,a}:\n100,1,1,~,~\n"));
}

#[tokio::test]
async fn patch_commits_multi_file_byte_edits_and_rejects_stale_preimages() {
    let directory = TestDirectory::new();
    let first_path = directory.0.join("a.txt");
    let second_path = directory.0.join("b.txt");
    fs::write(&first_path, b"hello world\n").expect("write");
    fs::write(&second_path, b"second\n").expect("write");
    let first_digest = blake3::hash(b"hello world\n").to_hex().to_string();
    let second_digest = blake3::hash(b"second\n").to_hex().to_string();
    let (session, operations) = runtime(&directory);
    let reference = session.store().retain(b"RUST".to_vec()).expect("retain");

    let request = Request::new(
        70,
        Arguments::Patch(
            PatchArgs::new(
                vec!["a.txt".to_owned(), "b.txt".to_owned()],
                vec![first_digest.clone(), second_digest],
                vec![
                    PatchEdit::new(0, 6, 5, PatchContent::Inline("ash".to_owned())).expect("edit"),
                    PatchEdit::new(1, 0, 6, PatchContent::Reference(reference)).expect("edit"),
                ],
                0,
            )
            .expect("patch"),
        ),
        budget(2048, 16),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let encoded = operations
        .execute(&request, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(encoded.starts_with("t:3\ni:70\ns:0\n"), "{encoded}");
    assert!(encoded.contains("d[2]{p,s,h}:"), "{encoded}");
    assert_eq!(fs::read(&first_path).expect("read"), b"hello ash\n");
    assert_eq!(fs::read(&second_path).expect("read"), b"RUST\n");
    drop(program);

    fs::write(&first_path, b"external\n").expect("external write");
    let second_now = fs::read(&second_path).expect("read");
    let stale = Request::new(
        71,
        Arguments::Patch(
            PatchArgs::new(
                vec!["a.txt".to_owned(), "b.txt".to_owned()],
                vec![first_digest, blake3::hash(&second_now).to_hex().to_string()],
                vec![
                    PatchEdit::new(0, 0, 1, PatchContent::Inline("A".to_owned())).expect("edit"),
                    PatchEdit::new(1, 0, 1, PatchContent::Inline("B".to_owned())).expect("edit"),
                ],
                0,
            )
            .expect("patch"),
        ),
        budget(2048, 16),
    )
    .expect("request");
    let program = session.begin(&stale).await.expect("program");
    let encoded = operations
        .execute(&stale, &program)
        .await
        .expect("typed conflict")
        .encode()
        .expect("encode")
        .encode();
    assert!(encoded.starts_with("t:3\ni:71\ns:8\n"), "{encoded}");
    assert!(
        encoded.contains("e{c,q,p,x,a}:\n501,1,4,~,~\n"),
        "{encoded}"
    );
    assert_eq!(fs::read(&first_path).expect("read"), b"external\n");
    assert_eq!(fs::read(&second_path).expect("read"), second_now);
}

#[tokio::test]
async fn fs_commits_typed_file_transaction_and_reports_stale_preimages() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("copy-source"), b"copy").expect("copy source");
    fs::write(directory.0.join("move-source"), b"move").expect("move source");
    fs::write(directory.0.join("remove-source"), b"remove").expect("remove source");
    let (session, operations) = runtime(&directory);
    assert_eq!(PortableOperations::operation_mask(), 0x1ff);
    let binary_reference = session
        .store()
        .retain(vec![0xff, 0x00, 0x10])
        .expect("retain binary");
    let request = Request::new(
        75,
        Arguments::Fs(
            FsArgs::new(vec![
                FsAction::new(
                    1,
                    FsActionKind::Create,
                    "created",
                    None,
                    None,
                    Some(PatchContent::Inline("created".to_owned())),
                )
                .expect("create"),
                FsAction::new(
                    2,
                    FsActionKind::Create,
                    "binary",
                    None,
                    None,
                    Some(PatchContent::Reference(binary_reference)),
                )
                .expect("binary create"),
                FsAction::new(
                    3,
                    FsActionKind::Copy,
                    "copy-source",
                    Some("copied".to_owned()),
                    Some(blake3::hash(b"copy").to_hex().to_string()),
                    None,
                )
                .expect("copy"),
                FsAction::new(
                    4,
                    FsActionKind::Move,
                    "move-source",
                    Some("moved".to_owned()),
                    Some(blake3::hash(b"move").to_hex().to_string()),
                    None,
                )
                .expect("move"),
                FsAction::new(
                    5,
                    FsActionKind::Remove,
                    "remove-source",
                    None,
                    Some(blake3::hash(b"remove").to_hex().to_string()),
                    None,
                )
                .expect("remove"),
            ])
            .expect("fs"),
        ),
        budget(4096, 32),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let encoded = operations
        .execute(&request, &program)
        .await
        .expect("response")
        .encode()
        .expect("encode")
        .encode();
    assert!(encoded.starts_with("t:3\ni:75\ns:0\n"), "{encoded}");
    assert!(encoded.contains("d[5]{i,k,p,q,s,h}:"), "{encoded}");
    assert_eq!(
        fs::read(directory.0.join("created")).expect("created"),
        b"created"
    );
    assert_eq!(
        fs::read(directory.0.join("binary")).expect("binary"),
        [0xff, 0x00, 0x10]
    );
    assert_eq!(
        fs::read(directory.0.join("copied")).expect("copied"),
        b"copy"
    );
    assert_eq!(fs::read(directory.0.join("moved")).expect("moved"), b"move");
    assert!(!directory.0.join("move-source").exists());
    assert!(!directory.0.join("remove-source").exists());
    drop(program);

    let stale = Request::new(
        76,
        Arguments::Fs(
            FsArgs::new(vec![
                FsAction::new(
                    1,
                    FsActionKind::Remove,
                    "copy-source",
                    None,
                    Some(blake3::hash(b"stale").to_hex().to_string()),
                    None,
                )
                .expect("stale remove"),
            ])
            .expect("fs"),
        ),
        budget(1024, 8),
    )
    .expect("request");
    let program = session.begin(&stale).await.expect("program");
    let encoded = operations
        .execute(&stale, &program)
        .await
        .expect("typed conflict")
        .encode()
        .expect("encode")
        .encode();
    assert!(encoded.starts_with("t:3\ni:76\ns:8\n"), "{encoded}");
    assert!(encoded.contains("1,3,"), "{encoded}");
    assert!(encoded.contains(",~,1,"), "{encoded}");
    assert!(
        encoded.contains("e{c,q,p,x,a}:\n501,1,4,~,~\n"),
        "{encoded}"
    );
    assert_eq!(
        fs::read(directory.0.join("copy-source")).expect("copy source"),
        b"copy"
    );
}

#[tokio::test]
async fn batch_can_chain_fs_output_into_a_read_node() {
    let directory = TestDirectory::new();
    let (session, operations) = runtime(&directory);
    let nodes = vec![
        BatchNode::new(
            1,
            vec![],
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
        )
        .expect("fs node"),
        BatchNode::new(
            2,
            vec![1],
            Arguments::Read(
                ReadArgs::new(vec!["created.txt".to_owned()], ReadMode::Lines, 1, 1).expect("read"),
            ),
        )
        .expect("read node"),
    ];
    let request = Request::new(
        77,
        Arguments::Batch(BatchArgs::new(nodes).expect("batch")),
        budget(4096, 24),
    )
    .expect("request");
    let program = session.begin(&request).await.expect("program");
    let response = operations
        .execute(&request, &program)
        .await
        .expect("batch response");
    let encoded = response.encode().expect("encode").encode();
    assert!(encoded.starts_with("t:3\ni:77\ns:0\n"), "{encoded}");
    assert!(encoded.contains("d[2]{i,o,s,c,r}:"), "{encoded}");
    let child = program
        .store()
        .get(2)
        .expect("read child")
        .read_all(8 * 1024 * 1024)
        .await
        .expect("read child bytes");
    let child = std::str::from_utf8(&child).expect("ASON");
    assert!(child.contains("created\\n"), "{child}");
}

#[tokio::test]
async fn snapshot_reference_drives_a_compact_workspace_delta() {
    let directory = TestDirectory::new();
    fs::write(directory.0.join("modified.txt"), b"before").expect("write");
    fs::write(directory.0.join("removed.txt"), b"removed").expect("write");
    let (session, operations) = runtime(&directory);
    let capture = Request::new(
        80,
        Arguments::Snapshot(
            SnapshotArgs::new(
                vec![".".to_owned()],
                64,
                SnapshotMode::Capture,
                None,
                SNAPSHOT_INCLUDE_HIDDEN,
            )
            .expect("snapshot"),
        ),
        budget(4096, 32),
    )
    .expect("request");
    let program = session.begin(&capture).await.expect("program");
    let response = operations
        .execute(&capture, &program)
        .await
        .expect("capture");
    let baseline = response.reference().expect("snapshot reference");
    assert_ne!(response.flags() & 0b1000, 0);
    drop(program);

    fs::write(directory.0.join("modified.txt"), b"after").expect("modify");
    fs::remove_file(directory.0.join("removed.txt")).expect("remove");
    fs::write(directory.0.join("added.txt"), b"added").expect("add");
    let delta = Request::new(
        81,
        Arguments::Snapshot(
            SnapshotArgs::new(
                vec![".".to_owned()],
                64,
                SnapshotMode::Delta,
                Some(baseline),
                SNAPSHOT_INCLUDE_HIDDEN,
            )
            .expect("delta"),
        ),
        budget(4096, 32),
    )
    .expect("request");
    let program = session.begin(&delta).await.expect("program");
    let response = operations.execute(&delta, &program).await.expect("delta");
    assert!(response.reference().is_some());
    let encoded = response.encode().expect("encode").encode();
    assert!(encoded.starts_with("t:3\ni:81\ns:0\n"), "{encoded}");
    assert!(encoded.contains("d[3]{p,c,k,z,h}:"), "{encoded}");
    let document = ash_protocol::ason::decode(&encoded).expect("ASON");
    let ash_protocol::ason::Value::Table(changes) = document.get("d").expect("changes") else {
        panic!("snapshot result table")
    };
    let states: Vec<_> = changes
        .rows()
        .iter()
        .map(|row| match &row[1] {
            ash_protocol::ason::Cell::Atom(ash_protocol::ason::Atom::Text(state)) => state.as_str(),
            _ => panic!("state"),
        })
        .collect();
    assert_eq!(states, ["1", "2", "3"]);
}
