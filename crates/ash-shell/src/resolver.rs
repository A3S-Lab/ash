use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{PlatformEnvironment, ShellState};

/// Host semantics relevant to command backend selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum HostPlatform {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl HostPlatform {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// Injectable native executable lookup used by deterministic resolver tests.
pub trait NativeCommandLookup {
    fn resolve(
        &self,
        command: &str,
        cwd: &Path,
        environment: &PlatformEnvironment,
    ) -> Option<PathBuf>;
}

impl<F> NativeCommandLookup for F
where
    F: Fn(&str, &Path, &PlatformEnvironment) -> Option<PathBuf>,
{
    fn resolve(
        &self,
        command: &str,
        cwd: &Path,
        environment: &PlatformEnvironment,
    ) -> Option<PathBuf> {
        self(command, cwd, environment)
    }
}

/// Native `PATH` lookup that never invokes an implicit host shell.
#[derive(Clone, Copy, Debug, Default)]
pub struct PathCommandLookup;

impl NativeCommandLookup for PathCommandLookup {
    fn resolve(
        &self,
        command: &str,
        cwd: &Path,
        environment: &PlatformEnvironment,
    ) -> Option<PathBuf> {
        let requested = Path::new(command);
        if requested.is_absolute() || requested.components().count() > 1 {
            let candidate = if requested.is_absolute() {
                requested.to_owned()
            } else {
                cwd.join(requested)
            };
            return executable_candidate(&candidate);
        }

        let path = environment.get("PATH")?;
        for directory in std::env::split_paths(path) {
            let directory = if directory.as_os_str().is_empty() {
                cwd.to_owned()
            } else if directory.is_absolute() {
                directory
            } else {
                cwd.join(directory)
            };
            if let Some(candidate) = executable_candidate(&directory.join(command)) {
                return Some(candidate);
            }
        }
        None
    }
}

#[cfg(unix)]
fn executable_candidate(candidate: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(candidate).ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then(|| candidate.to_owned())
}

#[cfg(windows)]
fn executable_candidate(candidate: &Path) -> Option<PathBuf> {
    let extension = candidate.extension().and_then(|value| value.to_str());
    if extension
        .is_some_and(|value| value.eq_ignore_ascii_case("exe") || value.eq_ignore_ascii_case("com"))
    {
        return fs::metadata(candidate)
            .ok()
            .is_some_and(|metadata| metadata.is_file())
            .then(|| candidate.to_owned());
    }
    for extension in ["exe", "com"] {
        let candidate = candidate.with_extension(extension);
        if fs::metadata(&candidate)
            .ok()
            .is_some_and(|metadata| metadata.is_file())
        {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(any(unix, windows)))]
fn executable_candidate(candidate: &Path) -> Option<PathBuf> {
    fs::metadata(candidate)
        .ok()
        .is_some_and(|metadata| metadata.is_file())
        .then(|| candidate.to_owned())
}

/// Builtins that must run in the parent shell when used as foreground commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StatefulBuiltin {
    Cd,
    Export,
    Unset,
    Exit,
    Alias,
    Jobs,
    Foreground,
    Background,
}

impl StatefulBuiltin {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cd => "cd",
            Self::Export => "export",
            Self::Unset => "unset",
            Self::Exit => "exit",
            Self::Alias => "alias",
            Self::Jobs => "jobs",
            Self::Foreground => "fg",
            Self::Background => "bg",
        }
    }
}

/// Portable commands in the first native non-interactive milestone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PortableCommand {
    Pwd,
    Echo,
    List,
    Cat,
    Grep,
}

impl PortableCommand {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pwd => "pwd",
            Self::Echo => "echo",
            Self::List => "ls",
            Self::Cat => "cat",
            Self::Grep => "grep",
        }
    }
}

/// Deterministic command category selected before execution or lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResolvedCommand {
    StatefulBuiltin(StatefulBuiltin),
    Alias {
        name: String,
        replacement: String,
    },
    Function {
        name: String,
    },
    Portable(PortableCommand),
    Native {
        executable: PathBuf,
        explicit: bool,
    },
    Wsl {
        command: String,
        distribution: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResolutionError {
    EmptyCommand,
    CommandNotFound { command: String },
    BackendUnavailable { backend: &'static str },
}

impl fmt::Display for ResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => formatter.write_str("command name is empty"),
            Self::CommandNotFound { command } => {
                write!(formatter, "native command `{command}` was not found")
            }
            Self::BackendUnavailable { backend } => {
                write!(formatter, "the explicit `{backend}` backend is unavailable")
            }
        }
    }
}

impl std::error::Error for ResolutionError {}

/// Resolves one already-expanded command name in the documented precedence order.
pub struct CommandResolver<'a, L> {
    state: &'a ShellState,
    lookup: L,
    host: HostPlatform,
}

