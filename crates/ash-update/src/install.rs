use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

use atomicwrites::{AllowOverwrite, AtomicFile};
use semver::Version;
use serde::{Deserialize, Serialize};
use tempfile::Builder;

use crate::archive::{ExtractedPackage, sha256_file, validate_local_version};
use crate::manifest::{UpdateDecision, UpdateError, VerifiedRelease};

const REPOSITORY: &str = "A3S-Lab/ash";
const MAX_LOCAL_METADATA_BYTES: u64 = 64 * 1024;
const RECEIPT_NAME: &str = "install-receipt.json";
const STATE_NAME: &str = "update-state.json";
const JOURNAL_NAME: &str = "update-journal.json";
const LOCK_NAME: &str = ".install-lock";
#[cfg(unix)]
static NEXT_LINK: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct InstallationInfo {
    prefix: PathBuf,
    current_version: String,
    target: String,
    highest_sequence: u64,
    highest_manifest_sha256: Option<String>,
}

impl InstallationInfo {
    #[must_use]
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    #[must_use]
    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    #[must_use]
    pub const fn highest_sequence(&self) -> u64 {
        self.highest_sequence
    }

    #[must_use]
    pub fn highest_manifest_sha256(&self) -> Option<&str> {
        self.highest_manifest_sha256.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationOutcome {
    Current { version: String },
    Activated { version: String },
    HelperRequired { candidate: PathBuf, prefix: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryOutcome {
    Clean,
    Finalized { version: String },
    RolledBack { version: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallReceipt {
    schema: u8,
    repository: String,
    version: String,
    target: String,
    prefix: String,
    launcher: String,
    path_added: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateState {
    schema: u8,
    active: String,
    previous: Option<String>,
    highest_sequence: u64,
    highest_manifest_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateJournal {
    schema: u8,
    target: String,
    old_version: String,
    new_version: String,
    sequence: u64,
    manifest_sha256: String,
    prior_previous: Option<String>,
    prior_highest_sequence: u64,
    prior_manifest_sha256: Option<String>,
}

struct Context {
    prefix: PathBuf,
    receipt_path: PathBuf,
    state_path: PathBuf,
    journal_path: PathBuf,
    receipt: InstallReceipt,
    state: UpdateState,
    journal: Option<UpdateJournal>,
}

pub fn inspect_installation(prefix: &Path, target: &str) -> Result<InstallationInfo, UpdateError> {
    let _lock = InstallLock::acquire(prefix)?;
    let context = load_context(prefix, target, false)?;
    Ok(InstallationInfo {
        prefix: context.prefix,
        current_version: context.receipt.version,
        target: context.receipt.target,
        highest_sequence: context.state.highest_sequence,
        highest_manifest_sha256: context.state.highest_manifest_sha256,
    })
}

pub fn confirm_current_release(
    prefix: &Path,
    target: &str,
    release: &VerifiedRelease,
) -> Result<(), UpdateError> {
    let _lock = InstallLock::acquire(prefix)?;
    let mut context = load_context(prefix, target, false)?;
    if release.artifact().target() != target
        || release.manifest().version() != context.receipt.version
    {
        return Err(UpdateError::Target);
    }
    validate_anchor(&context.state, release)?;
    confirm_current(&mut context, release)
}

pub fn recover_installation<F>(
    prefix: &Path,
    target: &str,
    health: F,
) -> Result<RecoveryOutcome, UpdateError>
where
    F: Fn(&Path, &str, &str) -> Result<(), UpdateError>,
{
    let _lock = InstallLock::acquire(prefix)?;
    let mut context = load_context(prefix, target, true)?;
    let Some(journal) = context.journal.clone() else {
        return Ok(RecoveryOutcome::Clean);
    };
    validate_journal(&context, &journal)?;
    if is_active_version(&context, &journal.new_version)?
        && health(launcher(&context), &journal.new_version, &journal.target).is_ok()
    {
        finalize(&mut context, &journal)?;
        return Ok(RecoveryOutcome::Finalized {
            version: journal.new_version,
        });
    }
    restore_prior(&mut context, &journal, &health)?;
    Ok(RecoveryOutcome::RolledBack {
        version: journal.old_version,
    })
}

pub fn install_release<F>(
    prefix: &Path,
    target: &str,
    package: &ExtractedPackage,
    release: &VerifiedRelease,
    health: F,
) -> Result<ActivationOutcome, UpdateError>
where
    F: Fn(&Path, &str, &str) -> Result<(), UpdateError>,
{
    let _lock = InstallLock::acquire(prefix)?;
    let mut context = load_context(prefix, target, false)?;
    if release.artifact().target() != target {
        return Err(UpdateError::Target);
    }
    validate_anchor(&context.state, release)?;
    let current = Version::parse(&context.receipt.version).map_err(|_| UpdateError::Version)?;
    let next = Version::parse(release.manifest().version()).map_err(|_| UpdateError::Version)?;
    if next == current {
        confirm_current(&mut context, release)?;
        return Ok(ActivationOutcome::Current {
            version: context.receipt.version,
        });
    }
    if next < current && release.decision() != UpdateDecision::SignedRollback {
        return Err(UpdateError::RollbackDenied);
    }
    let candidate = stage_version(&context, package, release)?;
    health(&candidate, release.manifest().version(), target)?;
    let journal = UpdateJournal {
        schema: 1,
        target: target.to_owned(),
        old_version: context.receipt.version.clone(),
        new_version: release.manifest().version().to_owned(),
        sequence: release.manifest().sequence(),
        manifest_sha256: release.manifest_sha256().to_owned(),
        prior_previous: context.state.previous.clone(),
        prior_highest_sequence: context.state.highest_sequence,
        prior_manifest_sha256: context.state.highest_manifest_sha256.clone(),
    };
    validate_journal(&context, &journal)?;
    write_json(&context.journal_path, &journal)?;
    context.journal = Some(journal.clone());

    #[cfg(unix)]
    {
        apply_pending(&mut context, &journal, &health)?;
        Ok(ActivationOutcome::Activated {
            version: journal.new_version,
        })
    }
    #[cfg(windows)]
    {
        Ok(ActivationOutcome::HelperRequired {
            candidate,
            prefix: context.prefix,
        })
    }
}

fn confirm_current(context: &mut Context, release: &VerifiedRelease) -> Result<(), UpdateError> {
    let binary = version_binary(context, &context.receipt.version)?;
    let (size, digest) = sha256_file(&binary)?;
    if size != release.artifact().binary_size() || digest != release.artifact().binary_sha256() {
        return Err(UpdateError::Package);
    }
    context.state.highest_sequence = release.manifest().sequence();
    context.state.highest_manifest_sha256 = Some(release.manifest_sha256().to_owned());
    write_json(&context.state_path, &context.state)?;
    sync_directory(&context.prefix)
}

pub fn rollback_installation<F>(
    prefix: &Path,
    target: &str,
    health: F,
) -> Result<ActivationOutcome, UpdateError>
where
    F: Fn(&Path, &str, &str) -> Result<(), UpdateError>,
{
    let _lock = InstallLock::acquire(prefix)?;
    let mut context = load_context(prefix, target, false)?;
    let previous = context
        .state
        .previous
        .clone()
        .ok_or(UpdateError::NoRollback)?;
    let candidate = version_binary(&context, &previous)?;
    health(&candidate, &previous, target)?;
    let digest = context
        .state
        .highest_manifest_sha256
        .clone()
        .ok_or(UpdateError::NoRollback)?;
    let journal = UpdateJournal {
        schema: 1,
        target: target.to_owned(),
        old_version: context.receipt.version.clone(),
        new_version: previous,
        sequence: context.state.highest_sequence,
        manifest_sha256: digest,
        prior_previous: context.state.previous.clone(),
        prior_highest_sequence: context.state.highest_sequence,
        prior_manifest_sha256: context.state.highest_manifest_sha256.clone(),
    };
    validate_journal(&context, &journal)?;
    write_json(&context.journal_path, &journal)?;
    context.journal = Some(journal.clone());

    #[cfg(unix)]
    {
        apply_pending(&mut context, &journal, &health)?;
        Ok(ActivationOutcome::Activated {
            version: journal.new_version,
        })
    }
    #[cfg(windows)]
    {
        Ok(ActivationOutcome::HelperRequired {
            candidate,
            prefix: context.prefix,
        })
    }
}

pub fn complete_pending_activation<F>(
    prefix: &Path,
    target: &str,
    health: F,
) -> Result<ActivationOutcome, UpdateError>
where
    F: Fn(&Path, &str, &str) -> Result<(), UpdateError>,
{
    let _lock = InstallLock::acquire(prefix)?;
    let mut context = load_context(prefix, target, true)?;
    let journal = context.journal.clone().ok_or(UpdateError::PendingUpdate)?;
    validate_journal(&context, &journal)?;
    apply_pending(&mut context, &journal, &health)?;
    Ok(ActivationOutcome::Activated {
        version: journal.new_version,
    })
}

fn validate_anchor(state: &UpdateState, release: &VerifiedRelease) -> Result<(), UpdateError> {
    if release.manifest().sequence() < state.highest_sequence
        || (release.manifest().sequence() == state.highest_sequence
            && state.highest_sequence != 0
            && state.highest_manifest_sha256.as_deref() != Some(release.manifest_sha256()))
    {
        Err(UpdateError::SequenceRollback)
    } else {
        Ok(())
    }
}

fn stage_version(
    context: &Context,
    package: &ExtractedPackage,
    release: &VerifiedRelease,
) -> Result<PathBuf, UpdateError> {
    let versions = context.prefix.join("versions");
    ensure_owned_directory(&versions)?;
    let destination = versions.join(release.manifest().version());
    if destination.exists() {
        let binary = validate_local_version(
            &destination,
            release.manifest().version(),
            release.artifact().target(),
        )?;
        let (size, digest) = sha256_file(&binary)?;
        if size != release.artifact().binary_size() || digest != release.artifact().binary_sha256()
        {
            return Err(UpdateError::Package);
        }
        return Ok(binary);
    }

    let temporary = Builder::new().prefix(".candidate-").tempdir_in(&versions)?;
    let binary_name = binary_name(release.artifact().target());
    for name in [
        binary_name,
        "LICENSE",
        "THIRD-PARTY-LICENSES",
        "release.json",
    ] {
        let source = package.root().join(name);
        let destination = temporary.path().join(name);
        fs::copy(source, &destination)?;
        OpenOptions::new()
            .write(true)
            .open(&destination)?
            .sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            temporary.path().join(binary_name),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    sync_directory(temporary.path())?;
    let temporary = temporary.keep();
    fs::rename(&temporary, &destination)?;
    sync_directory(&versions)?;
    let binary = validate_local_version(
        &destination,
        release.manifest().version(),
        release.artifact().target(),
    )?;
    let (size, digest) = sha256_file(&binary)?;
    if size != release.artifact().binary_size() || digest != release.artifact().binary_sha256() {
        return Err(UpdateError::Package);
    }
    Ok(binary)
}

fn apply_pending<F>(
    context: &mut Context,
    journal: &UpdateJournal,
    health: &F,
) -> Result<(), UpdateError>
where
    F: Fn(&Path, &str, &str) -> Result<(), UpdateError>,
{
    activate_version(context, &journal.new_version)?;
    if health(launcher(context), &journal.new_version, &journal.target).is_err() {
        restore_prior(context, journal, health)?;
        return Err(UpdateError::Health);
    }
    finalize(context, journal)
}

fn finalize(context: &mut Context, journal: &UpdateJournal) -> Result<(), UpdateError> {
    let highest_sequence = journal.prior_highest_sequence.max(journal.sequence);
    let highest_manifest_sha256 = if journal.sequence >= journal.prior_highest_sequence {
        Some(journal.manifest_sha256.clone())
    } else {
        journal.prior_manifest_sha256.clone()
    };
    context.state = UpdateState {
        schema: 1,
        active: journal.new_version.clone(),
        previous: Some(journal.old_version.clone()),
        highest_sequence,
        highest_manifest_sha256,
    };
    context.receipt.version.clone_from(&journal.new_version);
    write_json(&context.state_path, &context.state)?;
    write_json(&context.receipt_path, &context.receipt)?;
    fs::remove_file(&context.journal_path)?;
    sync_directory(&context.prefix)?;
    context.journal = None;
    Ok(())
}

fn restore_prior<F>(
    context: &mut Context,
    journal: &UpdateJournal,
    health: &F,
) -> Result<(), UpdateError>
where
    F: Fn(&Path, &str, &str) -> Result<(), UpdateError>,
{
    activate_version(context, &journal.old_version)?;
    health(launcher(context), &journal.old_version, &journal.target)
        .map_err(|_| UpdateError::Activation)?;
    context.state = UpdateState {
        schema: 1,
        active: journal.old_version.clone(),
        previous: journal.prior_previous.clone(),
        highest_sequence: journal.prior_highest_sequence,
        highest_manifest_sha256: journal.prior_manifest_sha256.clone(),
    };
    context.receipt.version.clone_from(&journal.old_version);
    write_json(&context.state_path, &context.state)?;
    write_json(&context.receipt_path, &context.receipt)?;
    fs::remove_file(&context.journal_path)?;
    sync_directory(&context.prefix)?;
    context.journal = None;
    Ok(())
}

fn load_context(prefix: &Path, target: &str, allow_pending: bool) -> Result<Context, UpdateError> {
    let prefix = fs::canonicalize(prefix)?;
    if !prefix.is_absolute() || prefix.parent().is_none() {
        return Err(UpdateError::Installation);
    }
    let receipt_path = prefix.join(RECEIPT_NAME);
    let state_path = prefix.join(STATE_NAME);
    let journal_path = prefix.join(JOURNAL_NAME);
    let receipt: InstallReceipt = read_json(&receipt_path)?;
    validate_receipt(&prefix, target, &receipt)?;
    let state = if state_path.exists() {
        read_json(&state_path)?
    } else {
        UpdateState {
            schema: 1,
            active: receipt.version.clone(),
            previous: None,
            highest_sequence: 0,
            highest_manifest_sha256: None,
        }
    };
    validate_state(&state)?;
    let journal = if journal_path.exists() {
        Some(read_json(&journal_path)?)
    } else {
        None
    };
    if journal.is_some() && !allow_pending {
        return Err(UpdateError::PendingUpdate);
    }
    if journal.is_none() && state.active != receipt.version {
        return Err(UpdateError::Installation);
    }
    let context = Context {
        prefix,
        receipt_path,
        state_path,
        journal_path,
        receipt,
        state,
        journal,
    };
    version_binary(&context, &context.receipt.version)?;
    if context.journal.is_none() && !is_active_version(&context, &context.receipt.version)? {
        return Err(UpdateError::Installation);
    }
    Ok(context)
}

fn validate_receipt(
    prefix: &Path,
    target: &str,
    receipt: &InstallReceipt,
) -> Result<(), UpdateError> {
    if receipt.schema != 1
        || receipt.repository != REPOSITORY
        || receipt.target != target
        || Version::parse(&receipt.version).is_err()
    {
        return Err(UpdateError::Installation);
    }
    let recorded_prefix_path = PathBuf::from(&receipt.prefix);
    if !recorded_prefix_path.is_absolute() {
        return Err(UpdateError::Installation);
    }
    let recorded_prefix = fs::canonicalize(&recorded_prefix_path)?;
    if recorded_prefix != prefix {
        return Err(UpdateError::Installation);
    }
    let launcher = PathBuf::from(&receipt.launcher);
    if !launcher.is_absolute() || receipt.profile.as_deref().is_some_and(has_control) {
        return Err(UpdateError::Installation);
    }
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(&launcher)?;
        if !metadata.file_type().is_symlink()
            || fs::read_link(&launcher)? != recorded_prefix_path.join("active").join("ash")
        {
            return Err(UpdateError::Installation);
        }
    }
    #[cfg(windows)]
    {
        let parent = launcher.parent().ok_or(UpdateError::Installation)?;
        if parent.parent().is_none() {
            return Err(UpdateError::Installation);
        }
        let metadata = fs::symlink_metadata(&launcher)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(UpdateError::Installation);
        }
        let parent = fs::canonicalize(parent)?;
        let versions = prefix.join("versions");
        if parent.starts_with(&versions) {
            return Err(UpdateError::Installation);
        }
    }
    Ok(())
}

fn validate_state(state: &UpdateState) -> Result<(), UpdateError> {
    if state.schema != 1
        || Version::parse(&state.active).is_err()
        || state
            .previous
            .as_deref()
            .is_some_and(|value| Version::parse(value).is_err() || value == state.active)
        || (state.highest_sequence == 0) != state.highest_manifest_sha256.is_none()
        || state
            .highest_manifest_sha256
            .as_deref()
            .is_some_and(|value| !canonical_digest(value))
    {
        Err(UpdateError::Installation)
    } else {
        Ok(())
    }
}

fn validate_journal(context: &Context, journal: &UpdateJournal) -> Result<(), UpdateError> {
    if journal.schema != 1
        || journal.target != context.receipt.target
        || Version::parse(&journal.old_version).is_err()
        || Version::parse(&journal.new_version).is_err()
        || journal.old_version == journal.new_version
        || journal.sequence == 0
        || journal.sequence < journal.prior_highest_sequence
        || (journal.sequence == journal.prior_highest_sequence
            && journal.prior_manifest_sha256.as_deref() != Some(journal.manifest_sha256.as_str()))
        || !canonical_digest(&journal.manifest_sha256)
        || (journal.prior_highest_sequence == 0) != journal.prior_manifest_sha256.is_none()
        || journal
            .prior_manifest_sha256
            .as_deref()
            .is_some_and(|value| !canonical_digest(value))
        || journal
            .prior_previous
            .as_deref()
            .is_some_and(|value| Version::parse(value).is_err())
    {
        return Err(UpdateError::Installation);
    }
    let prior_state = UpdateState {
        schema: 1,
        active: journal.old_version.clone(),
        previous: journal.prior_previous.clone(),
        highest_sequence: journal.prior_highest_sequence,
        highest_manifest_sha256: journal.prior_manifest_sha256.clone(),
    };
    validate_state(&prior_state)?;
    let final_state = UpdateState {
        schema: 1,
        active: journal.new_version.clone(),
        previous: Some(journal.old_version.clone()),
        highest_sequence: journal.prior_highest_sequence.max(journal.sequence),
        highest_manifest_sha256: if journal.sequence >= journal.prior_highest_sequence {
            Some(journal.manifest_sha256.clone())
        } else {
            journal.prior_manifest_sha256.clone()
        },
    };
    validate_state(&final_state)?;
    if (context.state != prior_state && context.state != final_state)
        || (context.receipt.version != journal.old_version
            && context.receipt.version != journal.new_version)
        || (context.state == prior_state && context.receipt.version != journal.old_version)
    {
        return Err(UpdateError::Installation);
    }
    version_binary(context, &journal.old_version)?;
    version_binary(context, &journal.new_version)?;
    Ok(())
}

fn version_binary(context: &Context, version: &str) -> Result<PathBuf, UpdateError> {
    if Version::parse(version).is_err() {
        return Err(UpdateError::Installation);
    }
    let root = context.prefix.join("versions").join(version);
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(UpdateError::Installation);
    }
    validate_local_version(&root, version, &context.receipt.target)
}

fn launcher(context: &Context) -> &Path {
    Path::new(&context.receipt.launcher)
}

fn binary_name(target: &str) -> &'static str {
    if target.contains("windows") {
        "ash.exe"
    } else {
        "ash"
    }
}

fn is_active_version(context: &Context, version: &str) -> Result<bool, UpdateError> {
    let binary = version_binary(context, version)?;
    #[cfg(unix)]
    {
        let _ = &binary;
        let active = context.prefix.join("active");
        let metadata = fs::symlink_metadata(&active)?;
        Ok(metadata.file_type().is_symlink()
            && fs::read_link(active)? == PathBuf::from("versions").join(version))
    }
    #[cfg(windows)]
    {
        let (expected_size, expected_digest) = sha256_file(&binary)?;
        let (actual_size, actual_digest) = sha256_file(launcher(context))?;
        Ok(expected_size == actual_size && expected_digest == actual_digest)
    }
}

fn activate_version(context: &Context, version: &str) -> Result<(), UpdateError> {
    let binary = version_binary(context, version)?;
    #[cfg(unix)]
    {
        let _ = binary;
        use std::os::unix::fs::symlink;

        let active = context.prefix.join("active");
        let metadata = fs::symlink_metadata(&active)?;
        if !metadata.file_type().is_symlink() {
            return Err(UpdateError::Installation);
        }
        let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
        let temporary = context
            .prefix
            .join(format!(".active-{}-{id}", std::process::id()));
        symlink(PathBuf::from("versions").join(version), &temporary)?;
        if let Err(error) = fs::rename(&temporary, &active) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        sync_directory(&context.prefix)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        atomic_copy(&binary, launcher(context))
    }
}

fn ensure_owned_directory(path: &Path) -> Result<(), UpdateError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(UpdateError::Installation);
        }
    } else {
        fs::create_dir(path)?;
    }
    Ok(())
}

fn read_json<T>(path: &Path) -> Result<T, UpdateError>
where
    T: for<'de> Deserialize<'de>,
{
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_LOCAL_METADATA_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| UpdateError::Installation)? > MAX_LOCAL_METADATA_BYTES
    {
        return Err(UpdateError::Installation);
    }
    serde_json::from_slice(&bytes).map_err(|_| UpdateError::Installation)
}

fn write_json<T>(path: &Path, value: &T) -> Result<(), UpdateError>
where
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(value).map_err(|_| UpdateError::Installation)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    match AtomicFile::new(path, AllowOverwrite).write(|file| -> io::Result<()> {
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()
    }) {
        Ok(()) => Ok(()),
        Err(atomicwrites::Error::Internal(error) | atomicwrites::Error::User(error)) => {
            Err(error.into())
        }
    }
}

