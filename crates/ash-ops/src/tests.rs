use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ash_engine::{Engine, Parallelism, SessionConfig};
use ash_platform::Workspace;
use ash_protocol::request::{
    Arguments, BatchArgs, BatchNode, Budget, EXEC_CLEAR_ENVIRONMENT, ExecArgs, InputSource,
    LIST_FILES_ONLY, ListArgs, PatchArgs, PatchContent, PatchEdit, REF_CASE_INSENSITIVE, ReadArgs,
    ReadMode, RefArgs, RefMode, Request, SEARCH_CASE_INSENSITIVE, SNAPSHOT_INCLUDE_HIDDEN,
    SearchArgs, SnapshotArgs, SnapshotMode,
};

use super::PortableOperations;

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

fn compile_helper(directory: &TestDirectory) -> String {
    let bin = directory.0.join("bin");
    fs::create_dir(&bin).expect("create bin");
    let source = directory.0.join("helper.rs");
    fs::write(
        &source,
        r#"
use std::io::{self, Read};
use std::time::Duration;

fn main() {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("fail") => {
            eprintln!("bad");
            std::process::exit(7);
        }
        Some("wait") => std::thread::sleep(Duration::from_secs(5)),
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

    let failed = program.store().get(4).expect("failed child response");
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
    let evidence = program.store().get(reference).expect("evidence");
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
        Arguments::Ref(RefArgs::new(reference, RefMode::Lines, 2, 1, None, 0).expect("lines")),
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
            RefArgs::new(
                reference,
                RefMode::Search,
                0,
                1024,
                Some("needle".to_owned()),
                REF_CASE_INSENSITIVE,
            )
            .expect("search"),
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
        Arguments::Ref(
            RefArgs::new(binary_reference, RefMode::Bytes, 0, 3, None, 0).expect("bytes"),
        ),
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
        Arguments::Ref(
            RefArgs::new(long_reference, RefMode::Bytes, 0, 128 * 1024, None, 0).expect("bytes"),
        ),
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
        Arguments::Ref(RefArgs::new(reference, RefMode::Release, 0, 0, None, 0).expect("release")),
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
