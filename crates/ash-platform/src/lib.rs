#![deny(unsafe_code)]

//! Native operating-system adapters for ash.

mod mutation;
mod process;
mod workspace;

pub use mutation::{MutationGuard, ReplaceOutcome};
pub use process::{EnvironmentChange, ProcessExit, ProcessHandle, ProcessSpec};
pub use workspace::{EntryKind, NativeEntry, ResolvedPath, WalkOptions, Workspace};

use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("logical path is not a canonical relative ASH path")]
    InvalidLogicalPath,
    #[error("resolved path escapes the workspace capability")]
    WorkspaceEscape,
    #[error("workspace root must resolve to a directory")]
    InvalidWorkspace,
    #[error("native path cannot be represented as a UTF-8 logical path")]
    NonUtf8Path,
    #[error("process environment contains an invalid name or NUL value")]
    InvalidEnvironment,
    #[error("mutation target must be a regular file without symlink or reparse traversal")]
    InvalidMutationTarget,
    #[error("mutation serialization lock was poisoned")]
    MutationLockPoisoned,
    #[error("input contains {size} bytes, exceeding the operation ceiling of {max}")]
    InputTooLarge { size: u64, max: u64 },
    #[error("filesystem or process I/O failed: {0}")]
    Io(#[from] io::Error),
}