#[cfg(windows)]
fn atomic_copy(source: &Path, destination: &Path) -> Result<(), UpdateError> {
    let mut source = File::open(source)?;
    match AtomicFile::new(destination, AllowOverwrite).write(|output| -> io::Result<()> {
        io::copy(&mut source, output)?;
        output.flush()?;
        output.sync_all()
    }) {
        Ok(()) => Ok(()),
        Err(atomicwrites::Error::Internal(error) | atomicwrites::Error::User(error)) => {
            Err(error.into())
        }
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), UpdateError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<(), UpdateError> {
    Ok(())
}

fn canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

struct InstallLock {
    path: PathBuf,
    #[cfg(windows)]
    file: Option<File>,
}

impl InstallLock {
    fn acquire(prefix: &Path) -> Result<Self, UpdateError> {
        let prefix = fs::canonicalize(prefix)?;
        let path = prefix.join(LOCK_NAME);
        #[cfg(unix)]
        {
            match fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if unix_lock_is_active(&path)? {
                        return Err(UpdateError::InstallLock);
                    }
                    match fs::remove_file(path.join("owner")) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                    fs::remove_dir(&path)?;
                    fs::create_dir(&path).map_err(|error| {
                        if error.kind() == io::ErrorKind::AlreadyExists {
                            UpdateError::InstallLock
                        } else {
                            error.into()
                        }
                    })?;
                }
                Err(error) => return Err(error.into()),
            }
            if let Err(error) = fs::write(path.join("owner"), std::process::id().to_string()) {
                let _ = fs::remove_dir(&path);
                return Err(error.into());
            }
            Ok(Self { path })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;

            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .share_mode(0)
                .open(&path)
                .map_err(|error| {
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
                    ) || matches!(error.raw_os_error(), Some(32 | 33))
                    {
                        UpdateError::InstallLock
                    } else {
                        error.into()
                    }
                })?;
            Ok(Self {
                path,
                file: Some(file),
            })
        }
    }
}

