use std::collections::HashSet;
use std::fs::{self, File, OpenOptions, TryLockError as FileTryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::sync::TryLockError as MutexTryLockError;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::mutation::ReplaceOutcome;
use crate::workspace::validate_logical;
use crate::{PlatformError, Workspace};

const STATE_DIRECTORY: &str = ".ash";
const FORMAT_FILE: &str = "FORMAT";
const LOCK_FILE: &str = "LOCK";
const TRANSACTION_DIRECTORY: &str = "transaction";
const PREPARING_DIRECTORY: &str = "transaction.new";
const MANIFEST_FILE: &str = "MANIFEST";
const COMMITTING_FILE: &str = "COMMITTED.new";
const COMMITTED_FILE: &str = "COMMITTED";
const STAGE_DIRECTORY: &str = "stage";
const REMOVED_DIRECTORY: &str = "removed";
const FORMAT_MARKER: &[u8] = b"ash-workspace-state-v1\n";
const COMMITTED_MARKER: &[u8] = b"committed\n";
const MANIFEST_MAGIC: &[u8; 8] = b"ASHFS002";
const NONE_LENGTH: u32 = u32::MAX;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const LOCK_POLL_MILLIS: u64 = 10;
const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_ACTIONS: usize = 256;

pub const MAX_FILE_TRANSACTION_FILE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_FILE_TRANSACTION_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FileActionKind {
    Create = 0,
    Copy = 1,
    Move = 2,
    Remove = 3,
    /// Internal durable replacement used by the ASH patch operation. ASH/1
    /// filesystem action decoding deliberately does not expose this variant.
    Replace = 4,
}

#[derive(Clone, Debug)]
pub struct FileAction {
    kind: FileActionKind,
    path: String,
    destination: Option<String>,
    expected_digest: Option<[u8; 32]>,
    content: Option<Vec<u8>>,
}

impl FileAction {
    #[must_use]
    pub fn create(path: impl Into<String>, content: Vec<u8>) -> Self {
        Self {
            kind: FileActionKind::Create,
            path: path.into(),
            destination: None,
            expected_digest: None,
            content: Some(content),
        }
    }

    #[must_use]
    pub fn copy(
        path: impl Into<String>,
        destination: impl Into<String>,
        expected_digest: [u8; 32],
    ) -> Self {
        Self {
            kind: FileActionKind::Copy,
            path: path.into(),
            destination: Some(destination.into()),
            expected_digest: Some(expected_digest),
            content: None,
        }
    }

    #[must_use]
    pub fn move_file(
        path: impl Into<String>,
        destination: impl Into<String>,
        expected_digest: [u8; 32],
    ) -> Self {
        Self {
            kind: FileActionKind::Move,
            path: path.into(),
            destination: Some(destination.into()),
            expected_digest: Some(expected_digest),
            content: None,
        }
    }

    #[must_use]
    pub fn remove(path: impl Into<String>, expected_digest: [u8; 32]) -> Self {
        Self {
            kind: FileActionKind::Remove,
            path: path.into(),
            destination: None,
            expected_digest: Some(expected_digest),
            content: None,
        }
    }

    #[must_use]
    pub fn replace(path: impl Into<String>, expected_digest: [u8; 32], content: Vec<u8>) -> Self {
        Self {
            kind: FileActionKind::Replace,
            path: path.into(),
            destination: None,
            expected_digest: Some(expected_digest),
            content: Some(content),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> FileActionKind {
        self.kind
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn destination(&self) -> Option<&str> {
        self.destination.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileTransactionLimits {
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl FileTransactionLimits {
    pub fn new(max_file_bytes: u64, max_total_bytes: u64) -> Result<Self, PlatformError> {
        if max_file_bytes == 0
            || max_total_bytes == 0
            || max_file_bytes > max_total_bytes
            || max_file_bytes > MAX_FILE_TRANSACTION_FILE_BYTES
            || max_total_bytes > MAX_FILE_TRANSACTION_TOTAL_BYTES
        {
            Err(PlatformError::InvalidMutationTarget)
        } else {
            Ok(Self {
                max_file_bytes,
                max_total_bytes,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionControl {
    Continue,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileActionState {
    Committed,
    Conflict,
    RolledBack,
    RecoveryRequired,
    Skipped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileActionOutcome {
    pub state: FileActionState,
    pub digest: Option<[u8; 32]>,
}

impl FileActionOutcome {
    const fn skipped() -> Self {
        Self {
            state: FileActionState::Skipped,
            digest: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileTransactionFailure {
    Conflict,
    Cancelled,
    TimedOut,
    Filesystem,
    RecoveryRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileTransactionOutcome {
    pub actions: Vec<FileActionOutcome>,
    pub failure: Option<FileTransactionFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedAction {
    kind: FileActionKind,
    path: String,
    destination: Option<String>,
    digest: [u8; 32],
    size: u64,
    preimage_digest: Option<[u8; 32]>,
    preimage_size: Option<u64>,
}

#[derive(Default)]
struct TransactionTotals {
    materialized: u64,
    preimages: u64,
}

struct TransactionLocks<'a> {
    _local: MutexGuard<'a, ()>,
    _file: File,
}

enum LockResult<'a> {
    Acquired(TransactionLocks<'a>),
    Stopped(FileTransactionFailure),
}

enum StepFailure {
    Conflict([u8; 32]),
    Stopped(FileTransactionFailure),
    Platform(PlatformError),
}

impl From<PlatformError> for StepFailure {
    fn from(error: PlatformError) -> Self {
        Self::Platform(error)
    }
}

#[derive(Clone, Copy)]
enum AppliedState {
    Applied,
    Linked,
    NotApplied,
    Indeterminate,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CrashPoint {
    PreparingCreated,
    ActionPrepared(usize),
    ManifestPrepared,
    JournalPublished,
    CreateCopyLinked(usize),
    CreateCopyParentSynced(usize),
    CreateCopyStageRemoved(usize),
    CreateCopyStageSynced(usize),
    MoveLinked(usize),
    MoveDestinationSynced(usize),
    MoveSourceRemoved(usize),
    MoveSourceSynced(usize),
    RemoveRenamed(usize),
    RemoveSourceSynced(usize),
    RemoveJournalSynced(usize),
    ReplaceSwapped(usize),
    ReplaceParentSynced(usize),
    ReplaceStageRemoved(usize),
    ReplaceStageSynced(usize),
    CommitTempWritten,
    CommitMarkerRenamed,
    CommitSynced,
    RollbackCreateCopyDestinationRemoved(usize),
    RollbackMoveSourceLinked(usize),
    RollbackMoveDestinationRemoved(usize),
    RollbackRemoveRestored(usize),
    RollbackReplaceRestored(usize),
    RollbackActionCompleted(usize),
    RecoveryJournalRemoved,
}

#[cfg(test)]
thread_local! {
    static ARMED_CRASH_POINT: std::cell::Cell<Option<CrashPoint>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn arm_crash(point: CrashPoint) {
    ARMED_CRASH_POINT.with(|armed| armed.set(Some(point)));
}

#[cfg(test)]
fn crash_at(point: CrashPoint) {
    ARMED_CRASH_POINT.with(|armed| {
        if armed.get() == Some(point) {
            armed.set(None);
            panic!("simulated process crash at {point:?}");
        }
    });
}

impl Workspace {
    /// Validates a file transaction without acquiring locks or touching disk.
    pub fn validate_file_actions(&self, actions: &[FileAction]) -> Result<(), PlatformError> {
        validate_actions(self, actions)
    }

    pub fn file_transaction<F>(
        &self,
        actions: Vec<FileAction>,
        limits: FileTransactionLimits,
        mut control: F,
    ) -> Result<FileTransactionOutcome, PlatformError>
    where
        F: FnMut() -> TransactionControl,
    {
        validate_actions(self, &actions)?;
        let mut rows = vec![FileActionOutcome::skipped(); actions.len()];
        let _locks = match self.acquire_transaction_locks(&mut control)? {
            LockResult::Acquired(locks) => locks,
            LockResult::Stopped(failure) => {
                return Ok(FileTransactionOutcome {
                    actions: rows,
                    failure: Some(failure),
                });
            }
        };
        self.recover_locked()?;

        let state = self.state_directory();
        let preparing = state.join(PREPARING_DIRECTORY);
        let transaction = state.join(TRANSACTION_DIRECTORY);
        if internal_directory_exists(&preparing)? {
            remove_directory_if_present(&preparing)?;
        }
        fs::create_dir(&preparing)?;
        fs::create_dir(preparing.join(STAGE_DIRECTORY))?;
        fs::create_dir(preparing.join(REMOVED_DIRECTORY))?;
        sync_directory(&state)?;
        #[cfg(test)]
        crash_at(CrashPoint::PreparingCreated);

        let mut totals = TransactionTotals::default();
        let mut prepared = Vec::with_capacity(actions.len());
        for (index, action) in actions.iter().enumerate() {
            let result =
                self.prepare_action(index, action, &preparing, limits, &mut totals, &mut control);
            match result {
                Ok(action) => {
                    prepared.push(action);
                    #[cfg(test)]
                    crash_at(CrashPoint::ActionPrepared(index));
                }
                Err(StepFailure::Conflict(digest)) => {
                    rows[index] = FileActionOutcome {
                        state: FileActionState::Conflict,
                        digest: Some(digest),
                    };
                    remove_directory_if_present(&preparing)?;
                    return Ok(FileTransactionOutcome {
                        actions: rows,
                        failure: Some(FileTransactionFailure::Conflict),
                    });
                }
                Err(StepFailure::Stopped(failure)) => {
                    remove_directory_if_present(&preparing)?;
                    return Ok(FileTransactionOutcome {
                        actions: rows,
                        failure: Some(failure),
                    });
                }
                Err(StepFailure::Platform(error)) => {
                    remove_directory_if_present(&preparing)?;
                    return Err(error);
                }
            }
        }

        let manifest = encode_manifest(&prepared)?;
        write_new_sync(&preparing.join(MANIFEST_FILE), &manifest)?;
        sync_directory(&preparing)?;
        #[cfg(test)]
        crash_at(CrashPoint::ManifestPrepared);
        fs::rename(&preparing, &transaction)?;
        sync_directory(&state)?;
        #[cfg(test)]
        crash_at(CrashPoint::JournalPublished);

        let mut failure = None;
        for (index, action) in prepared.iter().enumerate() {
            // Until the step reports a definitive result, a filesystem error
            // may have happened after its atomic mutation became visible.
            rows[index] = FileActionOutcome {
                state: FileActionState::RecoveryRequired,
                digest: Some(action.digest),
            };
            match self.apply_action(index, action, &transaction, limits, &mut control) {
                Ok(()) => {
                    rows[index] = FileActionOutcome {
                        state: FileActionState::Committed,
                        digest: Some(action.digest),
                    };
                }
                Err(StepFailure::Conflict(digest)) => {
                    rows[index] = FileActionOutcome {
                        state: FileActionState::Conflict,
                        digest: Some(digest),
                    };
                    failure = Some(FileTransactionFailure::Conflict);
                    break;
                }
                Err(StepFailure::Stopped(stopped)) => {
                    // Control points are deliberately placed before mutations.
                    rows[index] = FileActionOutcome::skipped();
                    failure = Some(stopped);
                    break;
                }
                Err(StepFailure::Platform(_)) => {
                    failure = Some(FileTransactionFailure::Filesystem);
                    break;
                }
            }
        }

        if failure.is_none() {
            match check_control(&mut control) {
                Ok(()) => {}
                Err(stopped) => failure = Some(stopped),
            }
        }
        if let Some(mut failure) = failure {
            if self.rollback_locked(&prepared, Some(&mut rows))? {
                failure = FileTransactionFailure::RecoveryRequired;
            } else {
                remove_directory_if_present(&transaction)?;
                sync_directory(&state)?;
            }
            return Ok(FileTransactionOutcome {
                actions: rows,
                failure: Some(failure),
            });
        }

        let committing = transaction.join(COMMITTING_FILE);
        let committed = transaction.join(COMMITTED_FILE);
        let commit_failed = if write_new_sync(&committing, COMMITTED_MARKER).is_err() {
            true
        } else {
            #[cfg(test)]
            crash_at(CrashPoint::CommitTempWritten);
            let rename_failed = fs::rename(&committing, &committed).is_err();
            if !rename_failed {
                #[cfg(test)]
                crash_at(CrashPoint::CommitMarkerRenamed);
            }
            rename_failed
        };
        if commit_failed {
            let recovery_required = self.rollback_locked(&prepared, Some(&mut rows))?;
            if !recovery_required {
                remove_directory_if_present(&transaction)?;
                sync_directory(&state)?;
            }
            return Ok(FileTransactionOutcome {
                actions: rows,
                failure: Some(if recovery_required {
                    FileTransactionFailure::RecoveryRequired
                } else {
                    FileTransactionFailure::Filesystem
                }),
            });
        }
        if sync_directory(&transaction).is_err() {
            for (row, action) in rows.iter_mut().zip(&prepared) {
                *row = FileActionOutcome {
                    state: FileActionState::RecoveryRequired,
                    digest: Some(action.digest),
                };
            }
            return Ok(FileTransactionOutcome {
                actions: rows,
                failure: Some(FileTransactionFailure::RecoveryRequired),
            });
        }
        #[cfg(test)]
        crash_at(CrashPoint::CommitSynced);
        // A durable commit marker makes cleanup retryable. Cleanup failure does
        // not change the committed outcome; the next transaction finalizes it.
        let _ = remove_directory_if_present(&transaction);
        let _ = sync_directory(&state);
        Ok(FileTransactionOutcome {
            actions: rows,
            failure: None,
        })
    }

    pub fn recover_file_transactions<F>(&self, mut control: F) -> Result<bool, PlatformError>
    where
        F: FnMut() -> TransactionControl,
    {
        if !self.internal_state.load(Ordering::Acquire) {
            return Ok(false);
        }
        let _locks = match self.acquire_transaction_locks(&mut control)? {
            LockResult::Acquired(locks) => locks,
            LockResult::Stopped(_) => return Ok(false),
        };
        self.recover_locked()
    }

    fn prepare_action<F>(
        &self,
        index: usize,
        action: &FileAction,
        preparing: &Path,
        limits: FileTransactionLimits,
        totals: &mut TransactionTotals,
        control: &mut F,
    ) -> Result<PreparedAction, StepFailure>
    where
        F: FnMut() -> TransactionControl,
    {
        check_control(control).map_err(StepFailure::Stopped)?;
        let stage = indexed_path(&preparing.join(STAGE_DIRECTORY), index);
        let (digest, size, preimage_digest, preimage_size) = match action.kind {
            FileActionKind::Create => {
                self.ensure_absent(&action.path, limits, &mut totals.materialized, control)?;
                let content = action
                    .content
                    .as_deref()
                    .ok_or(StepFailure::Platform(PlatformError::InvalidMutationTarget))?;
                charge_bytes(content.len() as u64, limits, &mut totals.materialized)?;
                write_new_sync(&stage, content).map_err(StepFailure::Platform)?;
                (
                    *blake3::hash(content).as_bytes(),
                    content.len() as u64,
                    None,
                    None,
                )
            }
            FileActionKind::Copy => {
                let destination = action
                    .destination
                    .as_deref()
                    .ok_or(StepFailure::Platform(PlatformError::InvalidMutationTarget))?;
                self.ensure_absent(destination, limits, &mut totals.materialized, control)?;
                let source = self
                    .checked_mutation_path(&action.path, true)
                    .map_err(StepFailure::Platform)?;
                let (digest, size) =
                    copy_file_bounded(&source, &stage, limits, &mut totals.materialized, control)?;
                if Some(digest) != action.expected_digest {
                    return Err(StepFailure::Conflict(digest));
                }
                (digest, size, None, None)
            }
            FileActionKind::Move | FileActionKind::Remove => {
                if let Some(destination) = action.destination.as_deref() {
                    self.ensure_absent(destination, limits, &mut totals.materialized, control)?;
                }
                let source = self
                    .checked_mutation_path(&action.path, true)
                    .map_err(StepFailure::Platform)?;
                let (digest, size) =
                    hash_file_bounded(&source, limits, &mut totals.materialized, control)?;
                if Some(digest) != action.expected_digest {
                    return Err(StepFailure::Conflict(digest));
                }
                (digest, size, None, None)
            }
            FileActionKind::Replace => {
                let source = self
                    .checked_mutation_path(&action.path, true)
                    .map_err(StepFailure::Platform)?;
                let preimage = indexed_path(&preparing.join(REMOVED_DIRECTORY), index);
                let (preimage_digest, preimage_size) =
                    copy_file_bounded(&source, &preimage, limits, &mut totals.preimages, control)?;
                if Some(preimage_digest) != action.expected_digest {
                    return Err(StepFailure::Conflict(preimage_digest));
                }
                let content = action
                    .content
                    .as_deref()
                    .ok_or(StepFailure::Platform(PlatformError::InvalidMutationTarget))?;
                charge_bytes(content.len() as u64, limits, &mut totals.materialized)?;
                write_new_sync(&stage, content).map_err(StepFailure::Platform)?;
                (
                    *blake3::hash(content).as_bytes(),
                    content.len() as u64,
                    Some(preimage_digest),
                    Some(preimage_size),
                )
            }
        };
        Ok(PreparedAction {
            kind: action.kind,
            path: action.path.clone(),
            destination: action.destination.clone(),
            digest,
            size,
            preimage_digest,
            preimage_size,
        })
    }

    fn apply_action<F>(
        &self,
        index: usize,
        action: &PreparedAction,
        transaction: &Path,
        limits: FileTransactionLimits,
        control: &mut F,
    ) -> Result<(), StepFailure>
    where
        F: FnMut() -> TransactionControl,
    {
        check_control(control).map_err(StepFailure::Stopped)?;
        match action.kind {
            FileActionKind::Create | FileActionKind::Copy => {
                let destination = action.destination.as_deref().unwrap_or(&action.path);
                self.ensure_absent_for_apply(destination, limits, control)?;
                let destination = self
                    .checked_mutation_path(destination, false)
                    .map_err(StepFailure::Platform)?;
                let stage = indexed_path(&transaction.join(STAGE_DIRECTORY), index);
                let (stage_digest, stage_size) = hash_file_without_charge(&stage, limits, control)?;
                if stage_digest != action.digest || stage_size != action.size {
                    return Err(StepFailure::Platform(PlatformError::JournalCorrupt));
                }
                fs::hard_link(&stage, &destination)
                    .map_err(|error| hard_link_error(&destination, limits, control, error))?;
                #[cfg(test)]
                crash_at(CrashPoint::CreateCopyLinked(index));
                sync_parent(&destination).map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::CreateCopyParentSynced(index));
                fs::remove_file(&stage).map_err(PlatformError::from)?;
                #[cfg(test)]
                crash_at(CrashPoint::CreateCopyStageRemoved(index));
                sync_directory(&transaction.join(STAGE_DIRECTORY))
                    .map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::CreateCopyStageSynced(index));
                Ok(())
            }
            FileActionKind::Move => {
                let source = self
                    .checked_mutation_path(&action.path, true)
                    .map_err(StepFailure::Platform)?;
                let (actual, size) = hash_file_without_charge(&source, limits, control)?;
                if actual != action.digest || size != action.size {
                    return Err(StepFailure::Conflict(actual));
                }
                let destination_logical = action
                    .destination
                    .as_deref()
                    .ok_or(StepFailure::Platform(PlatformError::JournalCorrupt))?;
                self.ensure_absent_for_apply(destination_logical, limits, control)?;
                let destination = self
                    .checked_mutation_path(destination_logical, false)
                    .map_err(StepFailure::Platform)?;
                fs::hard_link(&source, &destination)
                    .map_err(|error| hard_link_error(&destination, limits, control, error))?;
                #[cfg(test)]
                crash_at(CrashPoint::MoveLinked(index));
                sync_parent(&destination).map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::MoveDestinationSynced(index));
                fs::remove_file(&source).map_err(PlatformError::from)?;
                #[cfg(test)]
                crash_at(CrashPoint::MoveSourceRemoved(index));
                sync_parent(&source).map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::MoveSourceSynced(index));
                Ok(())
            }
            FileActionKind::Remove => {
                let source = self
                    .checked_mutation_path(&action.path, true)
                    .map_err(StepFailure::Platform)?;
                let (actual, size) = hash_file_without_charge(&source, limits, control)?;
                if actual != action.digest || size != action.size {
                    return Err(StepFailure::Conflict(actual));
                }
                let removed = indexed_path(&transaction.join(REMOVED_DIRECTORY), index);
                fs::rename(&source, &removed).map_err(PlatformError::from)?;
                #[cfg(test)]
                crash_at(CrashPoint::RemoveRenamed(index));
                sync_parent(&source).map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::RemoveSourceSynced(index));
                sync_directory(&transaction.join(REMOVED_DIRECTORY))
                    .map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::RemoveJournalSynced(index));
                Ok(())
            }
            FileActionKind::Replace => {
                let expected_digest = action
                    .preimage_digest
                    .ok_or(StepFailure::Platform(PlatformError::JournalCorrupt))?;
                let expected_size = action
                    .preimage_size
                    .ok_or(StepFailure::Platform(PlatformError::JournalCorrupt))?;
                let destination = self
                    .checked_mutation_path(&action.path, true)
                    .map_err(StepFailure::Platform)?;
                let (actual_digest, actual_size) =
                    hash_file_without_charge(&destination, limits, control)?;
                if actual_digest != expected_digest || actual_size != expected_size {
                    return Err(StepFailure::Conflict(actual_digest));
                }
                let stage = indexed_path(&transaction.join(STAGE_DIRECTORY), index);
                let contents =
                    read_staged_replacement(&stage, action.size, action.digest, limits, control)?;
                match self.compare_and_swap_replace_inner(
                    &action.path,
                    expected_digest,
                    &contents,
                    limits.max_file_bytes,
                ) {
                    Ok(ReplaceOutcome::Committed { new_digest, .. })
                        if new_digest == action.digest => {}
                    Ok(ReplaceOutcome::Committed { .. }) => {
                        return Err(StepFailure::Platform(PlatformError::RecoveryRequired));
                    }
                    Ok(ReplaceOutcome::Conflict { actual_digest }) => {
                        return Err(StepFailure::Conflict(actual_digest));
                    }
                    Ok(ReplaceOutcome::Indeterminate { .. }) => {
                        return Err(StepFailure::Platform(PlatformError::RecoveryRequired));
                    }
                    Err(error) => return Err(StepFailure::Platform(error)),
                }
                #[cfg(test)]
                crash_at(CrashPoint::ReplaceSwapped(index));
                sync_parent(&destination).map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::ReplaceParentSynced(index));
                fs::remove_file(&stage).map_err(PlatformError::from)?;
                #[cfg(test)]
                crash_at(CrashPoint::ReplaceStageRemoved(index));
                sync_directory(&transaction.join(STAGE_DIRECTORY))
                    .map_err(StepFailure::Platform)?;
                #[cfg(test)]
                crash_at(CrashPoint::ReplaceStageSynced(index));
                Ok(())
            }
        }
    }

    fn ensure_absent<F>(
        &self,
        logical: &str,
        limits: FileTransactionLimits,
        total_bytes: &mut u64,
        control: &mut F,
    ) -> Result<PathBuf, StepFailure>
    where
        F: FnMut() -> TransactionControl,
    {
        let candidate = self
            .checked_mutation_path(logical, false)
            .map_err(StepFailure::Platform)?;
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(candidate),
            Ok(metadata) if metadata.is_file() => {
                let (digest, _) = hash_file_bounded(&candidate, limits, total_bytes, control)?;
                Err(StepFailure::Conflict(digest))
            }
            Ok(_) => Err(StepFailure::Platform(PlatformError::InvalidMutationTarget)),
            Err(error) => Err(StepFailure::Platform(error.into())),
        }
    }

    fn ensure_absent_for_apply<F>(
        &self,
        logical: &str,
        limits: FileTransactionLimits,
        control: &mut F,
    ) -> Result<(), StepFailure>
    where
        F: FnMut() -> TransactionControl,
    {
        let candidate = self
            .checked_mutation_path(logical, false)
            .map_err(StepFailure::Platform)?;
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(metadata) if metadata.is_file() => Err(StepFailure::Conflict(
                hash_file_without_charge(&candidate, limits, control)?.0,
            )),
            Ok(_) => Err(StepFailure::Platform(PlatformError::InvalidMutationTarget)),
            Err(error) => Err(StepFailure::Platform(error.into())),
        }
    }

    fn acquire_transaction_locks<'a, F>(
        &'a self,
        control: &mut F,
    ) -> Result<LockResult<'a>, PlatformError>
    where
        F: FnMut() -> TransactionControl,
    {
        let local = loop {
            match self.mutation_lock.try_lock() {
                Ok(guard) => break guard,
                Err(MutexTryLockError::WouldBlock) => {
                    if let Some(failure) = control_failure(control()) {
                        return Ok(LockResult::Stopped(failure));
                    }
                    thread::sleep(Duration::from_millis(LOCK_POLL_MILLIS));
                }
                Err(MutexTryLockError::Poisoned(_)) => {
                    return Err(PlatformError::MutationLockPoisoned);
                }
            }
        };
        self.ensure_internal_state()?;
        let lock_path = self.state_directory().join(LOCK_FILE);
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(PlatformError::JournalCorrupt),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Ok(LockResult::Acquired(TransactionLocks {
                        _local: local,
                        _file: file,
                    }));
                }
                Err(FileTryLockError::WouldBlock) => {
                    if let Some(failure) = control_failure(control()) {
                        return Ok(LockResult::Stopped(failure));
                    }
                    thread::sleep(Duration::from_millis(LOCK_POLL_MILLIS));
                }
                Err(FileTryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }

    fn ensure_internal_state(&self) -> Result<(), PlatformError> {
        let state = self.state_directory();
        match fs::symlink_metadata(&state) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(PlatformError::ReservedPath);
                }
                let marker = state.join(FORMAT_FILE);
                if read_regular_bounded(&marker, FORMAT_MARKER.len())
                    .ok()
                    .as_deref()
                    != Some(FORMAT_MARKER)
                {
                    return Err(PlatformError::ReservedPath);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&state)?;
                set_private_directory_permissions(&state)?;
                write_new_sync(&state.join(FORMAT_FILE), FORMAT_MARKER)?;
                sync_directory(self.native_root())?;
            }
            Err(error) => return Err(error.into()),
        }
        self.internal_state.store(true, Ordering::Release);
        Ok(())
    }

    fn state_directory(&self) -> PathBuf {
        self.native_root().join(STATE_DIRECTORY)
    }

    fn recover_locked(&self) -> Result<bool, PlatformError> {
        let state = self.state_directory();
        let preparing = state.join(PREPARING_DIRECTORY);
        let transaction = state.join(TRANSACTION_DIRECTORY);
        let mut recovered = false;
        if internal_directory_exists(&preparing)? {
            remove_directory_if_present(&preparing)?;
            recovered = true;
        }
        if !internal_directory_exists(&transaction)? {
            return Ok(recovered);
        }
        require_internal_directory(&transaction.join(STAGE_DIRECTORY))?;
        require_internal_directory(&transaction.join(REMOVED_DIRECTORY))?;
        let prepared = decode_manifest(&read_regular_bounded(
            &transaction.join(MANIFEST_FILE),
            MAX_MANIFEST_BYTES,
        )?)?;
        let committed = transaction.join(COMMITTED_FILE);
        match fs::symlink_metadata(&committed) {
            Ok(_) => {
                if read_regular_bounded(&committed, COMMITTED_MARKER.len())? != COMMITTED_MARKER {
                    return Err(PlatformError::JournalCorrupt);
                }
                remove_directory_if_present(&transaction)?;
                sync_directory(&state)?;
                return Ok(true);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if self.rollback_locked(&prepared, None)? {
            return Err(PlatformError::RecoveryRequired);
        }
        remove_directory_if_present(&transaction)?;
        #[cfg(test)]
        crash_at(CrashPoint::RecoveryJournalRemoved);
        sync_directory(&state)?;
        Ok(true)
    }

    /// Returns true when at least one action could not be safely restored.
    fn rollback_locked(
        &self,
        actions: &[PreparedAction],
        mut rows: Option<&mut [FileActionOutcome]>,
    ) -> Result<bool, PlatformError> {
        let transaction = self.state_directory().join(TRANSACTION_DIRECTORY);
        let mut recovery_required = false;
        for (index, action) in actions.iter().enumerate().rev() {
            let known_not_applied = rows.as_deref().is_some_and(|rows| {
                matches!(
                    rows[index].state,
                    FileActionState::Conflict | FileActionState::Skipped
                )
            });
            let state = if known_not_applied {
                Ok(AppliedState::NotApplied)
            } else {
                self.applied_state(index, action, &transaction)
            };
            match state {
                Ok(AppliedState::NotApplied) => {
                    if let Some(rows) = rows.as_deref_mut()
                        && matches!(
                            rows[index].state,
                            FileActionState::Committed | FileActionState::RecoveryRequired
                        )
                    {
                        rows[index] = FileActionOutcome {
                            state: FileActionState::RolledBack,
                            digest: Some(rollback_digest(action)),
                        };
                    }
                }
                Ok(AppliedState::Indeterminate) | Err(_) => {
                    recovery_required = true;
                    set_recovery_row(rows.as_deref_mut(), index, action.digest);
                }
                Ok(AppliedState::Applied | AppliedState::Linked) => {
                    if self.rollback_action(index, action, &transaction).is_ok() {
                        #[cfg(test)]
                        crash_at(CrashPoint::RollbackActionCompleted(index));
                        if let Some(rows) = rows.as_deref_mut() {
                            rows[index] = FileActionOutcome {
                                state: FileActionState::RolledBack,
                                digest: Some(rollback_digest(action)),
                            };
                        }
                    } else {
                        recovery_required = true;
                        set_recovery_row(rows.as_deref_mut(), index, action.digest);
                    }
                }
            }
        }
        Ok(recovery_required)
    }

    fn applied_state(
        &self,
        index: usize,
        action: &PreparedAction,
        transaction: &Path,
    ) -> Result<AppliedState, PlatformError> {
        let exists = |path: &Path| match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(PlatformError::RecoveryRequired),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        };
        match action.kind {
            FileActionKind::Create | FileActionKind::Copy => {
                let stage = indexed_path(&transaction.join(STAGE_DIRECTORY), index);
                let destination = self.checked_mutation_path(
                    action.destination.as_deref().unwrap_or(&action.path),
                    false,
                )?;
                match (exists(&stage)?, exists(&destination)?) {
                    (true, false) | (false, false) => Ok(AppliedState::NotApplied),
                    (true, true)
                        if file_matches_recovery(&stage, action.size, action.digest)?
                            && same_regular_file(&stage, &destination)? =>
                    {
                        Ok(AppliedState::Linked)
                    }
                    (false, true)
                        if hash_file_recovery(&destination, action.size)? == action.digest =>
                    {
                        Ok(AppliedState::Applied)
                    }
                    _ => Ok(AppliedState::Indeterminate),
                }
            }
            FileActionKind::Move => {
                let source = self.checked_mutation_path(&action.path, false)?;
                let destination = self.checked_mutation_path(
                    action
                        .destination
                        .as_deref()
                        .ok_or(PlatformError::JournalCorrupt)?,
                    false,
                )?;
                match (exists(&source)?, exists(&destination)?) {
                    (true, false) => Ok(AppliedState::NotApplied),
                    (true, true)
                        if file_matches_recovery(&source, action.size, action.digest)?
                            && same_regular_file(&source, &destination)? =>
                    {
                        Ok(AppliedState::Linked)
                    }
                    (false, true)
                        if hash_file_recovery(&destination, action.size)? == action.digest =>
                    {
                        Ok(AppliedState::Applied)
                    }
                    _ => Ok(AppliedState::Indeterminate),
                }
            }
            FileActionKind::Remove => {
                let source = self.checked_mutation_path(&action.path, false)?;
                let removed = indexed_path(&transaction.join(REMOVED_DIRECTORY), index);
                match (exists(&source)?, exists(&removed)?) {
                    (true, false) => Ok(AppliedState::NotApplied),
                    (false, true)
                        if hash_file_recovery(&removed, action.size)? == action.digest =>
                    {
                        Ok(AppliedState::Applied)
                    }
                    _ => Ok(AppliedState::Indeterminate),
                }
            }
            FileActionKind::Replace => {
                let destination = self.checked_mutation_path(&action.path, false)?;
                let removed = indexed_path(&transaction.join(REMOVED_DIRECTORY), index);
                if !exists(&destination)? {
                    return Ok(AppliedState::Indeterminate);
                }
                let preimage_digest = action
                    .preimage_digest
                    .ok_or(PlatformError::JournalCorrupt)?;
                let preimage_size = action.preimage_size.ok_or(PlatformError::JournalCorrupt)?;
                if !exists(&removed)? {
                    return if file_matches_recovery(&destination, preimage_size, preimage_digest)? {
                        Ok(AppliedState::NotApplied)
                    } else {
                        Ok(AppliedState::Indeterminate)
                    };
                }
                if !file_matches_recovery(&removed, preimage_size, preimage_digest)? {
                    return Ok(AppliedState::Indeterminate);
                }
                if file_matches_recovery(&destination, action.size, action.digest)? {
                    Ok(AppliedState::Applied)
                } else if file_matches_recovery(&destination, preimage_size, preimage_digest)? {
                    Ok(AppliedState::NotApplied)
                } else {
                    Ok(AppliedState::Indeterminate)
                }
            }
        }
    }

    fn rollback_action(
        &self,
        index: usize,
        action: &PreparedAction,
        transaction: &Path,
    ) -> Result<(), PlatformError> {
        match action.kind {
            FileActionKind::Create | FileActionKind::Copy => {
                let stage = indexed_path(&transaction.join(STAGE_DIRECTORY), index);
                let destination = self.checked_mutation_path(
                    action.destination.as_deref().unwrap_or(&action.path),
                    true,
                )?;
                if hash_file_recovery(&destination, action.size)? != action.digest {
                    return Err(PlatformError::RecoveryRequired);
                }
                if stage.exists() && !same_regular_file(&stage, &destination)? {
                    return Err(PlatformError::RecoveryRequired);
                }
                fs::remove_file(&destination)?;
                #[cfg(test)]
                crash_at(CrashPoint::RollbackCreateCopyDestinationRemoved(index));
                sync_parent(&destination)
            }
            FileActionKind::Move => {
                let source = self.checked_mutation_path(&action.path, false)?;
                if source.exists() {
                    let destination = self.checked_mutation_path(
                        action
                            .destination
                            .as_deref()
                            .ok_or(PlatformError::JournalCorrupt)?,
                        true,
                    )?;
                    if !file_matches_recovery(&source, action.size, action.digest)?
                        || !same_regular_file(&source, &destination)?
                    {
                        return Err(PlatformError::RecoveryRequired);
                    }
                    fs::remove_file(&destination)?;
                    #[cfg(test)]
                    crash_at(CrashPoint::RollbackMoveDestinationRemoved(index));
                    return sync_parent(&destination);
                }
                let destination = self.checked_mutation_path(
                    action
                        .destination
                        .as_deref()
                        .ok_or(PlatformError::JournalCorrupt)?,
                    true,
                )?;
                if hash_file_recovery(&destination, action.size)? != action.digest {
                    return Err(PlatformError::RecoveryRequired);
                }
                fs::hard_link(&destination, &source)?;
                #[cfg(test)]
                crash_at(CrashPoint::RollbackMoveSourceLinked(index));
                sync_parent(&source)?;
                fs::remove_file(&destination)?;
                #[cfg(test)]
                crash_at(CrashPoint::RollbackMoveDestinationRemoved(index));
                sync_parent(&destination)
            }
            FileActionKind::Remove => {
                let source = self.checked_mutation_path(&action.path, false)?;
                if source.exists() {
                    return Err(PlatformError::RecoveryRequired);
                }
                let removed = indexed_path(&transaction.join(REMOVED_DIRECTORY), index);
                if hash_file_recovery(&removed, action.size)? != action.digest {
                    return Err(PlatformError::RecoveryRequired);
                }
                fs::rename(&removed, &source)?;
                #[cfg(test)]
                crash_at(CrashPoint::RollbackRemoveRestored(index));
                sync_parent(&source)?;
                sync_directory(&transaction.join(REMOVED_DIRECTORY))
            }
            FileActionKind::Replace => {
                let preimage_digest = action
                    .preimage_digest
                    .ok_or(PlatformError::JournalCorrupt)?;
                let preimage_size = action.preimage_size.ok_or(PlatformError::JournalCorrupt)?;
                let destination = self.checked_mutation_path(&action.path, true)?;
                if !file_matches_recovery(&destination, action.size, action.digest)? {
                    return Err(PlatformError::RecoveryRequired);
                }
                let removed = indexed_path(&transaction.join(REMOVED_DIRECTORY), index);
                if !file_matches_recovery(&removed, preimage_size, preimage_digest)? {
                    return Err(PlatformError::RecoveryRequired);
                }
                let max_bytes =
                    usize::try_from(preimage_size).map_err(|_| PlatformError::RecoveryRequired)?;
                let contents = read_regular_bounded(&removed, max_bytes)?;
                if contents.len() as u64 != preimage_size
                    || blake3::hash(&contents).as_bytes() != &preimage_digest
                {
                    return Err(PlatformError::RecoveryRequired);
                }
                match self.compare_and_swap_replace_inner(
                    &action.path,
                    action.digest,
                    &contents,
                    MAX_FILE_TRANSACTION_FILE_BYTES,
                )? {
                    ReplaceOutcome::Committed { new_digest, .. }
                        if new_digest == preimage_digest => {}
                    _ => return Err(PlatformError::RecoveryRequired),
                }
                #[cfg(test)]
                crash_at(CrashPoint::RollbackReplaceRestored(index));
                sync_parent(&destination)?;
                remove_file_if_present(&removed)?;
                remove_file_if_present(&indexed_path(&transaction.join(STAGE_DIRECTORY), index))?;
                sync_directory(&transaction.join(REMOVED_DIRECTORY))?;
                sync_directory(&transaction.join(STAGE_DIRECTORY))
            }
        }
    }
}

pub(crate) fn has_internal_state(root: &Path) -> Result<bool, PlatformError> {
    let state = root.join(STATE_DIRECTORY);
    match fs::symlink_metadata(&state) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let marker = state.join(FORMAT_FILE);
    match read_regular_bounded(&marker, FORMAT_MARKER.len()) {
        Ok(value) => Ok(value == FORMAT_MARKER),
        Err(PlatformError::JournalCorrupt) => Ok(false),
        Err(PlatformError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn validate_actions(workspace: &Workspace, actions: &[FileAction]) -> Result<(), PlatformError> {
    if actions.is_empty() || actions.len() > MAX_ACTIONS {
        return Err(PlatformError::InvalidMutationTarget);
    }
    let mut paths = HashSet::new();
    for action in actions {
        validate_logical(&action.path)?;
        workspace.reject_reserved(&action.path)?;
        if action.path == "." || !paths.insert(action.path.as_str()) {
            return Err(PlatformError::InvalidMutationTarget);
        }
        if let Some(destination) = &action.destination {
            validate_logical(destination)?;
            workspace.reject_reserved(destination)?;
            if destination == "." || !paths.insert(destination.as_str()) {
                return Err(PlatformError::InvalidMutationTarget);
            }
        }
        let shape_valid = match action.kind {
            FileActionKind::Create => {
                action.destination.is_none()
                    && action.expected_digest.is_none()
                    && action.content.is_some()
            }
            FileActionKind::Copy | FileActionKind::Move => {
                action.destination.is_some()
                    && action.expected_digest.is_some()
                    && action.content.is_none()
            }
            FileActionKind::Remove => {
                action.destination.is_none()
                    && action.expected_digest.is_some()
                    && action.content.is_none()
            }
            FileActionKind::Replace => {
                action.destination.is_none()
                    && action.expected_digest.is_some()
                    && action.content.is_some()
            }
        };
        if !shape_valid {
            return Err(PlatformError::InvalidMutationTarget);
        }
    }
    Ok(())
}

fn check_control<F>(control: &mut F) -> Result<(), FileTransactionFailure>
where
    F: FnMut() -> TransactionControl,
{
    match control() {
        TransactionControl::Continue => Ok(()),
        TransactionControl::Cancelled => Err(FileTransactionFailure::Cancelled),
        TransactionControl::TimedOut => Err(FileTransactionFailure::TimedOut),
    }
}

const fn control_failure(control: TransactionControl) -> Option<FileTransactionFailure> {
    match control {
        TransactionControl::Continue => None,
        TransactionControl::Cancelled => Some(FileTransactionFailure::Cancelled),
        TransactionControl::TimedOut => Some(FileTransactionFailure::TimedOut),
    }
}

fn charge_bytes(
    bytes: u64,
    limits: FileTransactionLimits,
    total: &mut u64,
) -> Result<(), StepFailure> {
    if bytes > limits.max_file_bytes {
        return Err(StepFailure::Platform(PlatformError::InputTooLarge {
            size: bytes,
            max: limits.max_file_bytes,
        }));
    }
    *total =
        total
            .checked_add(bytes)
            .ok_or(StepFailure::Platform(PlatformError::InputTooLarge {
                size: u64::MAX,
                max: limits.max_total_bytes,
            }))?;
    if *total > limits.max_total_bytes {
        Err(StepFailure::Platform(PlatformError::InputTooLarge {
            size: *total,
            max: limits.max_total_bytes,
        }))
    } else {
        Ok(())
    }
}

fn hash_file_bounded<F>(
    path: &Path,
    limits: FileTransactionLimits,
    total: &mut u64,
    control: &mut F,
) -> Result<([u8; 32], u64), StepFailure>
where
    F: FnMut() -> TransactionControl,
{
    let (digest, size) = hash_reader_bounded(
        File::open(path).map_err(PlatformError::from)?,
        limits.max_file_bytes,
        control,
    )?;
    charge_bytes(size, limits, total)?;
    Ok((digest, size))
}

fn hash_file_without_charge<F>(
    path: &Path,
    limits: FileTransactionLimits,
    control: &mut F,
) -> Result<([u8; 32], u64), StepFailure>
where
    F: FnMut() -> TransactionControl,
{
    hash_reader_bounded(
        File::open(path).map_err(PlatformError::from)?,
        limits.max_file_bytes,
        control,
    )
}

fn hash_reader_bounded<F>(
    mut reader: File,
    max_bytes: u64,
    control: &mut F,
) -> Result<([u8; 32], u64), StepFailure>
where
    F: FnMut() -> TransactionControl,
{
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        check_control(control).map_err(StepFailure::Stopped)?;
        let count = reader.read(&mut buffer).map_err(PlatformError::from)?;
        if count == 0 {
            return Ok((*hasher.finalize().as_bytes(), size));
        }
        size = size.checked_add(count as u64).ok_or(StepFailure::Platform(
            PlatformError::InputTooLarge {
                size: u64::MAX,
                max: max_bytes,
            },
        ))?;
        if size > max_bytes {
            return Err(StepFailure::Platform(PlatformError::InputTooLarge {
                size,
                max: max_bytes,
            }));
        }
        hasher.update(&buffer[..count]);
    }
}

fn copy_file_bounded<F>(
    source: &Path,
    destination: &Path,
    limits: FileTransactionLimits,
    total: &mut u64,
    control: &mut F,
) -> Result<([u8; 32], u64), StepFailure>
where
    F: FnMut() -> TransactionControl,
{
    let metadata = fs::metadata(source).map_err(PlatformError::from)?;
    if metadata.len() > limits.max_file_bytes {
        return Err(StepFailure::Platform(PlatformError::InputTooLarge {
            size: metadata.len(),
            max: limits.max_file_bytes,
        }));
    }
    let mut reader = File::open(source).map_err(PlatformError::from)?;
    let mut writer = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(PlatformError::from)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        check_control(control).map_err(StepFailure::Stopped)?;
        let count = reader.read(&mut buffer).map_err(PlatformError::from)?;
        if count == 0 {
            break;
        }
        size = size.checked_add(count as u64).ok_or(StepFailure::Platform(
            PlatformError::InputTooLarge {
                size: u64::MAX,
                max: limits.max_file_bytes,
            },
        ))?;
        if size > limits.max_file_bytes {
            return Err(StepFailure::Platform(PlatformError::InputTooLarge {
                size,
                max: limits.max_file_bytes,
            }));
        }
        writer
            .write_all(&buffer[..count])
            .map_err(PlatformError::from)?;
        hasher.update(&buffer[..count]);
    }
    writer
        .set_permissions(metadata.permissions())
        .map_err(PlatformError::from)?;
    writer.sync_all().map_err(PlatformError::from)?;
    charge_bytes(size, limits, total)?;
    Ok((*hasher.finalize().as_bytes(), size))
}

fn hash_file_recovery(path: &Path, expected_size: u64) -> Result<[u8; 32], PlatformError> {
    let size = fs::metadata(path)?.len();
    if size != expected_size {
        return Err(PlatformError::RecoveryRequired);
    }
    let mut reader = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut read_size = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            if read_size != expected_size {
                return Err(PlatformError::RecoveryRequired);
            }
            return Ok(*hasher.finalize().as_bytes());
        }
        read_size = read_size
            .checked_add(count as u64)
            .ok_or(PlatformError::RecoveryRequired)?;
        if read_size > expected_size {
            return Err(PlatformError::RecoveryRequired);
        }
        hasher.update(&buffer[..count]);
    }
}

fn file_matches_recovery(
    path: &Path,
    expected_size: u64,
    expected_digest: [u8; 32],
) -> Result<bool, PlatformError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(PlatformError::RecoveryRequired);
    }
    if metadata.len() != expected_size {
        return Ok(false);
    }
    Ok(hash_file_recovery(path, expected_size)? == expected_digest)
}

fn read_staged_replacement<F>(
    path: &Path,
    expected_size: u64,
    expected_digest: [u8; 32],
    limits: FileTransactionLimits,
    control: &mut F,
) -> Result<Vec<u8>, StepFailure>
where
    F: FnMut() -> TransactionControl,
{
    let metadata = fs::symlink_metadata(path).map_err(PlatformError::from)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != expected_size
        || expected_size > limits.max_file_bytes
    {
        return Err(StepFailure::Platform(PlatformError::JournalCorrupt));
    }
    let capacity = usize::try_from(expected_size)
        .map_err(|_| StepFailure::Platform(PlatformError::JournalCorrupt))?;
    let mut reader = File::open(path).map_err(PlatformError::from)?;
    let mut contents = Vec::with_capacity(capacity);
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = blake3::Hasher::new();
    loop {
        check_control(control).map_err(StepFailure::Stopped)?;
        let count = reader.read(&mut buffer).map_err(PlatformError::from)?;
        if count == 0 {
            break;
        }
        if contents.len().saturating_add(count) > capacity {
            return Err(StepFailure::Platform(PlatformError::JournalCorrupt));
        }
        contents.extend_from_slice(&buffer[..count]);
        hasher.update(&buffer[..count]);
    }
    if contents.len() != capacity || hasher.finalize().as_bytes() != &expected_digest {
        Err(StepFailure::Platform(PlatformError::JournalCorrupt))
    } else {
        Ok(contents)
    }
}

fn hard_link_error<F>(
    destination: &Path,
    limits: FileTransactionLimits,
    control: &mut F,
    error: io::Error,
) -> StepFailure
where
    F: FnMut() -> TransactionControl,
{
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_file() => {
            match hash_file_without_charge(destination, limits, control) {
                Ok((digest, _)) => StepFailure::Conflict(digest),
                Err(error) => error,
            }
        }
        _ => StepFailure::Platform(error.into()),
    }
}

fn encode_manifest(actions: &[PreparedAction]) -> Result<Vec<u8>, PlatformError> {
    validate_manifest_sizes(actions)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(
        &u32::try_from(actions.len())
            .map_err(|_| PlatformError::JournalCorrupt)?
            .to_be_bytes(),
    );
    for action in actions {
        bytes.push(action.kind as u8);
        encode_text(&mut bytes, &action.path)?;
        encode_optional_text(&mut bytes, action.destination.as_deref())?;
        bytes.extend_from_slice(&action.digest);
        bytes.extend_from_slice(&action.size.to_be_bytes());
        match (action.preimage_digest, action.preimage_size) {
            (Some(digest), Some(size)) => {
                bytes.push(1);
                bytes.extend_from_slice(&digest);
                bytes.extend_from_slice(&size.to_be_bytes());
            }
            (None, None) => bytes.push(0),
            _ => return Err(PlatformError::JournalCorrupt),
        }
    }
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

fn decode_manifest(bytes: &[u8]) -> Result<Vec<PreparedAction>, PlatformError> {
    if bytes.len() < MANIFEST_MAGIC.len() + 4 + 32 || bytes.len() > MAX_MANIFEST_BYTES {
        return Err(PlatformError::JournalCorrupt);
    }
    let (body, checksum) = bytes.split_at(bytes.len() - 32);
    if blake3::hash(body).as_bytes() != checksum {
        return Err(PlatformError::JournalCorrupt);
    }
    let mut cursor = 0_usize;
    if take(body, &mut cursor, MANIFEST_MAGIC.len())? != MANIFEST_MAGIC {
        return Err(PlatformError::JournalCorrupt);
    }
    let count = read_u32(body, &mut cursor)? as usize;
    if count == 0 || count > MAX_ACTIONS {
        return Err(PlatformError::JournalCorrupt);
    }
    let mut actions = Vec::with_capacity(count);
    let mut paths = HashSet::<String>::new();
    for _ in 0..count {
        let kind = match take(body, &mut cursor, 1)?[0] {
            0 => FileActionKind::Create,
            1 => FileActionKind::Copy,
            2 => FileActionKind::Move,
            3 => FileActionKind::Remove,
            4 => FileActionKind::Replace,
            _ => return Err(PlatformError::JournalCorrupt),
        };
        let path = decode_text(body, &mut cursor)?;
        let destination = decode_optional_text(body, &mut cursor)?;
        let digest: [u8; 32] = take(body, &mut cursor, 32)?
            .try_into()
            .map_err(|_| PlatformError::JournalCorrupt)?;
        let size = read_u64(body, &mut cursor)?;
        let (preimage_digest, preimage_size) = match take(body, &mut cursor, 1)?[0] {
            0 => (None, None),
            1 => (
                Some(
                    take(body, &mut cursor, 32)?
                        .try_into()
                        .map_err(|_| PlatformError::JournalCorrupt)?,
                ),
                Some(read_u64(body, &mut cursor)?),
            ),
            _ => return Err(PlatformError::JournalCorrupt),
        };
        validate_logical(&path).map_err(|_| PlatformError::JournalCorrupt)?;
        if path == "."
            || path == STATE_DIRECTORY
            || path.starts_with(".ash/")
            || !paths.insert(path.clone())
        {
            return Err(PlatformError::JournalCorrupt);
        }
        if let Some(destination) = &destination {
            validate_logical(destination).map_err(|_| PlatformError::JournalCorrupt)?;
            if destination == "."
                || destination == STATE_DIRECTORY
                || destination.starts_with(".ash/")
                || !paths.insert(destination.clone())
            {
                return Err(PlatformError::JournalCorrupt);
            }
        }
        let shape_valid = match kind {
            FileActionKind::Create | FileActionKind::Remove => {
                destination.is_none() && preimage_digest.is_none()
            }
            FileActionKind::Copy | FileActionKind::Move => {
                destination.is_some() && preimage_digest.is_none()
            }
            FileActionKind::Replace => {
                destination.is_none() && preimage_digest.is_some() && preimage_size.is_some()
            }
        };
        if !shape_valid {
            return Err(PlatformError::JournalCorrupt);
        }
        actions.push(PreparedAction {
            kind,
            path,
            destination,
            digest,
            size,
            preimage_digest,
            preimage_size,
        });
    }
    if cursor != body.len() {
        return Err(PlatformError::JournalCorrupt);
    }
    validate_manifest_sizes(&actions)?;
    Ok(actions)
}

fn validate_manifest_sizes(actions: &[PreparedAction]) -> Result<(), PlatformError> {
    let mut materialized = 0_u64;
    let mut preimages = 0_u64;
    for action in actions {
        if action.size > MAX_FILE_TRANSACTION_FILE_BYTES {
            return Err(PlatformError::JournalCorrupt);
        }
        materialized = materialized
            .checked_add(action.size)
            .ok_or(PlatformError::JournalCorrupt)?;
        if materialized > MAX_FILE_TRANSACTION_TOTAL_BYTES {
            return Err(PlatformError::JournalCorrupt);
        }
        if let Some(size) = action.preimage_size {
            if size > MAX_FILE_TRANSACTION_FILE_BYTES {
                return Err(PlatformError::JournalCorrupt);
            }
            preimages = preimages
                .checked_add(size)
                .ok_or(PlatformError::JournalCorrupt)?;
            if preimages > MAX_FILE_TRANSACTION_TOTAL_BYTES {
                return Err(PlatformError::JournalCorrupt);
            }
        }
    }
    Ok(())
}

fn encode_text(output: &mut Vec<u8>, value: &str) -> Result<(), PlatformError> {
    let length = u32::try_from(value.len()).map_err(|_| PlatformError::JournalCorrupt)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn encode_optional_text(output: &mut Vec<u8>, value: Option<&str>) -> Result<(), PlatformError> {
    if let Some(value) = value {
        encode_text(output, value)
    } else {
        output.extend_from_slice(&NONE_LENGTH.to_be_bytes());
        Ok(())
    }
}

fn decode_text(bytes: &[u8], cursor: &mut usize) -> Result<String, PlatformError> {
    let length = read_u32(bytes, cursor)?;
    if length == NONE_LENGTH || length > 4096 {
        return Err(PlatformError::JournalCorrupt);
    }
    let value = take(bytes, cursor, length as usize)?;
    String::from_utf8(value.to_vec()).map_err(|_| PlatformError::JournalCorrupt)
}

fn decode_optional_text(bytes: &[u8], cursor: &mut usize) -> Result<Option<String>, PlatformError> {
    let length = read_u32(bytes, cursor)?;
    if length == NONE_LENGTH {
        return Ok(None);
    }
    if length > 4096 {
        return Err(PlatformError::JournalCorrupt);
    }
    let value = take(bytes, cursor, length as usize)?;
    String::from_utf8(value.to_vec())
        .map(Some)
        .map_err(|_| PlatformError::JournalCorrupt)
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, PlatformError> {
    Ok(u32::from_be_bytes(
        take(bytes, cursor, 4)?
            .try_into()
            .map_err(|_| PlatformError::JournalCorrupt)?,
    ))
}

fn read_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, PlatformError> {
    Ok(u64::from_be_bytes(
        take(bytes, cursor, 8)?
            .try_into()
            .map_err(|_| PlatformError::JournalCorrupt)?,
    ))
}

fn take<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], PlatformError> {
    let end = cursor
        .checked_add(length)
        .ok_or(PlatformError::JournalCorrupt)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(PlatformError::JournalCorrupt)?;
    *cursor = end;
    Ok(value)
}

fn indexed_path(directory: &Path, index: usize) -> PathBuf {
    directory.join(format!("{index:08x}"))
}

fn rollback_digest(action: &PreparedAction) -> [u8; 32] {
    action.preimage_digest.unwrap_or(action.digest)
}

fn remove_file_if_present(path: &Path) -> Result<(), PlatformError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => Err(PlatformError::RecoveryRequired),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_new_sync(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn read_regular_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, PlatformError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes as u64
    {
        return Err(PlatformError::JournalCorrupt);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        Err(PlatformError::JournalCorrupt)
    } else {
        Ok(bytes)
    }
}

fn internal_directory_exists(path: &Path) -> Result<bool, PlatformError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(PlatformError::JournalCorrupt),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn require_internal_directory(path: &Path) -> Result<(), PlatformError> {
    if internal_directory_exists(path)? {
        Ok(())
    } else {
        Err(PlatformError::JournalCorrupt)
    }
}

fn remove_directory_if_present(path: &Path) -> Result<(), PlatformError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn set_recovery_row(rows: Option<&mut [FileActionOutcome]>, index: usize, digest: [u8; 32]) {
    if let Some(rows) = rows {
        rows[index] = FileActionOutcome {
            state: FileActionState::RecoveryRequired,
            digest: Some(digest),
        };
    }
}

fn same_regular_file(left: &Path, right: &Path) -> Result<bool, PlatformError> {
    for path in [left, right] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Ok(false);
        }
    }
    same_file::is_same_file(left, right).map_err(PlatformError::from)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), PlatformError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(windows)]
fn set_private_directory_permissions(_path: &Path) -> Result<(), PlatformError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), PlatformError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<(), PlatformError> {
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), PlatformError> {
    sync_directory(path.parent().ok_or(PlatformError::InvalidMutationTarget)?)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        COMMITTED_FILE, COMMITTED_MARKER, CrashPoint, FileAction, FileActionState,
        FileTransactionFailure, FileTransactionLimits, MANIFEST_FILE, PREPARING_DIRECTORY,
        PreparedAction, REMOVED_DIRECTORY, STAGE_DIRECTORY, TRANSACTION_DIRECTORY,
        TransactionControl, arm_crash, encode_manifest, indexed_path, same_regular_file,
        write_new_sync,
    };
    use crate::{PlatformError, WalkOptions, Workspace};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-transaction-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn limits() -> FileTransactionLimits {
        FileTransactionLimits::new(1024, 4096).expect("limits")
    }

    #[test]
    fn native_identity_distinguishes_hard_links_from_equal_content() {
        let directory = TestDirectory::new();
        let original = directory.0.join("original");
        let linked = directory.0.join("linked");
        let equal = directory.0.join("equal");
        fs::write(&original, b"same bytes").expect("original");
        fs::hard_link(&original, &linked).expect("hard link");
        fs::write(&equal, b"same bytes").expect("equal content");

        assert!(same_regular_file(&original, &linked).expect("linked identity"));
        assert!(!same_regular_file(&original, &equal).expect("separate identity"));
    }

    #[test]
    fn recovery_never_treats_equal_content_as_hard_link_proof() {
        let create_directory = TestDirectory::new();
        let create_workspace = Workspace::new(&create_directory.0).expect("create workspace");
        arm_crash(CrashPoint::CreateCopyLinked(0));
        assert!(
            catch_unwind(AssertUnwindSafe(|| create_workspace.file_transaction(
                vec![FileAction::create("created", b"same".to_vec())],
                limits(),
                || TransactionControl::Continue,
            )))
            .is_err()
        );
        drop(create_workspace);
        fs::remove_file(create_directory.0.join("created")).expect("unlink published create");
        fs::write(create_directory.0.join("created"), b"same").expect("external equal create");
        let create_restart = Workspace::new(&create_directory.0).expect("create restart");
        assert!(matches!(
            create_restart.recover_file_transactions(|| TransactionControl::Continue),
            Err(PlatformError::RecoveryRequired)
        ));
        assert_eq!(
            fs::read(create_directory.0.join("created")).expect("preserved create"),
            b"same"
        );

        let move_directory = TestDirectory::new();
        fs::write(move_directory.0.join("source"), b"same").expect("move source");
        let move_workspace = Workspace::new(&move_directory.0).expect("move workspace");
        arm_crash(CrashPoint::MoveLinked(0));
        assert!(
            catch_unwind(AssertUnwindSafe(|| move_workspace.file_transaction(
                vec![FileAction::move_file(
                    "source",
                    "destination",
                    *blake3::hash(b"same").as_bytes(),
                )],
                limits(),
                || TransactionControl::Continue,
            )))
            .is_err()
        );
        drop(move_workspace);
        fs::remove_file(move_directory.0.join("destination")).expect("unlink published move");
        fs::write(move_directory.0.join("destination"), b"same").expect("external equal move");
        let move_restart = Workspace::new(&move_directory.0).expect("move restart");
        assert!(matches!(
            move_restart.recover_file_transactions(|| TransactionControl::Continue),
            Err(PlatformError::RecoveryRequired)
        ));
        assert_eq!(
            fs::read(move_directory.0.join("source")).expect("preserved source"),
            b"same"
        );
        assert_eq!(
            fs::read(move_directory.0.join("destination")).expect("preserved destination"),
            b"same"
        );
    }

    #[test]
    fn real_transaction_recovery_covers_every_forward_durable_cutpoint() {
        let cutpoints = [
            CrashPoint::PreparingCreated,
            CrashPoint::ActionPrepared(0),
            CrashPoint::ActionPrepared(1),
            CrashPoint::ActionPrepared(2),
            CrashPoint::ActionPrepared(3),
            CrashPoint::ActionPrepared(4),
            CrashPoint::ManifestPrepared,
            CrashPoint::JournalPublished,
            CrashPoint::CreateCopyLinked(0),
            CrashPoint::CreateCopyParentSynced(0),
            CrashPoint::CreateCopyStageRemoved(0),
            CrashPoint::CreateCopyStageSynced(0),
            CrashPoint::CreateCopyLinked(1),
            CrashPoint::CreateCopyParentSynced(1),
            CrashPoint::CreateCopyStageRemoved(1),
            CrashPoint::CreateCopyStageSynced(1),
            CrashPoint::MoveLinked(2),
            CrashPoint::MoveDestinationSynced(2),
            CrashPoint::MoveSourceRemoved(2),
            CrashPoint::MoveSourceSynced(2),
            CrashPoint::RemoveRenamed(3),
            CrashPoint::RemoveSourceSynced(3),
            CrashPoint::RemoveJournalSynced(3),
            CrashPoint::ReplaceSwapped(4),
            CrashPoint::ReplaceParentSynced(4),
            CrashPoint::ReplaceStageRemoved(4),
            CrashPoint::ReplaceStageSynced(4),
            CrashPoint::CommitTempWritten,
        ];
        for point in cutpoints {
            assert_crash_recovers(point, false);
        }
        for point in [CrashPoint::CommitMarkerRenamed, CrashPoint::CommitSynced] {
            assert_crash_recovers(point, true);
        }
    }

    #[test]
    fn recovery_is_reentrant_at_every_rollback_mutation_boundary() {
        for point in [
            CrashPoint::RollbackReplaceRestored(4),
            CrashPoint::RollbackActionCompleted(4),
            CrashPoint::RollbackRemoveRestored(3),
            CrashPoint::RollbackActionCompleted(3),
            CrashPoint::RollbackMoveSourceLinked(2),
            CrashPoint::RollbackMoveDestinationRemoved(2),
            CrashPoint::RollbackActionCompleted(2),
            CrashPoint::RollbackCreateCopyDestinationRemoved(1),
            CrashPoint::RollbackActionCompleted(1),
            CrashPoint::RollbackCreateCopyDestinationRemoved(0),
            CrashPoint::RollbackActionCompleted(0),
            CrashPoint::RecoveryJournalRemoved,
        ] {
            assert_recovery_crash_reenters(point);
        }
    }

    #[test]
    fn transaction_commits_all_file_action_kinds_and_hides_internal_state() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("copy-source"), b"copy").expect("copy source");
        fs::write(directory.0.join("move-source"), b"move").expect("move source");
        fs::write(directory.0.join("remove-source"), b"remove").expect("remove source");
        fs::write(directory.0.join("replace-source"), b"before").expect("replace source");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let actions = vec![
            FileAction::create("created", b"created".to_vec()),
            FileAction::copy("copy-source", "copied", *blake3::hash(b"copy").as_bytes()),
            FileAction::move_file("move-source", "moved", *blake3::hash(b"move").as_bytes()),
            FileAction::remove("remove-source", *blake3::hash(b"remove").as_bytes()),
            FileAction::replace(
                "replace-source",
                *blake3::hash(b"before").as_bytes(),
                b"after".to_vec(),
            ),
        ];

        let outcome = workspace
            .file_transaction(actions, limits(), || TransactionControl::Continue)
            .expect("transaction");

        assert_eq!(outcome.failure, None);
        assert!(
            outcome
                .actions
                .iter()
                .all(|row| row.state == FileActionState::Committed)
        );
        assert_eq!(
            fs::read(directory.0.join("created")).expect("created"),
            b"created"
        );
        assert_eq!(
            fs::read(directory.0.join("copied")).expect("copied"),
            b"copy"
        );
        assert_eq!(
            fs::read(directory.0.join("copy-source")).expect("copy source"),
            b"copy"
        );
        assert_eq!(fs::read(directory.0.join("moved")).expect("moved"), b"move");
        assert!(!directory.0.join("move-source").exists());
        assert!(!directory.0.join("remove-source").exists());
        assert_eq!(
            fs::read(directory.0.join("replace-source")).expect("replace source"),
            b"after"
        );
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );

        let root = workspace.resolve_existing(".").expect("root");
        let entries = workspace
            .walk(
                &root,
                WalkOptions {
                    max_depth: 2,
                    include_hidden: true,
                    max_entries: 64,
                },
            )
            .expect("walk");
        assert!(
            entries
                .iter()
                .all(|entry| !entry.logical.starts_with(".ash"))
        );
        assert!(matches!(
            workspace.resolve_existing(".ash"),
            Err(PlatformError::ReservedPath)
        ));
    }

    #[test]
    fn conflict_rolls_back_prior_actions_without_touching_external_change() {
        let directory = TestDirectory::new();
        let guarded = directory.0.join("guarded");
        fs::write(&guarded, b"before").expect("guarded");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let root = directory.0.clone();
        let mut injected = false;
        let outcome = workspace
            .file_transaction(
                vec![
                    FileAction::create("created", b"created".to_vec()),
                    FileAction::remove("guarded", *blake3::hash(b"before").as_bytes()),
                ],
                limits(),
                || {
                    if !injected && root.join(".ash").join(TRANSACTION_DIRECTORY).exists() {
                        fs::write(&guarded, b"external").expect("inject conflict");
                        injected = true;
                    }
                    TransactionControl::Continue
                },
            )
            .expect("transaction");

        assert_eq!(outcome.failure, Some(FileTransactionFailure::Conflict));
        assert_eq!(outcome.actions[0].state, FileActionState::RolledBack);
        assert_eq!(outcome.actions[1].state, FileActionState::Conflict);
        assert!(!directory.0.join("created").exists());
        assert_eq!(fs::read(guarded).expect("guarded"), b"external");
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn later_apply_conflict_rolls_back_an_earlier_durable_replace() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first");
        let second = directory.0.join("second");
        fs::write(&first, b"first-old").expect("first");
        fs::write(&second, b"second-old").expect("second");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let root = directory.0.clone();
        let mut injected = false;
        let outcome = workspace
            .file_transaction(
                vec![
                    FileAction::replace(
                        "first",
                        *blake3::hash(b"first-old").as_bytes(),
                        b"first-new".to_vec(),
                    ),
                    FileAction::replace(
                        "second",
                        *blake3::hash(b"second-old").as_bytes(),
                        b"second-new".to_vec(),
                    ),
                ],
                limits(),
                || {
                    if !injected && root.join(".ash").join(TRANSACTION_DIRECTORY).exists() {
                        fs::write(&second, b"external").expect("inject conflict");
                        injected = true;
                    }
                    TransactionControl::Continue
                },
            )
            .expect("transaction");

        assert_eq!(outcome.failure, Some(FileTransactionFailure::Conflict));
        assert_eq!(outcome.actions[0].state, FileActionState::RolledBack);
        assert_eq!(
            outcome.actions[0].digest,
            Some(*blake3::hash(b"first-old").as_bytes())
        );
        assert_eq!(outcome.actions[1].state, FileActionState::Conflict);
        assert_eq!(fs::read(first).expect("first"), b"first-old");
        assert_eq!(fs::read(second).expect("second"), b"external");
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn cancellation_before_preparation_has_no_filesystem_effect() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let outcome = workspace
            .file_transaction(
                vec![FileAction::create("created", b"created".to_vec())],
                limits(),
                || TransactionControl::Cancelled,
            )
            .expect("transaction");

        assert_eq!(outcome.failure, Some(FileTransactionFailure::Cancelled));
        assert_eq!(outcome.actions[0].state, FileActionState::Skipped);
        assert!(!directory.0.join("created").exists());
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn startup_recovery_rolls_back_an_uncommitted_visible_create() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        install_visible_create(&workspace, "created", b"created", false);

        assert!(
            workspace
                .recover_file_transactions(|| TransactionControl::Continue)
                .expect("recover")
        );
        assert!(!directory.0.join("created").exists());
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn startup_recovery_finalizes_a_durably_committed_transaction() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        install_visible_create(&workspace, "created", b"created", true);

        assert!(
            workspace
                .recover_file_transactions(|| TransactionControl::Continue)
                .expect("recover")
        );
        assert_eq!(
            fs::read(directory.0.join("created")).expect("created"),
            b"created"
        );
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn replace_recovery_covers_published_applied_and_committed_cutpoints() {
        for (applied, committed, expected) in [
            (false, false, b"before".as_slice()),
            (true, false, b"before".as_slice()),
            (true, true, b"after".as_slice()),
        ] {
            let directory = TestDirectory::new();
            fs::write(directory.0.join("file"), b"before").expect("file");
            let workspace = Workspace::new(&directory.0).expect("workspace");
            install_replace_cutpoint(&workspace, "file", b"before", b"after", applied, committed);
            drop(workspace);

            let restarted = Workspace::new(&directory.0).expect("restart workspace");
            assert!(
                restarted
                    .recover_file_transactions(|| TransactionControl::Continue)
                    .expect("recover")
            );
            assert_eq!(fs::read(directory.0.join("file")).expect("file"), expected);
            assert!(
                !directory
                    .0
                    .join(".ash")
                    .join(TRANSACTION_DIRECTORY)
                    .exists()
            );
        }
    }

    #[test]
    fn replace_recovery_preserves_ambiguous_external_content_and_journal() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("file"), b"before").expect("file");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        install_replace_cutpoint(&workspace, "file", b"before", b"after", true, false);
        fs::write(directory.0.join("file"), b"external").expect("external");
        drop(workspace);

        let restarted = Workspace::new(&directory.0).expect("restart workspace");
        assert!(matches!(
            restarted.recover_file_transactions(|| TransactionControl::Continue),
            Err(PlatformError::RecoveryRequired)
        ));
        assert_eq!(
            fs::read(directory.0.join("file")).expect("file"),
            b"external"
        );
        assert!(
            directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn recovery_fails_closed_when_visible_content_no_longer_matches_manifest() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        install_visible_create(&workspace, "created", b"created", false);
        fs::write(directory.0.join("created"), b"externally changed").expect("external write");

        assert!(matches!(
            workspace.recover_file_transactions(|| TransactionControl::Continue),
            Err(PlatformError::RecoveryRequired)
        ));
        assert_eq!(
            fs::read(directory.0.join("created")).expect("created"),
            b"externally changed"
        );
        assert!(
            directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn corrupt_manifest_is_preserved_and_rejected() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        workspace.ensure_internal_state().expect("state");
        let transaction = workspace.state_directory().join(TRANSACTION_DIRECTORY);
        fs::create_dir(&transaction).expect("transaction directory");
        fs::create_dir(transaction.join(STAGE_DIRECTORY)).expect("stage directory");
        fs::create_dir(transaction.join(REMOVED_DIRECTORY)).expect("removed directory");
        write_new_sync(&transaction.join(MANIFEST_FILE), b"corrupt").expect("manifest");

        assert!(matches!(
            workspace.recover_file_transactions(|| TransactionControl::Continue),
            Err(PlatformError::JournalCorrupt)
        ));
        assert!(transaction.exists());
    }

    #[test]
    fn durable_limits_bound_both_new_transactions_and_recovery_manifests() {
        assert!(
            FileTransactionLimits::new(super::MAX_FILE_TRANSACTION_FILE_BYTES + 1, u64::MAX)
                .is_err()
        );
        let oversized = PreparedAction {
            kind: super::FileActionKind::Create,
            path: "created".to_owned(),
            destination: None,
            digest: [0; 32],
            size: super::MAX_FILE_TRANSACTION_FILE_BYTES + 1,
            preimage_digest: None,
            preimage_size: None,
        };
        assert!(matches!(
            encode_manifest(&[oversized]),
            Err(PlatformError::JournalCorrupt)
        ));
    }

    #[test]
    fn non_ash_dot_ash_directory_remains_readable_but_cannot_be_taken_over() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join(".ash")).expect("state-like directory");
        fs::write(directory.0.join(".ash").join("FORMAT"), b"user data").expect("marker");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        assert_eq!(
            workspace
                .read_sync(
                    &workspace
                        .resolve_existing(".ash/FORMAT")
                        .expect("resolve user data")
                )
                .expect("read user data"),
            b"user data"
        );
        assert!(matches!(
            workspace.file_transaction(
                vec![FileAction::create("created", Vec::new())],
                limits(),
                || TransactionControl::Continue
            ),
            Err(PlatformError::ReservedPath)
        ));
    }

    fn assert_crash_recovers(point: CrashPoint, committed: bool) {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("copy-source"), b"copy").expect("copy source");
        fs::write(directory.0.join("move-source"), b"move").expect("move source");
        fs::write(directory.0.join("remove-source"), b"remove").expect("remove source");
        fs::write(directory.0.join("replace-source"), b"before").expect("replace source");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        arm_crash(point);
        let crashed = catch_unwind(AssertUnwindSafe(|| {
            workspace.file_transaction(crash_actions(), limits(), || TransactionControl::Continue)
        }));
        assert!(crashed.is_err(), "cutpoint was not reached: {point:?}");
        drop(workspace);

        let restarted = Workspace::new(&directory.0).expect("restart workspace");
        assert!(
            restarted
                .recover_file_transactions(|| TransactionControl::Continue)
                .expect("recover transaction"),
            "recovery did no work after {point:?}"
        );
        assert_workspace_state(&directory.0, committed, point);
        assert!(
            !directory.0.join(".ash").join(PREPARING_DIRECTORY).exists(),
            "preparing journal remains after {point:?}"
        );
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists(),
            "published journal remains after {point:?}"
        );
    }

    fn assert_recovery_crash_reenters(recovery_point: CrashPoint) {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("copy-source"), b"copy").expect("copy source");
        fs::write(directory.0.join("move-source"), b"move").expect("move source");
        fs::write(directory.0.join("remove-source"), b"remove").expect("remove source");
        fs::write(directory.0.join("replace-source"), b"before").expect("replace source");

        let workspace = Workspace::new(&directory.0).expect("workspace");
        arm_crash(CrashPoint::CommitTempWritten);
        let forward_crash = catch_unwind(AssertUnwindSafe(|| {
            workspace.file_transaction(crash_actions(), limits(), || TransactionControl::Continue)
        }));
        assert!(forward_crash.is_err(), "forward crash was not reached");
        drop(workspace);

        let first_restart = Workspace::new(&directory.0).expect("first restart");
        arm_crash(recovery_point);
        let recovery_crash = catch_unwind(AssertUnwindSafe(|| {
            first_restart.recover_file_transactions(|| TransactionControl::Continue)
        }));
        assert!(
            recovery_crash.is_err(),
            "recovery cutpoint was not reached: {recovery_point:?}"
        );
        drop(first_restart);

        let second_restart = Workspace::new(&directory.0).expect("second restart");
        let recovered = second_restart
            .recover_file_transactions(|| TransactionControl::Continue)
            .expect("re-enter recovery");
        assert_eq!(
            recovered,
            recovery_point != CrashPoint::RecoveryJournalRemoved,
            "unexpected recovery work after {recovery_point:?}"
        );
        assert_workspace_state(&directory.0, false, recovery_point);
        assert!(
            !directory
                .0
                .join(".ash")
                .join(TRANSACTION_DIRECTORY)
                .exists(),
            "journal remains after re-entering {recovery_point:?}"
        );
    }

    fn crash_actions() -> Vec<FileAction> {
        vec![
            FileAction::create("created", b"created".to_vec()),
            FileAction::copy("copy-source", "copied", *blake3::hash(b"copy").as_bytes()),
            FileAction::move_file("move-source", "moved", *blake3::hash(b"move").as_bytes()),
            FileAction::remove("remove-source", *blake3::hash(b"remove").as_bytes()),
            FileAction::replace(
                "replace-source",
                *blake3::hash(b"before").as_bytes(),
                b"after".to_vec(),
            ),
        ]
    }

    fn assert_workspace_state(root: &std::path::Path, committed: bool, point: CrashPoint) {
        assert_eq!(
            root.join("created").exists(),
            committed,
            "created state after {point:?}"
        );
        assert_eq!(
            root.join("copied").exists(),
            committed,
            "copy destination after {point:?}"
        );
        assert_eq!(
            fs::read(root.join("copy-source")).expect("copy source"),
            b"copy",
            "copy source after {point:?}"
        );
        assert_eq!(
            root.join("move-source").exists(),
            !committed,
            "move source after {point:?}"
        );
        assert_eq!(
            root.join("moved").exists(),
            committed,
            "move destination after {point:?}"
        );
        assert_eq!(
            root.join("remove-source").exists(),
            !committed,
            "removed source after {point:?}"
        );
        assert_eq!(
            fs::read(root.join("replace-source")).expect("replace source"),
            if committed {
                b"after".as_slice()
            } else {
                b"before".as_slice()
            },
            "replace state after {point:?}"
        );
    }

    fn install_visible_create(
        workspace: &Workspace,
        logical: &str,
        contents: &[u8],
        committed: bool,
    ) {
        workspace.ensure_internal_state().expect("state");
        let transaction = workspace.state_directory().join(TRANSACTION_DIRECTORY);
        fs::create_dir(&transaction).expect("transaction directory");
        fs::create_dir(transaction.join(STAGE_DIRECTORY)).expect("stage directory");
        fs::create_dir(transaction.join(REMOVED_DIRECTORY)).expect("removed directory");
        let action = PreparedAction {
            kind: super::FileActionKind::Create,
            path: logical.to_owned(),
            destination: None,
            digest: *blake3::hash(contents).as_bytes(),
            size: contents.len() as u64,
            preimage_digest: None,
            preimage_size: None,
        };
        write_new_sync(
            &transaction.join(MANIFEST_FILE),
            &encode_manifest(std::slice::from_ref(&action)).expect("manifest"),
        )
        .expect("write manifest");
        let stage = indexed_path(&transaction.join(STAGE_DIRECTORY), 0);
        write_new_sync(&stage, contents).expect("write stage");
        fs::hard_link(&stage, workspace.native_root().join(logical)).expect("publish create");
        fs::remove_file(stage).expect("remove stage link");
        if committed {
            write_new_sync(&transaction.join(COMMITTED_FILE), COMMITTED_MARKER)
                .expect("commit marker");
        }
    }

    fn install_replace_cutpoint(
        workspace: &Workspace,
        logical: &str,
        before: &[u8],
        after: &[u8],
        applied: bool,
        committed: bool,
    ) {
        assert!(!committed || applied);
        workspace.ensure_internal_state().expect("state");
        let transaction = workspace.state_directory().join(TRANSACTION_DIRECTORY);
        fs::create_dir(&transaction).expect("transaction directory");
        fs::create_dir(transaction.join(STAGE_DIRECTORY)).expect("stage directory");
        fs::create_dir(transaction.join(REMOVED_DIRECTORY)).expect("removed directory");
        let action = PreparedAction {
            kind: super::FileActionKind::Replace,
            path: logical.to_owned(),
            destination: None,
            digest: *blake3::hash(after).as_bytes(),
            size: after.len() as u64,
            preimage_digest: Some(*blake3::hash(before).as_bytes()),
            preimage_size: Some(before.len() as u64),
        };
        write_new_sync(
            &transaction.join(MANIFEST_FILE),
            &encode_manifest(std::slice::from_ref(&action)).expect("manifest"),
        )
        .expect("write manifest");
        let stage = indexed_path(&transaction.join(STAGE_DIRECTORY), 0);
        let removed = indexed_path(&transaction.join(REMOVED_DIRECTORY), 0);
        write_new_sync(&stage, after).expect("stage");
        write_new_sync(&removed, before).expect("preimage");
        if applied {
            fs::write(workspace.native_root().join(logical), after).expect("visible replace");
            fs::remove_file(stage).expect("remove stage");
        }
        if committed {
            write_new_sync(&transaction.join(COMMITTED_FILE), COMMITTED_MARKER)
                .expect("commit marker");
        }
    }
}
