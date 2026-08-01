use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ash_protocol::ason::{Atom, Document, Field, Key, Value};
use ash_update::{
    ActivationOutcome, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES, RecoveryOutcome,
    UpdateDecision, UpdateError, complete_pending_activation, confirm_current_release,
    embedded_trust_store, extract_release_archive, inspect_installation, install_release,
    recover_installation, rollback_installation, verify_release,
};
use futures::StreamExt;
use reqwest::Client;
use reqwest::redirect::Policy;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cli_error::CliError;

const RELEASE_BASE: &str = "https://github.com/A3S-Lab/ash/releases/latest/download";
const MANIFEST_NAME: &str = "release-manifest.json";
const SIGNATURE_NAME: &str = "release-manifest.sig";
const PROTOCOL_VERSION: (u16, u16) = (1, 0);
const ASON_VERSION: (u16, u16) = (1, 0);
const BUILD_INFO_LIMIT: usize = 1024;
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct CandidateInfo {
    version: String,
    target: String,
}

#[derive(Clone, Copy)]
enum MetadataKind {
    Manifest,
    Signature,
}

pub async fn run(arguments: &[OsString]) -> Result<(), CliError> {
    let Some((command, tail)) = arguments.split_first() else {
        return Err(CliError::Usage);
    };
    if command == "status" {
        status(parse_prefix(tail)?).await
    } else if command == "update" {
        let (prefix, source) = parse_update(tail)?;
        update(prefix, source.as_deref()).await
    } else if command == "rollback" {
        rollback(parse_prefix(tail)?).await
    } else if command == "recover" {
        recover(parse_prefix(tail)?).await
    } else if command == "check" {
        check(tail).await
    } else if command == "replace" {
        replace(parse_prefix(tail)?).await
    } else {
        Err(CliError::Usage)
    }
}

pub(crate) fn trust_fingerprint() -> String {
    embedded_trust_store()
        .map(|trust| trust.fingerprint().to_owned())
        .unwrap_or_else(|_| "~".to_owned())
}

async fn status(prefix: PathBuf) -> Result<(), CliError> {
    let installation = inspect_installation(&prefix, crate::build_target())?;
    emit(vec![
        ("s", Atom::text("0")),
        ("a", Atom::text("status")),
        ("v", Atom::text(installation.current_version())),
        ("t", Atom::text(installation.target())),
        ("q", Atom::text(installation.highest_sequence().to_string())),
        (
            "h",
            installation
                .highest_manifest_sha256()
                .map_or(Atom::Null, Atom::text),
        ),
        ("k", fingerprint_atom()),
    ])
    .await
}

async fn update(prefix: PathBuf, source: Option<&Path>) -> Result<(), CliError> {
    recover_installation(&prefix, crate::build_target(), candidate_health)?;
    let installation = inspect_installation(&prefix, crate::build_target())?;
    let trust = embedded_trust_store()?;
    let (manifest, signature, client) = if let Some(directory) = source {
        (
            read_metadata(&directory.join(MANIFEST_NAME), MetadataKind::Manifest).await?,
            read_metadata(&directory.join(SIGNATURE_NAME), MetadataKind::Signature).await?,
            None,
        )
    } else {
        let client = release_client()?;
        let manifest = download_metadata(&client, MANIFEST_NAME, MetadataKind::Manifest).await?;
        let signature = download_metadata(&client, SIGNATURE_NAME, MetadataKind::Signature).await?;
        (manifest, signature, Some(client))
    };
    let release = verify_release(
        &manifest,
        &signature,
        &trust,
        installation.current_version(),
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION,
        ASON_VERSION,
        crate::build_target(),
        installation.highest_sequence(),
        installation.highest_manifest_sha256(),
    )?;
    let version = release.manifest().version().to_owned();
    if release.decision() == UpdateDecision::Current {
        confirm_current_release(&prefix, crate::build_target(), &release)?;
        return emit(vec![
            ("s", Atom::text("0")),
            ("a", Atom::text("current")),
            ("v", Atom::text(version)),
        ])
        .await;
    }

    let stage = tempdir().map_err(UpdateError::Io)?;
    let archive = if let Some(directory) = source {
        directory.join(release.artifact().archive())
    } else {
        let destination = stage.path().join(release.artifact().archive());
        download_archive(
            client.as_ref().ok_or(UpdateError::Archive)?,
            release.artifact().archive(),
            release.artifact().archive_size(),
            &destination,
        )
        .await?;
        destination
    };
    let package_root = stage.path().join("package");
    let package = extract_release_archive(&archive, &release, &package_root)?;
    match install_release(
        &prefix,
        crate::build_target(),
        &package,
        &release,
        candidate_health,
    )? {
        ActivationOutcome::Current { version } => emit_success("current", version).await,
        ActivationOutcome::Activated { version } => emit_success("updated", version).await,
        ActivationOutcome::HelperRequired { candidate, prefix } => {
            spawn_replacement_helper(&candidate, &prefix)?;
            emit_success("scheduled", version).await
        }
    }
}

