#![cfg(windows)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use a3s_ash_shell::{ExecutionBackend, ShellState, execute_source};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("ash-wsl-stream-{}-{id}", std::process::id()))
            .join("fixture with spaces 项目");
        fs::create_dir_all(&path).expect("create WSL fixture directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Some(root) = self.0.parent() {
            let _ = fs::remove_dir_all(root);
        }
    }
}

#[tokio::test]
async fn configured_wsl_distribution_streams_windows_file_redirections() {
    let Some(distribution) =
        std::env::var_os("ASH_TEST_WSL_DISTRIBUTION").filter(|value| !value.is_empty())
    else {
        return;
    };
    let distribution = distribution
        .into_string()
        .expect("ASH_TEST_WSL_DISTRIBUTION must be valid Unicode");
    let directory = TestDirectory::new();
    const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
    let payload = (0..PAYLOAD_BYTES)
        .map(|index| u8::try_from(index % 251).expect("bounded payload byte"))
        .collect::<Vec<_>>();
    fs::write(directory.0.join("input.bin"), payload).expect("write WSL input fixture");

    let mut state = ShellState::from_process().expect("process shell state");
    state.set_cwd(&directory.0);
    state
        .options_mut()
        .set_wsl_distribution(Some(distribution.clone()));
    let execution =
        execute_source("linux:cat <input.bin | linux:wc -c >count.txt", &mut state).await;

    assert!(execution.stdout().is_empty());
    assert!(execution.stderr().is_empty());
    assert!(execution.diagnostics().is_empty());
    assert_eq!(execution.status().code(), 0);
    assert_eq!(
        execution.status().backend(),
        &ExecutionBackend::Wsl {
            distribution: Some(distribution)
        }
    );
    let count = fs::read_to_string(directory.0.join("count.txt")).expect("read WSL count");
    assert_eq!(
        count.trim().parse::<usize>().expect("numeric WSL count"),
        PAYLOAD_BYTES
    );
}