impl<'a, L> CommandResolver<'a, L>
where
    L: NativeCommandLookup,
{
    #[must_use]
    pub fn new(state: &'a ShellState, lookup: L) -> Self {
        Self {
            state,
            lookup,
            host: HostPlatform::current(),
        }
    }

    #[must_use]
    pub const fn for_platform(state: &'a ShellState, lookup: L, host: HostPlatform) -> Self {
        Self {
            state,
            lookup,
            host,
        }
    }

    pub fn resolve(&self, command: &str) -> Result<ResolvedCommand, ResolutionError> {
        if command.is_empty() {
            return Err(ResolutionError::EmptyCommand);
        }
        if let Some(builtin) = stateful_builtin(command) {
            return Ok(ResolvedCommand::StatefulBuiltin(builtin));
        }
        if let Some(replacement) = self.state.alias(command) {
            return Ok(ResolvedCommand::Alias {
                name: command.to_owned(),
                replacement: replacement.to_owned(),
            });
        }
        if self.state.function(command).is_some() {
            return Ok(ResolvedCommand::Function {
                name: command.to_owned(),
            });
        }
        if let Some(portable) = portable_command(command) {
            return Ok(ResolvedCommand::Portable(portable));
        }
        if let Some(native) = command.strip_prefix("native:") {
            if native.is_empty() {
                return Err(ResolutionError::EmptyCommand);
            }
            return self.resolve_native(native, true);
        }
        if let Some(linux) = command.strip_prefix("linux:") {
            if linux.is_empty() {
                return Err(ResolutionError::EmptyCommand);
            }
            if self.host != HostPlatform::Windows {
                return Err(ResolutionError::BackendUnavailable { backend: "linux" });
            }
            return Ok(ResolvedCommand::Wsl {
                command: linux.to_owned(),
                distribution: self.state.options().wsl_distribution().map(str::to_owned),
            });
        }
        self.resolve_native(command, false)
    }

    fn resolve_native(
        &self,
        command: &str,
        explicit: bool,
    ) -> Result<ResolvedCommand, ResolutionError> {
        self.lookup
            .resolve(command, self.state.cwd(), self.state.environment())
            .map(|executable| ResolvedCommand::Native {
                executable,
                explicit,
            })
            .ok_or_else(|| ResolutionError::CommandNotFound {
                command: command.to_owned(),
            })
    }
}

fn stateful_builtin(command: &str) -> Option<StatefulBuiltin> {
    match command {
        "cd" => Some(StatefulBuiltin::Cd),
        "export" => Some(StatefulBuiltin::Export),
        "unset" => Some(StatefulBuiltin::Unset),
        "exit" => Some(StatefulBuiltin::Exit),
        "alias" => Some(StatefulBuiltin::Alias),
        "jobs" => Some(StatefulBuiltin::Jobs),
        "fg" => Some(StatefulBuiltin::Foreground),
        "bg" => Some(StatefulBuiltin::Background),
        _ => None,
    }
}

fn portable_command(command: &str) -> Option<PortableCommand> {
    match command {
        "pwd" => Some(PortableCommand::Pwd),
        "echo" => Some(PortableCommand::Echo),
        "ls" => Some(PortableCommand::List),
        "cat" => Some(PortableCommand::Cat),
        "grep" => Some(PortableCommand::Grep),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        CommandResolver, HostPlatform, PathCommandLookup, PortableCommand, ResolutionError,
        ResolvedCommand, StatefulBuiltin,
    };
    use crate::{PlatformEnvironment, ShellFunction, ShellState, parse};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("ash-shell-resolver-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn lookup(command: &str, _cwd: &Path, _environment: &PlatformEnvironment) -> Option<PathBuf> {
        (command == "cargo").then(|| PathBuf::from("/fixture/bin/cargo"))
    }

    #[test]
    fn precedence_keeps_builtins_aliases_and_functions_ahead_of_portable_commands() {
        let mut state = ShellState::new("/fixture");
        state.set_alias("pwd", "echo alias").expect("alias");
        state
            .set_function(
                "echo",
                ShellFunction::new(parse("echo function").expect("function body")),
            )
            .expect("function");
        let resolver = CommandResolver::for_platform(&state, lookup, HostPlatform::Linux);

        assert_eq!(
            resolver.resolve("cd"),
            Ok(ResolvedCommand::StatefulBuiltin(StatefulBuiltin::Cd))
        );
        assert!(matches!(
            resolver.resolve("pwd"),
            Ok(ResolvedCommand::Alias { .. })
        ));
        assert_eq!(
            resolver.resolve("echo"),
            Ok(ResolvedCommand::Function {
                name: "echo".to_owned()
            })
        );
        assert_eq!(
            resolver.resolve("cat"),
            Ok(ResolvedCommand::Portable(PortableCommand::Cat))
        );
    }

    #[test]
    fn native_and_linux_backends_are_always_explicit() {
        let mut state = ShellState::new("/fixture");
        state
            .options_mut()
            .set_wsl_distribution(Some("Ubuntu".to_owned()));
        let windows = CommandResolver::for_platform(&state, lookup, HostPlatform::Windows);
        assert_eq!(
            windows.resolve("native:cargo"),
            Ok(ResolvedCommand::Native {
                executable: PathBuf::from("/fixture/bin/cargo"),
                explicit: true,
            })
        );
        assert_eq!(
            windows.resolve("linux:make"),
            Ok(ResolvedCommand::Wsl {
                command: "make".to_owned(),
                distribution: Some("Ubuntu".to_owned()),
            })
        );
        assert_eq!(
            windows.resolve("missing"),
            Err(ResolutionError::CommandNotFound {
                command: "missing".to_owned()
            })
        );

        let linux = CommandResolver::for_platform(&state, lookup, HostPlatform::Linux);
        assert_eq!(
            linux.resolve("linux:make"),
            Err(ResolutionError::BackendUnavailable { backend: "linux" })
        );
    }

    #[test]
    fn path_lookup_resolves_executables_without_a_shell() {
        let directory = TestDirectory::new();
        let bin = directory.0.join("bin");
        fs::create_dir(&bin).expect("bin directory");
        let name = if cfg!(windows) { "tool.exe" } else { "tool" };
        let executable = bin.join(name);
        fs::write(&executable, b"fixture").expect("executable fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("executable permissions");
        }

        let mut state = ShellState::new(&directory.0);
        state.environment_mut().insert("PATH", "bin");
        let resolver = CommandResolver::new(&state, PathCommandLookup);
        assert_eq!(
            resolver.resolve("tool"),
            Ok(ResolvedCommand::Native {
                executable,
                explicit: false,
            })
        );
    }
}