async fn rollback(prefix: PathBuf) -> Result<(), CliError> {
    recover_installation(&prefix, crate::build_target(), candidate_health)?;
    match rollback_installation(&prefix, crate::build_target(), candidate_health)? {
        ActivationOutcome::Current { version } => emit_success("current", version).await,
        ActivationOutcome::Activated { version } => emit_success("rolled-back", version).await,
        ActivationOutcome::HelperRequired { candidate, prefix } => {
            let version = probe_candidate(&candidate)?.version;
            spawn_replacement_helper(&candidate, &prefix)?;
            emit_success("scheduled-rollback", version).await
        }
    }
}

async fn recover(prefix: PathBuf) -> Result<(), CliError> {
    let outcome = recover_installation(&prefix, crate::build_target(), candidate_health)?;
    let (action, version) = match outcome {
        RecoveryOutcome::Clean => ("clean", None),
        RecoveryOutcome::Finalized { version } => ("finalized", Some(version)),
        RecoveryOutcome::RolledBack { version } => ("recovered-rollback", Some(version)),
    };
    emit(vec![
        ("s", Atom::text("0")),
        ("a", Atom::text(action)),
        ("v", version.map_or(Atom::Null, Atom::text)),
    ])
    .await
}

async fn check(arguments: &[OsString]) -> Result<(), CliError> {
    let [option, candidate] = arguments else {
        return Err(CliError::Usage);
    };
    if option != "--candidate" {
        return Err(CliError::Usage);
    }
    let info = probe_candidate(Path::new(candidate))?;
    if info.target != crate::build_target() {
        return Err(UpdateError::Target.into());
    }
    emit(vec![
        ("s", Atom::text("0")),
        ("a", Atom::text("healthy")),
        ("v", Atom::text(info.version)),
        ("t", Atom::text(info.target)),
    ])
    .await
}

