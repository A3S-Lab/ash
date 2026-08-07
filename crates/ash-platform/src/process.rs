use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

use crate::{PlatformError, Workspace};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentChange {
    Set(OsString, OsString),
    Remove(OsString),
}

/// Explicit standard-I/O mode for one child-process stream.
///
/// A piped endpoint exposes its corresponding handle through [`ProcessHandle`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProcessStdio {
    /// Reuse the parent's corresponding standard handle.
    Inherit,
    /// Attach the platform null device.
    Null,
    /// Create an operating-system pipe owned by [`ProcessHandle`].
    Piped,
}

impl ProcessStdio {
    fn into_stdio(self) -> Stdio {
        match self {
            Self::Inherit => Stdio::inherit(),
            Self::Null => Stdio::null(),
            Self::Piped => Stdio::piped(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    pub executable: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub environment: Vec<EnvironmentChange>,
    pub clear_environment: bool,
    pub stdin: ProcessStdio,
    pub stdout: ProcessStdio,
    pub stderr: ProcessStdio,
}

/// Native-string process description for host-authority frontends.
///
/// Unlike [`ProcessSpec`], paths are already resolved native paths and are not
/// interpreted relative to an ASH workspace capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProcessSpec {
    pub executable: OsString,
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: Vec<EnvironmentChange>,
    pub clear_environment: bool,
    pub stdin: ProcessStdio,
    pub stdout: ProcessStdio,
    pub stderr: ProcessStdio,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    pub success: bool,
    pub code: Option<i64>,
    pub signal: Option<i64>,
}

pub struct ProcessHandle {
    child: Box<dyn ChildWrapper>,
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
        spawn_native(&NativeProcessSpec {
            executable: executable.into_os_string(),
            argv: spec.argv.iter().map(OsString::from).collect(),
            cwd: cwd.native,
            environment: spec.environment.clone(),
            clear_environment: spec.clear_environment,
            stdin: spec.stdin,
            stdout: spec.stdout,
            stderr: spec.stderr,
        })
    }
}

/// Launches one host executable directly from an already-resolved native
/// specification. No command shell or workspace path interpretation is added.
pub fn spawn_native(spec: &NativeProcessSpec) -> Result<ProcessHandle, PlatformError> {
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.argv)
        .current_dir(&spec.cwd)
        .stdin(spec.stdin.into_stdio())
        .stdout(spec.stdout.into_stdio())
        .stderr(spec.stderr.into_stdio());
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
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    Ok(ProcessHandle {
        child: command.spawn()?,
    })
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
        self.child.stdin().take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout().take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr().take()
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
        match Box::into_pin(self.child.kill()).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // `start_kill` is overridden by the Unix process-group and Windows
        // Job Object wrappers, so aborting an async request cannot orphan its
        // descendants even though Drop itself cannot await reaping.
        let _ = self.child.start_kill();
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
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{EnvironmentChange, NativeProcessSpec, ProcessSpec, ProcessStdio, spawn_native};
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

