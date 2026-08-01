use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ash_engine::{Engine, Parallelism, SessionConfig};
use ash_platform::Workspace;
use ash_protocol::request::{
    Arguments, Budget, LIST_FILES_ONLY, ListArgs, ReadArgs, ReadMode, Request,
    SEARCH_CASE_INSENSITIVE, SearchArgs,
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
