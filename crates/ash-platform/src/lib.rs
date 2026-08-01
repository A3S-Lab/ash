#![deny(unsafe_code)]

//! Native operating-system adapters for ash.

mod identity;
mod mutation;
mod process;
mod transaction;
mod workspace;

pub use identity::FileIdentity;
pub use mutation::{MutationGuard, ReplaceOutcome};
pub use process::{EnvironmentChange, ProcessExit, ProcessHandle, ProcessSpec};
pub use transaction::{
    FileAction, FileActionKind, FileActionOutcome, FileActionState, FileTransactionFailure,
    FileTransactionLimits, FileTransactionOutcome, MAX_FILE_TRANSACTION_FILE_BYTES,
    MAX_FILE_TRANSACTION_TOTAL_BYTES, TransactionControl,
};
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
    #[error("path is reserved for ash workspace state")]
    ReservedPath,
    #[error("mutation serialization lock was poisoned")]
    MutationLockPoisoned,
    #[error("the durable mutation journal is corrupt or incompatible")]
    JournalCorrupt,
    #[error("a durable mutation journal requires explicit recovery")]
    RecoveryRequired,
    #[error("input contains {size} bytes, exceeding the operation ceiling of {max}")]
    InputTooLarge { size: u64, max: u64 },
    #[error("workspace walk exceeds the entry ceiling of {max}")]
    EntryLimit { max: usize },
    #[error("filesystem or process I/O failed: {0}")]
    Io(#[from] io::Error),
}