    fn compile_process_tree_helper(directory: &TestDirectory) -> String {
        let bin_directory = directory.0.join("bin");
        fs::create_dir(&bin_directory).expect("create bin directory");
        let source = directory.0.join("process-tree-helper.rs");
        fs::write(
            &source,
            r#"
use std::{env, fs, io, io::Write, process::Command, thread, time::Duration};

fn main() {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        Some("inspect") => {
            let argument = arguments.next().expect("argument");
            let cwd = env::current_dir().expect("current directory");
            println!("cwd={}", cwd.file_name().expect("directory name").to_string_lossy());
            println!("token={}", env::var("ASH_NATIVE_TOKEN").expect("environment"));
            println!("argument={argument}");
            eprintln!("native-stderr");
        }
        Some("copy") => {
            let mut input = io::stdin().lock();
            let mut output = io::stdout().lock();
            io::copy(&mut input, &mut output).expect("copy stdin to stdout");
            output.flush().expect("flush stdout");
        }
        Some("quiet") => {}
        Some("parent") => {
            let ready = arguments.next().expect("ready path");
            let escaped = arguments.next().expect("escaped path");
            let _child = Command::new(env::current_exe().expect("current executable"))
                .arg("child")
                .arg(escaped)
                .spawn()
                .expect("spawn descendant");
            fs::write(ready, b"ready").expect("write ready marker");
            thread::sleep(Duration::from_secs(10));
        }
        Some("child") => {
            let escaped = arguments.next().expect("escaped path");
            thread::sleep(Duration::from_secs(1));
            fs::write(escaped, b"escaped").expect("write escaped marker");
            thread::sleep(Duration::from_secs(10));
        }
        _ => panic!("unknown helper mode"),
    }
}
"#,
        )
        .expect("write helper source");
        let executable_name = if cfg!(windows) {
            "process-tree-helper.exe"
        } else {
            "process-tree-helper"
        };
        let executable = bin_directory.join(executable_name);
        let status = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .expect("run rustc");
        assert!(status.success(), "compile process-tree helper");
        format!("bin/{executable_name}")
    }

    #[tokio::test]
    async fn native_process_spec_preserves_cwd_environment_and_argv() {
        let directory = TestDirectory::new();
        let executable = directory.0.join(compile_process_tree_helper(&directory));
        let cwd = directory.0.join("native-cwd");
        fs::create_dir(&cwd).expect("native cwd");
        let mut process = spawn_native(&NativeProcessSpec {
            executable: executable.into_os_string(),
            argv: vec![OsString::from("inspect"), OsString::from("alpha beta")],
            cwd,
            environment: vec![EnvironmentChange::Set(
                OsString::from("ASH_NATIVE_TOKEN"),
                OsString::from("present"),
            )],
            clear_environment: true,
            stdin: ProcessStdio::Null,
            stdout: ProcessStdio::Piped,
            stderr: ProcessStdio::Piped,
        })
        .expect("spawn native helper");
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
        assert_eq!(
            stdout,
            b"cwd=native-cwd\ntoken=present\nargument=alpha beta\n"
        );
        assert_eq!(stderr, b"native-stderr\n");
    }

    #[tokio::test]
    async fn explicit_stdio_endpoints_stream_and_expose_only_piped_handles() {
        let directory = TestDirectory::new();
        let executable = directory.0.join(compile_process_tree_helper(&directory));
        let mut process = spawn_native(&NativeProcessSpec {
            executable: executable.clone().into_os_string(),
            argv: vec![OsString::from("copy")],
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            stdin: ProcessStdio::Piped,
            stdout: ProcessStdio::Piped,
            stderr: ProcessStdio::Null,
        })
        .expect("spawn streaming helper");
        let mut stdin = process.take_stdin().expect("piped stdin");
        let mut stdout = process.take_stdout().expect("piped stdout");
        assert!(process.take_stderr().is_none(), "null stderr has no handle");
        let expected: Vec<u8> = (0..8 * 1024 * 1024)
            .map(|index| u8::try_from(index % 251).expect("bounded byte"))
            .collect();
        let input = expected.clone();

        let writer = async move {
            stdin.write_all(&input).await.expect("write stdin");
            stdin.shutdown().await.expect("close stdin");
            drop(stdin);
        };
        let reader = async {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.expect("read stdout");
            bytes
        };
        let ((), actual, exit) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(writer, reader, process.wait())
        })
        .await
        .expect("streaming helper completes without deadlock");
        assert!(exit.expect("wait for streaming helper").success);
        assert_eq!(actual.len(), expected.len());
        assert!(actual == expected, "streaming payload changed");

        let mut inherited = spawn_native(&NativeProcessSpec {
            executable: executable.into_os_string(),
            argv: vec![OsString::from("quiet")],
            cwd: directory.0.clone(),
            environment: vec![],
            clear_environment: false,
            stdin: ProcessStdio::Inherit,
            stdout: ProcessStdio::Inherit,
            stderr: ProcessStdio::Inherit,
        })
        .expect("spawn inherited-stdio helper");
        assert!(inherited.take_stdin().is_none());
        assert!(inherited.take_stdout().is_none());
        assert!(inherited.take_stderr().is_none());
        assert!(
            inherited
                .wait()
                .await
                .expect("wait for quiet helper")
                .success
        );
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
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Piped,
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

    #[tokio::test]
    async fn terminating_a_process_terminates_its_descendants() {
        let directory = TestDirectory::new();
        let executable = compile_process_tree_helper(&directory);
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let mut process = workspace
            .spawn(&ProcessSpec {
                executable,
                argv: vec![
                    "parent".to_owned(),
                    "ready".to_owned(),
                    "escaped".to_owned(),
                ],
                cwd: ".".to_owned(),
                environment: vec![],
                clear_environment: false,
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Piped,
            })
            .expect("spawn process tree");

        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.0.join("ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant should start");
        process.terminate().await.expect("terminate process tree");
        tokio::time::sleep(Duration::from_millis(1_200)).await;

        assert!(
            !directory.0.join("escaped").exists(),
            "descendant survived process-tree termination"
        );
    }

    #[tokio::test]
    async fn dropping_a_process_handle_terminates_its_descendants() {
        let directory = TestDirectory::new();
        let executable = compile_process_tree_helper(&directory);
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let process = workspace
            .spawn(&ProcessSpec {
                executable,
                argv: vec![
                    "parent".to_owned(),
                    "ready".to_owned(),
                    "escaped".to_owned(),
                ],
                cwd: ".".to_owned(),
                environment: vec![],
                clear_environment: false,
                stdin: ProcessStdio::Null,
                stdout: ProcessStdio::Piped,
                stderr: ProcessStdio::Piped,
            })
            .expect("spawn process tree");

        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.0.join("ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant should start");
        drop(process);
        tokio::time::sleep(Duration::from_millis(1_200)).await;

        assert!(
            !directory.0.join("escaped").exists(),
            "descendant survived process-handle drop"
        );
    }
}