async fn replace(prefix: PathBuf) -> Result<(), CliError> {
    let deadline = Instant::now() + HELPER_TIMEOUT;
    loop {
        match complete_pending_activation(&prefix, crate::build_target(), candidate_health) {
            Ok(ActivationOutcome::Activated { version }) => {
                return emit_success("replaced", version).await;
            }
            Ok(_) => return Err(UpdateError::Activation.into()),
            Err(UpdateError::InstallLock | UpdateError::Io(_)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn parse_prefix(arguments: &[OsString]) -> Result<PathBuf, CliError> {
    match arguments {
        [] => default_prefix(),
        [option, value] if option == "--prefix" => Ok(PathBuf::from(value)),
        _ => Err(CliError::Usage),
    }
}

fn parse_update(arguments: &[OsString]) -> Result<(PathBuf, Option<PathBuf>), CliError> {
    let mut prefix = None;
    let mut source = None;
    let mut index = 0;
    while index < arguments.len() {
        let option = &arguments[index];
        let value = arguments.get(index + 1).ok_or(CliError::Usage)?;
        if option == "--prefix" && prefix.is_none() {
            prefix = Some(PathBuf::from(value));
        } else if option == "--from" && source.is_none() {
            source = Some(PathBuf::from(value));
        } else {
            return Err(CliError::Usage);
        }
        index += 2;
    }
    Ok((prefix.map_or_else(default_prefix, Ok)?, source))
}

fn default_prefix() -> Result<PathBuf, CliError> {
    if let Some(prefix) = std::env::var_os("ASH_INSTALL_PREFIX") {
        return Ok(PathBuf::from(prefix));
    }
    if let Some(prefix) = std::env::var_os("ASH_HOME") {
        return Ok(PathBuf::from(prefix));
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA").ok_or(UpdateError::Installation)?;
        Ok(PathBuf::from(local).join("Programs").join("ash"))
    }
    #[cfg(unix)]
    {
        let home = std::env::var_os("HOME").ok_or(UpdateError::Installation)?;
        Ok(PathBuf::from(home).join(".local").join("share").join("ash"))
    }
}

fn release_client() -> Result<Client, CliError> {
    Ok(Client::builder()
        .https_only(true)
        .redirect(Policy::limited(5))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("ash/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

async fn read_metadata(path: &Path, kind: MetadataKind) -> Result<Vec<u8>, CliError> {
    let maximum = metadata_maximum(kind);
    let file = tokio::fs::File::open(path).await.map_err(UpdateError::Io)?;
    let mut bytes = Vec::new();
    file.take((maximum as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(UpdateError::Io)?;
    if bytes.len() > maximum {
        return Err(metadata_size_error(kind).into());
    }
    Ok(bytes)
}

async fn download_metadata(
    client: &Client,
    name: &str,
    kind: MetadataKind,
) -> Result<Vec<u8>, CliError> {
    let maximum = metadata_maximum(kind);
    let response = client
        .get(format!("{RELEASE_BASE}/{name}"))
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(metadata_size_error(kind).into());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(metadata_size_error(kind).into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn download_archive(
    client: &Client,
    name: &str,
    expected_size: u64,
    destination: &Path,
) -> Result<(), CliError> {
    if expected_size == 0 || expected_size > MAX_ARCHIVE_BYTES {
        return Err(UpdateError::Archive.into());
    }
    let response = client
        .get(format!("{RELEASE_BASE}/{name}"))
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length != expected_size)
    {
        return Err(UpdateError::Archive.into());
    }
    let mut output = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .await
        .map_err(UpdateError::Io)?;
    let mut received = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        received = received
            .checked_add(u64::try_from(chunk.len()).map_err(|_| UpdateError::Archive)?)
            .ok_or(UpdateError::Archive)?;
        if received > expected_size {
            return Err(UpdateError::Archive.into());
        }
        output.write_all(&chunk).await.map_err(UpdateError::Io)?;
    }
    if received != expected_size {
        return Err(UpdateError::Archive.into());
    }
    output.flush().await.map_err(UpdateError::Io)?;
    output.sync_all().await.map_err(UpdateError::Io)?;
    Ok(())
}

fn metadata_maximum(kind: MetadataKind) -> usize {
    match kind {
        MetadataKind::Manifest => MAX_MANIFEST_BYTES,
        MetadataKind::Signature => MAX_SIGNATURE_BYTES,
    }
}

fn metadata_size_error(kind: MetadataKind) -> UpdateError {
    match kind {
        MetadataKind::Manifest => UpdateError::ManifestSize,
        MetadataKind::Signature => UpdateError::SignatureSize,
    }
}

fn candidate_health(path: &Path, version: &str, target: &str) -> Result<(), UpdateError> {
    let info = probe_candidate(path)?;
    if info.version != version || info.target != target {
        return Err(UpdateError::Health);
    }
    Ok(())
}

fn probe_candidate(path: &Path) -> Result<CandidateInfo, UpdateError> {
    let output = Command::new(path)
        .arg("--build-info")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() || output.stdout.len() > BUILD_INFO_LIMIT {
        return Err(UpdateError::Health);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| UpdateError::Health)?;
    let mut lines = text.lines();
    let version = build_value(lines.next(), "v:")?;
    let target = build_value(lines.next(), "t:")?;
    if build_value(lines.next(), "p:")? != "1"
        || build_value(lines.next(), "a:")? != "1"
        || build_value(lines.next(), "k:")? != trust_fingerprint()
        || !valid_build_commit(build_value(lines.next(), "c:")?)
        || lines.next().is_some()
        || !text.ends_with('\n')
    {
        return Err(UpdateError::Health);
    }
    Ok(CandidateInfo {
        version: version.to_owned(),
        target: target.to_owned(),
    })
}

fn valid_build_commit(value: &str) -> bool {
    value == "~"
        || (value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
}

fn build_value<'a>(line: Option<&'a str>, prefix: &str) -> Result<&'a str, UpdateError> {
    line.and_then(|value| value.strip_prefix(prefix))
        .filter(|value| !value.is_empty())
        .ok_or(UpdateError::Health)
}

fn spawn_replacement_helper(candidate: &Path, prefix: &Path) -> Result<(), UpdateError> {
    let mut command = Command::new(candidate);
    command
        .arg("self")
        .arg("replace")
        .arg("--prefix")
        .arg(prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()?;
    Ok(())
}

fn fingerprint_atom() -> Atom {
    let fingerprint = trust_fingerprint();
    if fingerprint == "~" {
        Atom::Null
    } else {
        Atom::text(fingerprint)
    }
}

async fn emit_success(action: &str, version: String) -> Result<(), CliError> {
    emit(vec![
        ("s", Atom::text("0")),
        ("a", Atom::text(action)),
        ("v", Atom::text(version)),
    ])
    .await
}

async fn emit(fields: Vec<(&str, Atom)>) -> Result<(), CliError> {
    let document = Document::new(
        fields
            .into_iter()
            .map(|(key, value)| Ok(Field::new(Key::new(key)?, Value::Scalar(value))))
            .collect::<Result<Vec<_>, ash_protocol::ason::BuildError>>()?,
    )?;
    let mut output = tokio::io::stdout();
    output.write_all(document.encode().as_bytes()).await?;
    output.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{default_prefix, parse_prefix, parse_update};

    #[test]
    fn options_are_pairwise_and_duplicate_safe() {
        assert!(parse_prefix(&[]).is_ok());
        assert_eq!(
            parse_prefix(&[OsString::from("--prefix"), OsString::from("root")]).expect("prefix"),
            PathBuf::from("root")
        );
        assert!(parse_prefix(&[OsString::from("--prefix")]).is_err());
        assert!(
            parse_update(&[
                OsString::from("--from"),
                OsString::from("releases"),
                OsString::from("--prefix"),
                OsString::from("root"),
            ])
            .is_ok()
        );
        assert!(
            parse_update(&[
                OsString::from("--from"),
                OsString::from("one"),
                OsString::from("--from"),
                OsString::from("two"),
            ])
            .is_err()
        );
        assert!(default_prefix().is_ok());
    }
}
