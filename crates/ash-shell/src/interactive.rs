use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rustyline::error::ReadlineError;
use rustyline::{Config, DefaultEditor};

use crate::PlatformEnvironment;

/// Default prompt used when `ASH_PROMPT` is not present.
pub const DEFAULT_INTERACTIVE_PROMPT: &str = "ash> ";

/// Process-derived configuration for one interactive shell session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractiveConfig {
    prompt: String,
    history_path: Option<PathBuf>,
    warnings: Vec<String>,
}

impl InteractiveConfig {
    /// Creates an explicit interactive configuration.
    #[must_use]
    pub fn new(prompt: impl Into<String>, history_path: Option<PathBuf>) -> Self {
        Self {
            prompt: prompt.into(),
            history_path,
            warnings: Vec::new(),
        }
    }

    /// Resolves prompt and history configuration from the shell environment.
    ///
    /// A relative `ASH_HISTORY` path is anchored to `initial_cwd`, so a startup
    /// profile cannot redirect history persistence by changing the shell cwd.
    pub fn from_environment(
        environment: &PlatformEnvironment,
        initial_cwd: &Path,
    ) -> Result<Self, InteractiveError> {
        let prompt = environment
            .get("ASH_PROMPT")
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(InteractiveError::PromptNotUtf8)
            })
            .transpose()?
            .unwrap_or_else(|| DEFAULT_INTERACTIVE_PROMPT.to_owned());
        let (history_path, warning) = history_path(environment, initial_cwd);
        Ok(Self {
            prompt,
            history_path,
            warnings: warning.into_iter().collect(),
        })
    }

    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    #[must_use]
    pub fn history_path(&self) -> Option<&Path> {
        self.history_path.as_deref()
    }
}

/// One terminal event returned by the interactive line editor.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InteractiveEvent {
    Line(String),
    Interrupted,
    EndOfFile,
}

/// Fatal interactive-editor configuration or input error.
#[derive(Debug)]
#[non_exhaustive]
pub enum InteractiveError {
    PromptNotUtf8,
    Readline(ReadlineError),
}

impl fmt::Display for InteractiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PromptNotUtf8 => formatter.write_str("ASH_PROMPT must be valid UTF-8"),
            Self::Readline(error) => write!(formatter, "interactive input failed: {error}"),
        }
    }
}

impl std::error::Error for InteractiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PromptNotUtf8 => None,
            Self::Readline(error) => Some(error),
        }
    }
}

/// Cross-platform line editor with optional, safety-checked file history.
pub struct InteractiveEditor {
    editor: DefaultEditor,
    prompt: String,
    history_path: Option<PathBuf>,
    warnings: Vec<String>,
}

impl InteractiveEditor {
    pub fn new(config: InteractiveConfig) -> Result<Self, InteractiveError> {
        let line_config = Config::builder().auto_add_history(false).build();
        let mut editor =
            DefaultEditor::with_config(line_config).map_err(InteractiveError::Readline)?;
        let mut history_path = config.history_path;
        let mut warnings = config.warnings;

        if let Some(path) = history_path.as_deref() {
            match prepare_history_path(path) {
                Ok(()) => {
                    if let Err(error) = editor.load_history(path) {
                        let _ = editor.clear_history();
                        warnings.push(history_warning(path, &error));
                        history_path = None;
                    }
                }
                Err(error) => {
                    warnings.push(history_warning(path, &error));
                    history_path = None;
                }
            }
        }

        Ok(Self {
            editor,
            prompt: config.prompt,
            history_path,
            warnings,
        })
    }

    /// Reads one edited line or normalized terminal-control event.
    pub fn read_line(&mut self) -> Result<InteractiveEvent, InteractiveError> {
        match self.editor.readline(&self.prompt) {
            Ok(line) => {
                self.remember_history(&line);
                Ok(InteractiveEvent::Line(line))
            }
            Err(ReadlineError::Interrupted) => Ok(InteractiveEvent::Interrupted),
            Err(ReadlineError::Eof) => Ok(InteractiveEvent::EndOfFile),
            Err(error) => Err(InteractiveError::Readline(error)),
        }
    }