#[cfg(unix)]
fn unix_lock_is_active(path: &Path) -> Result<bool, UpdateError> {
    let owner = match fs::read(path.join("owner")) {
        Ok(owner) => owner,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let elapsed = fs::symlink_metadata(path)?
                .modified()?
                .elapsed()
                .unwrap_or_default();
            return Ok(elapsed < std::time::Duration::from_secs(30));
        }
        Err(error) => return Err(error.into()),
    };
    if owner.len() > 32 {
        return Err(UpdateError::Installation);
    }
    let owner = std::str::from_utf8(&owner)
        .map_err(|_| UpdateError::Installation)?
        .trim()
        .parse::<u32>()
        .map_err(|_| UpdateError::Installation)?;
    if owner == 0 {
        return Err(UpdateError::Installation);
    }
    let status = std::process::Command::new("/bin/kill")
        .arg("-0")
        .arg(owner.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    Ok(status.map_or(true, |status| status.success()))
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = fs::remove_file(self.path.join("owner"));
            let _ = fs::remove_dir(&self.path);
        }
        #[cfg(windows)]
        {
            self.file.take();
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(windows)]
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use ed25519_dalek::{Signer, SigningKey};
    use tempfile::tempdir;

    #[cfg(windows)]
    use super::complete_pending_activation;
    use super::{
        ActivationOutcome, InstallReceipt, UpdateState, inspect_installation, install_release,
        rollback_installation, write_json,
    };
    use crate::manifest::RELEASE_TARGETS;
    use crate::{
        ReleaseSignature, TrustStore, UpdateError, canonical_signature, extract_release_archive,
        signing_payload, verify_release,
    };

    fn package(root: &Path, version: &str, target: &str, contents: &[u8]) -> PathBuf {
        let directory = root.join("versions").join(version);
        fs::create_dir_all(&directory).expect("version directory");
        let binary = directory.join(if target.contains("windows") {
            "ash.exe"
        } else {
            "ash"
        });
        fs::write(&binary, contents).expect("binary");
        let digest = crate::sha256_file(&binary).expect("digest").1;
        let metadata = format!(
            "{{\"schema\":1,\"product\":\"ash\",\"version\":\"{version}\",\"target\":\"{target}\",\"protocol\":\"1\",\"ason\":\"1\",\"commit\":\"{}\",\"build\":\"test\",\"binary_sha256\":\"{digest}\"}}\n",
            "a".repeat(40)
        );
        fs::write(directory.join("release.json"), metadata).expect("metadata");
        binary
    }

    fn installation() -> (tempfile::TempDir, String, PathBuf) {
        let temporary = tempdir().expect("temporary directory");
        let prefix = temporary.path().join("ash");
        fs::create_dir_all(prefix.join("versions")).expect("prefix");
        let target = if cfg!(windows) {
            "x86_64-pc-windows-msvc"
        } else if cfg!(target_os = "macos") {
            "x86_64-apple-darwin"
        } else {
            "x86_64-unknown-linux-musl"
        };
        let first = package(&prefix, "1.0.0", target, b"first");
        let second = package(&prefix, "2.0.0", target, b"second");
        #[cfg(unix)]
        let _ = &first;
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("versions/1.0.0", prefix.join("active")).expect("active");
            fs::create_dir(temporary.path().join("bin")).expect("bin");
            symlink(
                prefix.join("active").join("ash"),
                temporary.path().join("bin").join("ash"),
            )
            .expect("launcher");
        }
        #[cfg(windows)]
        {
            fs::create_dir(prefix.join("active")).expect("active");
            fs::copy(&first, prefix.join("active").join("ash.exe")).expect("launcher");
        }
        let launcher = if cfg!(windows) {
            prefix.join("active").join("ash.exe")
        } else {
            temporary.path().join("bin").join("ash")
        };
        let receipt = InstallReceipt {
            schema: 1,
            repository: "A3S-Lab/ash".to_owned(),
            version: "1.0.0".to_owned(),
            target: target.to_owned(),
            prefix: prefix.to_string_lossy().into_owned(),
            launcher: launcher.to_string_lossy().into_owned(),
            path_added: false,
            profile: None,
        };
        write_json(&prefix.join("install-receipt.json"), &receipt).expect("receipt");
        let state = UpdateState {
            schema: 1,
            active: "1.0.0".to_owned(),
            previous: Some("2.0.0".to_owned()),
            highest_sequence: 7,
            highest_manifest_sha256: Some("b".repeat(64)),
        };
        write_json(&prefix.join("update-state.json"), &state).expect("state");
        let _ = second;
        (temporary, target.to_owned(), prefix)
    }

    fn health(path: &Path, version: &str, _target: &str) -> Result<(), UpdateError> {
        let expected = match version {
            "1.0.0" => b"first".as_slice(),
            "2.0.0" => b"second".as_slice(),
            _ => b"third".as_slice(),
        };
        if fs::read(path)? == expected {
            Ok(())
        } else {
            Err(UpdateError::Health)
        }
    }

    #[test]
    fn rollback_is_journaled_activated_and_reversible() {
        let (_temporary, target, prefix) = installation();
        assert_eq!(
            inspect_installation(&prefix, &target)
                .expect("initial installation")
                .current_version(),
            "1.0.0"
        );
        let outcome = rollback_installation(&prefix, &target, health).expect("rollback");
        #[cfg(unix)]
        assert_eq!(
            outcome,
            ActivationOutcome::Activated {
                version: "2.0.0".to_owned()
            }
        );
        #[cfg(windows)]
        {
            assert!(matches!(outcome, ActivationOutcome::HelperRequired { .. }));
            assert_eq!(
                complete_pending_activation(&prefix, &target, health).expect("helper"),
                ActivationOutcome::Activated {
                    version: "2.0.0".to_owned()
                }
            );
        }
        assert_eq!(
            inspect_installation(&prefix, &target)
                .expect("installation")
                .current_version(),
            "2.0.0"
        );

        let outcome = rollback_installation(&prefix, &target, health).expect("reverse rollback");
        #[cfg(unix)]
        assert_eq!(
            outcome,
            ActivationOutcome::Activated {
                version: "1.0.0".to_owned()
            }
        );
        #[cfg(windows)]
        {
            assert!(matches!(outcome, ActivationOutcome::HelperRequired { .. }));
            complete_pending_activation(&prefix, &target, health).expect("helper");
        }
        assert_eq!(
            inspect_installation(&prefix, &target)
                .expect("installation")
                .current_version(),
            "1.0.0"
        );
    }

    #[test]
    fn installation_lock_is_exclusive_and_reusable() {
        let temporary = tempdir().expect("temporary directory");
        let first = super::InstallLock::acquire(temporary.path()).expect("first lock");
        assert!(matches!(
            super::InstallLock::acquire(temporary.path()),
            Err(UpdateError::InstallLock)
        ));
        drop(first);
        super::InstallLock::acquire(temporary.path()).expect("reused lock");
    }

    #[test]
    fn signed_package_is_staged_activated_and_anchored() {
        let (_installation, target, prefix) = installation();
        let release_root = tempdir().expect("release root");
        let package_root = release_root.path().join("source");
        fs::create_dir(&package_root).expect("package root");
        let binary_name = if cfg!(windows) { "ash.exe" } else { "ash" };
        fs::write(package_root.join(binary_name), b"third").expect("binary");
        fs::write(package_root.join("LICENSE"), b"MIT\n").expect("license");
        fs::write(package_root.join("THIRD-PARTY-LICENSES"), b"inventory\n").expect("inventory");
        let binary_digest = crate::sha256_file(&package_root.join(binary_name))
            .expect("binary digest")
            .1;
        fs::write(
            package_root.join("release.json"),
            format!(
                "{{\"schema\":1,\"product\":\"ash\",\"version\":\"3.0.0\",\"target\":\"{target}\",\"protocol\":\"1\",\"ason\":\"1\",\"commit\":\"{}\",\"build\":\"test\",\"binary_sha256\":\"{binary_digest}\"}}\n",
                "a".repeat(40)
            ),
        )
        .expect("release metadata");
        let archive = release_root.path().join(if cfg!(windows) {
            "package.zip"
        } else {
            "package.tar.gz"
        });
        write_archive(&package_root, &archive, binary_name);
        let (archive_size, archive_digest) = crate::sha256_file(&archive).expect("archive digest");
        let artifacts = RELEASE_TARGETS
            .iter()
            .map(|release_target| {
                let extension = if release_target.contains("windows") {
                    "zip"
                } else {
                    "tar.gz"
                };
                let selected = *release_target == target;
                let archive_hash = if selected {
                    archive_digest.clone()
                } else {
                    "c".repeat(64)
                };
                let binary_hash = if selected {
                    binary_digest.clone()
                } else {
                    "d".repeat(64)
                };
                format!(
                    "{{\"target\":\"{release_target}\",\"archive\":\"ash-{release_target}.{extension}\",\"archive_size\":{},\"archive_sha256\":\"{archive_hash}\",\"binary_size\":{},\"binary_sha256\":\"{binary_hash}\"}}",
                    if selected { archive_size } else { 1 },
                    if selected { 5 } else { 1 },
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let manifest = format!(
            "{{\"schema\":1,\"product\":\"ash\",\"channel\":\"stable\",\"sequence\":8,\"version\":\"3.0.0\",\"published_unix\":1800000000,\"source_commit\":\"{}\",\"protocol_major\":1,\"protocol_minor\":0,\"ason_major\":1,\"ason_minor\":0,\"minimum_updater\":\"0.1.0\",\"rollback\":false,\"key_id\":\"test-1\",\"artifacts\":[{artifacts}]}}\n",
            "a".repeat(40)
        )
        .into_bytes();
        let signing = SigningKey::from_bytes(&[9; 32]);
        let signature = canonical_signature(
            &ReleaseSignature::new(
                "test-1",
                signing.sign(&signing_payload(&manifest)).to_bytes(),
            )
            .expect("signature"),
        )
        .expect("signature document");
        let trust = TrustStore::parse(&format!(
            "test-1={}",
            hex(signing.verifying_key().as_bytes())
        ))
        .expect("trust");
        let release = verify_release(
            &manifest,
            &signature,
            &trust,
            "1.0.0",
            "0.1.0",
            (1, 0),
            (1, 0),
            &target,
            7,
            Some(&"b".repeat(64)),
        )
        .expect("verified release");
        let extracted_root = release_root.path().join("extracted");
        let package = extract_release_archive(&archive, &release, &extracted_root)
            .expect("extracted package");
        let outcome = install_release(&prefix, &target, &package, &release, health)
            .expect("installed release");
        #[cfg(unix)]
        assert_eq!(
            outcome,
            ActivationOutcome::Activated {
                version: "3.0.0".to_owned()
            }
        );
        #[cfg(windows)]
        {
            assert!(matches!(outcome, ActivationOutcome::HelperRequired { .. }));
            complete_pending_activation(&prefix, &target, health).expect("helper activation");
        }
        let installed = inspect_installation(&prefix, &target).expect("installation info");
        assert_eq!(installed.current_version(), "3.0.0");
        assert_eq!(installed.highest_sequence(), 8);
        assert_eq!(
            installed.highest_manifest_sha256(),
            Some(release.manifest_sha256())
        );
    }

    #[cfg(windows)]
    fn write_archive(source: &Path, archive: &Path, binary_name: &str) {
        let file = fs::File::create(archive).expect("archive");
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for name in [
            binary_name,
            "LICENSE",
            "THIRD-PARTY-LICENSES",
            "release.json",
        ] {
            archive.start_file(name, options).expect("archive entry");
            archive
                .write_all(&fs::read(source.join(name)).expect("entry"))
                .expect("entry bytes");
        }
        archive.finish().expect("finish archive");
    }

    #[cfg(unix)]
    fn write_archive(source: &Path, archive: &Path, binary_name: &str) {
        let output = fs::File::create(archive).expect("archive");
        let encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for name in [
            binary_name,
            "LICENSE",
            "THIRD-PARTY-LICENSES",
            "release.json",
        ] {
            archive
                .append_path_with_name(source.join(name), name)
                .expect("archive entry");
        }
        archive
            .into_inner()
            .expect("archive encoder")
            .finish()
            .expect("finish archive");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
