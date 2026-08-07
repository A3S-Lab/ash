use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::Script;

#[derive(Clone, Debug, Eq, PartialEq)]
struct EnvironmentEntry {
    name: OsString,
    value: OsString,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum EnvironmentKey {
    Native(OsString),
    #[cfg(windows)]
    Folded(String),
}

/// A native-string environment with host-appropriate name lookup rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlatformEnvironment {
    entries: BTreeMap<EnvironmentKey, EnvironmentEntry>,
}

impl PlatformEnvironment {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn from_process() -> Self {
        std::env::vars_os().collect()
    }

    pub fn insert(
        &mut self,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Option<OsString> {
        let name = name.into();
        let key = environment_key(&name);
        self.entries
            .insert(
                key,
                EnvironmentEntry {
                    name,
                    value: value.into(),
                },
            )
            .map(|entry| entry.value)
    }

    #[must_use]
    pub fn get(&self, name: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.entries
            .get(&environment_key(name.as_ref()))
            .map(|entry| entry.value.as_os_str())
    }

    pub fn remove(&mut self, name: impl AsRef<OsStr>) -> Option<OsString> {
        self.entries
            .remove(&environment_key(name.as_ref()))
            .map(|entry| entry.value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.entries
            .values()
            .map(|entry| (entry.name.as_os_str(), entry.value.as_os_str()))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<K, V> FromIterator<(K, V)> for PlatformEnvironment
where
    K: Into<OsString>,
    V: Into<OsString>,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut environment = Self::new();
        for (name, value) in iter {
            environment.insert(name, value);
        }
        environment
    }
}

#[cfg(windows)]
fn environment_key(name: &OsStr) -> EnvironmentKey {
    name.to_str().map_or_else(
        || EnvironmentKey::Native(name.to_os_string()),
        |name| EnvironmentKey::Folded(name.to_lowercase()),
    )
}

#[cfg(not(windows))]
fn environment_key(name: &OsStr) -> EnvironmentKey {
    EnvironmentKey::Native(name.to_os_string())
}

/// Explicit execution backend retained in shell status and job metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionBackend {
    Native,
    Wsl { distribution: Option<String> },
}

/// Stable high-level reason for a command status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ShellStatusKind {
    Exited,
    ParseError,
    ExpansionError,
    ResolutionError,
    SpawnError,
    RedirectionError,
    TimedOut,
    Interrupted,
}

/// Conventional numeric status plus backend-specific detail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellStatus {
    code: i64,
    kind: ShellStatusKind,
    signal: Option<i64>,
    backend: ExecutionBackend,
}

impl ShellStatus {
    #[must_use]
    pub const fn success() -> Self {
        Self {
            code: 0,
            kind: ShellStatusKind::Exited,
            signal: None,
            backend: ExecutionBackend::Native,
        }
    }

    #[must_use]
    pub const fn new(
        code: i64,
        kind: ShellStatusKind,
        signal: Option<i64>,
        backend: ExecutionBackend,
    ) -> Self {
        Self {
            code,
            kind,
            signal,
            backend,
        }
    }

    #[must_use]
    pub const fn code(&self) -> i64 {
        self.code
    }

    #[must_use]
    pub const fn kind(&self) -> ShellStatusKind {
        self.kind
    }

    #[must_use]
    pub const fn signal(&self) -> Option<i64> {
        self.signal
    }

    #[must_use]
    pub const fn backend(&self) -> &ExecutionBackend {
        &self.backend
    }
}

impl Default for ShellStatus {
    fn default() -> Self {
        Self::success()
    }
}

/// Shell-language options that affect deterministic execution semantics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShellOptions {
    pipefail: bool,
    wsl_distribution: Option<String>,
}

impl ShellOptions {
    #[must_use]
    pub const fn pipefail(&self) -> bool {
        self.pipefail
    }

    pub fn set_pipefail(&mut self, enabled: bool) {
        self.pipefail = enabled;
    }

    #[must_use]
    pub fn wsl_distribution(&self) -> Option<&str> {
        self.wsl_distribution.as_deref()
    }

    pub fn set_wsl_distribution(&mut self, distribution: Option<String>) {
        self.wsl_distribution = distribution;
    }
}

/// Parsed function body stored independently from aliases.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellFunction {
    body: Script,
}

impl ShellFunction {
    #[must_use]
    pub const fn new(body: Script) -> Self {
        Self { body }
    }

    #[must_use]
    pub const fn body(&self) -> &Script {
        &self.body
    }
}

/// Lifecycle state retained for a shell-owned job.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JobState {
    Running,
    Stopped,
    Exited(ShellStatus),
}

/// User-visible metadata for one shell-owned job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobSummary {
    id: u64,
    command: String,
    state: JobState,
}

impl JobSummary {
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    #[must_use]
    pub const fn state(&self) -> &JobState {
        &self.state
    }
}

/// Stable, insertion-ordered-by-ID registry for future interactive jobs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobTable {
    next_id: u64,
    jobs: BTreeMap<u64, JobSummary>,
}