    /// Drains non-fatal startup or persistence warnings accumulated so far.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    fn remember_history(&mut self, line: &str) {
        if line.starts_with([' ', '\t']) || line.is_empty() {
            return;
        }
        if let Err(error) = self.editor.add_history_entry(line) {
            self.warnings
                .push(format!("interactive history is unavailable: {error}"));
            self.history_path = None;
            return;
        }

        let Some(path) = self.history_path.clone() else {
            return;
        };
        let result = prepare_history_path(&path)
            .and_then(|()| self.editor.append_history(&path).map_err(readline_io_error))
            .and_then(|()| harden_history_file(&path));
        if let Err(error) = result {
            self.warnings.push(history_warning(&path, &error));
            self.history_path = None;
        }
    }
}

fn readline_io_error(error: ReadlineError) -> io::Error {
    io::Error::other(error)
}

fn history_warning(path: &Path, error: &dyn fmt::Display) -> String {
    format!(
        "persistent history at `{}` is disabled: {error}",
        path.display()
    )
}

fn history_path(
    environment: &PlatformEnvironment,
    initial_cwd: &Path,
) -> (Option<PathBuf>, Option<String>) {
    if let Some(configured) = environment.get("ASH_HISTORY") {
        if configured.is_empty() {
            return (None, None);
        }
        return (
            Some(resolve_from_initial_cwd(
                PathBuf::from(configured),
                initial_cwd,
            )),
            None,
        );
    }
    default_history_path(environment, initial_cwd)
}

#[cfg(windows)]
fn default_history_path(
    environment: &PlatformEnvironment,
    initial_cwd: &Path,
) -> (Option<PathBuf>, Option<String>) {
    let Some(base) = environment
        .get("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
    else {
        return (
            None,
            Some("persistent history is disabled because LOCALAPPDATA is unavailable".to_owned()),
        );
    };
    let path = PathBuf::from(base).join("ash").join("history");
    (Some(resolve_from_initial_cwd(path, initial_cwd)), None)
}

#[cfg(not(windows))]
fn default_history_path(
    environment: &PlatformEnvironment,
    initial_cwd: &Path,
) -> (Option<PathBuf>, Option<String>) {
    let base = environment
        .get("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            environment
                .get("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local").join("state"))
        });
    let Some(base) = base else {
        return (
            None,
            Some(
                "persistent history is disabled because XDG_STATE_HOME and HOME are unavailable"
                    .to_owned(),
            ),
        );
    };
    let path = base.join("ash").join("history");
    (Some(resolve_from_initial_cwd(path, initial_cwd)), None)
}

fn resolve_from_initial_cwd(path: PathBuf, initial_cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        initial_cwd.join(path)
    }
}

fn prepare_history_path(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "history path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;

    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_history_metadata(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match create_private_history_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(path)?;
                    validate_history_metadata(path, &metadata)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn validate_history_metadata(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history path is a symbolic link",
        ));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history path is not a regular file",
        ));
    }
    harden_history_file(path)
}

fn create_private_history_file(path: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    options.open(path)?;
    harden_history_file(path)
}

