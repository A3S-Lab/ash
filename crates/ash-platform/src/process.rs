use std::ffi::OsString;
use std::process::Stdio;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::{PlatformError, Workspace};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentChange {
    Set(OsString, OsString),
    Remove(OsString),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    pub executable: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub environment: Vec<EnvironmentChange>,
    pub clear_environment: bool,
    pub pipe_stdin: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    pub success: bool,
    pub code: Option<i64>,
    pub signal: Option<i64>,
}

pub struct ProcessHandle {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl Workspace {
    pub fn spawn(&self, spec: &ProcessSpec) -> Result<ProcessHandle, PlatformError> {
        validate_process_spec(spec)?;
        let cwd = self.resolve_existing(&spec.cwd)?;
        let executable = if spec.executable.contains('/') {
            self.resolve_existing(&spec.executable)?.native
        } else if spec.executable.contains(['\\', ':']) || spec.executable.is_empty() {
            return Err(PlatformError::InvalidLogicalPath);
        } else {
            spec.executable.clone().into()
        };
        let mut command = Command::new(executable);
        command
            .args(&spec.argv)
            .current_dir(cwd.native)
            .stdin(if spec.pipe_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if spec.clear_environment {
            command.env_clear();
        }
        for change in &spec.environment {
            match change {
                EnvironmentChange::Set(name, value) => {
                    command.env(name, value);
                }
                EnvironmentChange::Remove(name) => {
                    command.env_remove(name);
                }
            }
        }
        let mut child = command.spawn()?;
        Ok(ProcessHandle {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child,
        })
    }
}

fn validate_process_spec(spec: &ProcessSpec) -> Result<(), PlatformError> {
    if spec.executable.contains('\0') || spec.argv.iter().any(|argument| argument.contains('\0')) {
        return Err(PlatformError::InvalidLogicalPath);
    }
    for change in &spec.environment {
        let (name, value) = match change {
            EnvironmentChange::Set(name, value) => (name, Some(value)),
            EnvironmentChange::Remove(name) => (name, None),
        };
        let Some(name) = name.to_str() else {
            return Err(PlatformError::InvalidEnvironment);
        };
        let mut bytes = name.bytes();
        if !matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || value.is_some_and(|value| value.to_string_lossy().contains('\0'))
        {
            return Err(PlatformError::InvalidEnvironment);
        }
    }
    Ok(())
}

impl ProcessHandle {
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub async fn wait(&mut self) -> Result<ProcessExit, PlatformError> {
        let status = self.child.wait().await?;
        Ok(normalize_exit(status))
    }

    pub async fn terminate(&mut self) -> Result<(), PlatformError> {
        match self.child.kill().await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(unix)]
fn normalize_exit(status: std::process::ExitStatus) -> ProcessExit {
    use std::os::unix::process::ExitStatusExt;

    ProcessExit {
        success: status.success(),
        code: status.code().map(i64::from),
        signal: status.signal().map(i64::from),
    }
}

#[cfg(windows)]
fn normalize_exit(status: std::process::ExitStatus) -> ProcessExit {
    ProcessExit {
        success: status.success(),
        code: status.code().map(i64::from),
        signal: None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tokio::io::AsyncReadExt;

    use super::ProcessSpec;
    use crate::Workspace;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-process-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn process_is_spawned_directly_with_piped_machine_output() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let mut process = workspace
            .spawn(&ProcessSpec {
                executable: "rustc".to_owned(),
                argv: vec!["--version".to_owned()],
                cwd: ".".to_owned(),
                environment: vec![],
                clear_environment: false,
                pipe_stdin: false,
            })
            .expect("spawn rustc");
        let mut stdout = process.take_stdout().expect("stdout");
        let mut stderr = process.take_stderr().expect("stderr");
        let (stdout, stderr, exit) = tokio::join!(
            async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.expect("stdout");
                bytes
            },
            async {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.expect("stderr");
                bytes
            },
            process.wait(),
        );
        assert!(exit.expect("wait").success, "stderr={stderr:?}");
        assert!(stdout.starts_with(b"rustc "));
    }
}