impl JobTable {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_id: 1,
            jobs: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, command: String, state: JobState) -> Result<u64, StateError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(StateError::JobIdExhausted)?;
        self.jobs.insert(id, JobSummary { id, command, state });
        Ok(id)
    }

    #[must_use]
    pub fn get(&self, id: u64) -> Option<&JobSummary> {
        self.jobs.get(&id)
    }

    pub fn remove(&mut self, id: u64) -> Option<JobSummary> {
        self.jobs.remove(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &JobSummary> {
        self.jobs.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

impl Default for JobTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Mutable state that persists across human-shell commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellState {
    cwd: PathBuf,
    environment: PlatformEnvironment,
    variables: BTreeMap<String, OsString>,
    aliases: BTreeMap<String, String>,
    functions: BTreeMap<String, ShellFunction>,
    last_status: ShellStatus,
    options: ShellOptions,
    jobs: JobTable,
}

impl ShellState {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            environment: PlatformEnvironment::new(),
            variables: BTreeMap::new(),
            aliases: BTreeMap::new(),
            functions: BTreeMap::new(),
            last_status: ShellStatus::success(),
            options: ShellOptions::default(),
            jobs: JobTable::new(),
        }
    }

    pub fn from_process() -> std::io::Result<Self> {
        let mut state = Self::new(std::env::current_dir()?);
        state.environment = PlatformEnvironment::from_process();
        Ok(state)
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn set_cwd(&mut self, cwd: impl Into<PathBuf>) {
        self.cwd = cwd.into();
    }

    #[must_use]
    pub const fn environment(&self) -> &PlatformEnvironment {
        &self.environment
    }

    pub const fn environment_mut(&mut self) -> &mut PlatformEnvironment {
        &mut self.environment
    }

    #[must_use]
    pub fn variable(&self, name: &str) -> Option<&OsStr> {
        self.variables.get(name).map(OsString::as_os_str)
    }

    pub fn set_variable(
        &mut self,
        name: impl Into<String>,
        value: impl Into<OsString>,
    ) -> Result<Option<OsString>, StateError> {
        let name = name.into();
        validate_identifier(&name)?;
        Ok(self.variables.insert(name, value.into()))
    }

    pub fn unset_variable(&mut self, name: &str) -> Option<OsString> {
        self.variables.remove(name)
    }

    #[must_use]
    pub fn alias(&self, name: &str) -> Option<&str> {
        self.aliases.get(name).map(String::as_str)
    }

    pub fn set_alias(
        &mut self,
        name: impl Into<String>,
        replacement: impl Into<String>,
    ) -> Result<Option<String>, StateError> {
        let name = name.into();
        validate_identifier(&name)?;
        Ok(self.aliases.insert(name, replacement.into()))
    }

    pub fn unset_alias(&mut self, name: &str) -> Option<String> {
        self.aliases.remove(name)
    }

    #[must_use]
    pub fn function(&self, name: &str) -> Option<&ShellFunction> {
        self.functions.get(name)
    }

    pub fn set_function(
        &mut self,
        name: impl Into<String>,
        function: ShellFunction,
    ) -> Result<Option<ShellFunction>, StateError> {
        let name = name.into();
        validate_identifier(&name)?;
        Ok(self.functions.insert(name, function))
    }

    pub fn unset_function(&mut self, name: &str) -> Option<ShellFunction> {
        self.functions.remove(name)
    }

    #[must_use]
    pub const fn last_status(&self) -> &ShellStatus {
        &self.last_status
    }

    pub fn set_last_status(&mut self, status: ShellStatus) {
        self.last_status = status;
    }

    #[must_use]
    pub const fn options(&self) -> &ShellOptions {
        &self.options
    }

    pub const fn options_mut(&mut self) -> &mut ShellOptions {
        &mut self.options
    }

    #[must_use]
    pub const fn jobs(&self) -> &JobTable {
        &self.jobs
    }

    pub const fn jobs_mut(&mut self) -> &mut JobTable {
        &mut self.jobs
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    InvalidIdentifier,
    JobIdExhausted,
}

impl fmt::Display for StateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier => formatter.write_str(
                "shell names must be non-empty ASCII identifiers beginning with a letter or underscore",
            ),
            Self::JobIdExhausted => formatter.write_str("shell job identifiers are exhausted"),
        }
    }
}

impl std::error::Error for StateError {}

pub(crate) fn validate_identifier(name: &str) -> Result<(), StateError> {
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        Err(StateError::InvalidIdentifier)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{JobState, PlatformEnvironment, ShellState, StateError};

    #[test]
    fn environment_preserves_native_names_and_values() {
        let mut environment = PlatformEnvironment::new();
        assert_eq!(environment.insert("ASH_VALUE", "first"), None);
        assert_eq!(environment.get("ASH_VALUE"), Some(OsStr::new("first")));
        assert_eq!(
            environment.insert("ASH_VALUE", "second"),
            Some("first".into())
        );
        assert_eq!(environment.remove("ASH_VALUE"), Some("second".into()));
        assert!(environment.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn environment_names_are_case_insensitive_on_windows() {
        let mut environment = PlatformEnvironment::new();
        environment.insert("Path", "first");
        assert_eq!(environment.get("PATH"), Some(OsStr::new("first")));
        environment.insert("path", "second");
        assert_eq!(environment.len(), 1);
        assert_eq!(environment.get("PaTh"), Some(OsStr::new("second")));
    }

    #[test]
    fn state_rejects_ambiguous_names() {
        let mut state = ShellState::new(".");
        assert_eq!(
            state.set_alias("not-valid", "echo no"),
            Err(StateError::InvalidIdentifier)
        );
        assert_eq!(
            state.set_variable("9VALUE", "no"),
            Err(StateError::InvalidIdentifier)
        );
    }

    #[test]
    fn job_ids_are_stable_and_monotonic() {
        let mut state = ShellState::new(".");
        let first = state
            .jobs_mut()
            .insert("one".to_owned(), JobState::Running)
            .expect("job one");
        let second = state
            .jobs_mut()
            .insert("two".to_owned(), JobState::Running)
            .expect("job two");
        assert_eq!((first, second), (1, 2));
        assert_eq!(state.jobs().get(second).expect("job").command(), "two");
    }
}