#[cfg(unix)]
fn harden_history_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history path stopped being a regular file",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn harden_history_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history path stopped being a regular file",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{InteractiveConfig, InteractiveEditor, InteractiveError};
    use crate::PlatformEnvironment;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-interactive-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn environment_selects_prompt_and_explicit_history() {
        let directory = TestDirectory::new();
        let mut environment = PlatformEnvironment::new();
        environment.insert("ASH_PROMPT", "ready> ");
        environment.insert("ASH_HISTORY", "state/history");

        let config = InteractiveConfig::from_environment(&environment, directory.path())
            .expect("configuration");

        assert_eq!(config.prompt(), "ready> ");
        assert_eq!(
            config.history_path(),
            Some(directory.path().join("state/history").as_path())
        );
    }

    #[test]
    fn empty_explicit_history_disables_persistence_without_warning() {
        let directory = TestDirectory::new();
        let mut environment = PlatformEnvironment::new();
        environment.insert("ASH_HISTORY", "");

        let config = InteractiveConfig::from_environment(&environment, directory.path())
            .expect("configuration");

        assert_eq!(config.history_path(), None);
        assert!(config.warnings.is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn default_history_prefers_xdg_state_home_then_home() {
        let directory = TestDirectory::new();
        let mut environment = PlatformEnvironment::new();
        environment.insert("HOME", directory.path().join("home"));
        environment.insert("XDG_STATE_HOME", directory.path().join("xdg"));
        let xdg = InteractiveConfig::from_environment(&environment, directory.path())
            .expect("XDG configuration");
        assert_eq!(
            xdg.history_path(),
            Some(directory.path().join("xdg/ash/history").as_path())
        );

        environment.remove("XDG_STATE_HOME");
        let home = InteractiveConfig::from_environment(&environment, directory.path())
            .expect("HOME configuration");
        assert_eq!(
            home.history_path(),
            Some(
                directory
                    .path()
                    .join("home/.local/state/ash/history")
                    .as_path()
            )
        );
    }

    #[cfg(windows)]
    #[test]
    fn default_history_uses_local_app_data() {
        let directory = TestDirectory::new();
        let mut environment = PlatformEnvironment::new();
        environment.insert("LOCALAPPDATA", directory.path().join("local"));
        let config = InteractiveConfig::from_environment(&environment, directory.path())
            .expect("configuration");
        assert_eq!(
            config.history_path(),
            Some(directory.path().join("local/ash/history").as_path())
        );
    }

    #[cfg(unix)]
    #[test]
    fn prompt_rejects_non_utf8_environment_values() {
        use std::os::unix::ffi::OsStringExt;

        let mut environment = PlatformEnvironment::new();
        environment.insert("ASH_PROMPT", OsString::from_vec(vec![0xff]));
        assert!(matches!(
            InteractiveConfig::from_environment(&environment, Path::new(".")),
            Err(InteractiveError::PromptNotUtf8)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn prompt_rejects_non_utf8_environment_values() {
        use std::os::windows::ffi::OsStringExt;

        let mut environment = PlatformEnvironment::new();
        environment.insert("ASH_PROMPT", OsString::from_wide(&[0xd800]));
        assert!(matches!(
            InteractiveConfig::from_environment(&environment, Path::new(".")),
            Err(InteractiveError::PromptNotUtf8)
        ));
    }

    #[test]
    fn history_persists_regular_lines_but_suppresses_leading_whitespace() {
        let directory = TestDirectory::new();
        let history = directory.path().join("history");
        let mut editor =
            InteractiveEditor::new(InteractiveConfig::new("ash> ", Some(history.clone())))
                .expect("editor");

        editor.remember_history("echo visible");
        editor.remember_history(" echo secret");
        editor.remember_history("\techo secret-too");

        let persisted = fs::read_to_string(&history).expect("history file");
        assert!(persisted.contains("echo visible"));
        assert!(!persisted.contains("secret"));
        assert!(editor.take_warnings().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn history_file_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TestDirectory::new();
        let history = directory.path().join("history");
        let mut editor =
            InteractiveEditor::new(InteractiveConfig::new("ash> ", Some(history.clone())))
                .expect("editor");
        editor.remember_history("echo private");

        let mode = fs::metadata(history)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn non_regular_history_disables_only_persistence() {
        let directory = TestDirectory::new();
        let history = directory.path().join("history");
        fs::create_dir(&history).expect("history directory");

        let mut editor = InteractiveEditor::new(InteractiveConfig::new("ash> ", Some(history)))
            .expect("editor remains available");

        let warnings = editor.take_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not a regular file"));
        editor.remember_history("echo still-editable");
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_history_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let target = directory.path().join("target");
        let history = directory.path().join("history");
        fs::write(&target, "do not touch\n").expect("target");
        symlink(&target, &history).expect("history symlink");

        let mut editor = InteractiveEditor::new(InteractiveConfig::new("ash> ", Some(history)))
            .expect("editor remains available");

        let warnings = editor.take_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("symbolic link"));
        assert_eq!(
            fs::read_to_string(target).expect("target"),
            "do not touch\n"
        );
    }
}
