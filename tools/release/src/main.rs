#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use ash_update::{
    MAX_ARCHIVE_BYTES, MAX_BINARY_BYTES, RELEASE_TARGETS, ReleaseManifest, ReleaseSignature,
    TrustStore, canonical_manifest, canonical_signature, extract_release_archive, sha256_file,
    signing_payload, verify_release,
};
use ed25519_dalek::{Signer, SigningKey};
use flate2::Compression;
use flate2::GzBuilder;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use tempfile::tempdir;
use zeroize::{Zeroize, Zeroizing};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

const CHECKSUM_DOMAIN: &[u8] = b"ash-release-checksums-v1\0";
const MAX_DESCRIPTOR_BYTES: u64 = 16 * 1024;
const MAX_SBOM_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LICENSE_BYTES: u64 = 1024 * 1024;
const MAX_INVENTORY_BYTES: u64 = 8 * 1024 * 1024;
const PROTOCOL_VERSION: &str = "1";
const ASON_VERSION: &str = "1";
const MINIMUM_UPDATER: &str = "0.1.0";

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactDescriptor {
    schema: u8,
    target: String,
    archive: String,
    archive_size: u64,
    archive_sha256: String,
    binary_size: u64,
    binary_sha256: String,
}

#[derive(Serialize)]
struct PackageMetadata<'a> {
    schema: u8,
    product: &'static str,
    version: &'a str,
    target: &'a str,
    protocol: &'static str,
    ason: &'static str,
    commit: &'a str,
    build: &'a str,
    binary_sha256: &'a str,
}

#[derive(Serialize)]
struct ManifestDraft<'a> {
    schema: u8,
    product: &'static str,
    channel: &'static str,
    sequence: u64,
    version: &'a str,
    published_unix: u64,
    source_commit: &'a str,
    protocol_major: u16,
    protocol_minor: u16,
    ason_major: u16,
    ason_minor: u16,
    minimum_updater: &'a str,
    rollback: bool,
    key_id: &'a str,
    artifacts: Vec<ArtifactDraft<'a>>,
}

#[derive(Serialize)]
struct ArtifactDraft<'a> {
    target: &'a str,
    archive: &'a str,
    archive_size: u64,
    archive_sha256: &'a str,
    binary_size: u64,
    binary_sha256: &'a str,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ChecksumSignature {
    schema: u8,
    key_id: String,
    signature: String,
}

struct PackageInput<'a> {
    target: &'a str,
    binary: &'a [u8],
    output: &'a Path,
    version: &'a str,
    commit: &'a str,
    build: &'a str,
    license: &'a [u8],
    inventory: &'a [u8],
}

struct SignInput<'a> {
    artifacts: &'a Path,
    output: &'a Path,
    version: &'a str,
    commit: &'a str,
    published_unix: u64,
    key_id: &'a str,
    signing_key: &'a Path,
    trusted_keys: &'a str,
    sbom: &'a Path,
}

fn main() -> Result<()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let Some(command) = arguments.next() else {
        return Err(usage());
    };
    let tail = arguments.collect::<Vec<_>>();
    match command.to_str() {
        Some("package") => package_command(&tail),
        Some("sign") => sign_command(&tail),
        Some("verify") => verify_command(&tail),
        _ => Err(usage()),
    }
}

fn package_command(arguments: &[OsString]) -> Result<()> {
    let options = Options::parse(
        arguments,
        &[
            "target",
            "binary",
            "output",
            "version",
            "commit",
            "build",
            "trusted-keys",
            "license",
            "inventory",
        ],
    )?;
    let target = options.text("target")?;
    let binary = options.path("binary")?;
    let output = options.path("output")?;
    let version = options.text("version")?;
    let commit = options.text("commit")?;
    let build = options.text("build")?;
    let trusted_keys = options.text("trusted-keys")?;
    validate_release_values(target, version, commit, build)?;
    let trust = TrustStore::parse(trusted_keys)?;
    probe_binary(binary, version, target, commit, trust.fingerprint())?;
    let binary = read_bounded(binary, MAX_BINARY_BYTES)?;
    let license = read_bounded(options.path("license")?, MAX_LICENSE_BYTES)?;
    let inventory = read_bounded(options.path("inventory")?, MAX_INVENTORY_BYTES)?;
    package(PackageInput {
        target,
        binary: &binary,
        output,
        version,
        commit,
        build,
        license: &license,
        inventory: &inventory,
    })?;
    Ok(())
}

fn sign_command(arguments: &[OsString]) -> Result<()> {
    let options = Options::parse(
        arguments,
        &[
            "artifacts",
            "output",
            "version",
            "commit",
            "published-unix",
            "key-id",
            "signing-key",
            "trusted-keys",
            "sbom",
        ],
    )?;
    let published_unix = options.text("published-unix")?.parse::<u64>()?;
    sign(SignInput {
        artifacts: options.path("artifacts")?,
        output: options.path("output")?,
        version: options.text("version")?,
        commit: options.text("commit")?,
        published_unix,
        key_id: options.text("key-id")?,
        signing_key: options.path("signing-key")?,
        trusted_keys: options.text("trusted-keys")?,
        sbom: options.path("sbom")?,
    })
}

fn verify_command(arguments: &[OsString]) -> Result<()> {
    let options = Options::parse(arguments, &["release", "trusted-keys"])?;
    verify_release_directory(options.path("release")?, options.text("trusted-keys")?)
}

fn package(input: PackageInput<'_>) -> Result<ArtifactDescriptor> {
    validate_release_values(input.target, input.version, input.commit, input.build)?;
    if input.binary.is_empty()
        || input.binary.len() as u64 > MAX_BINARY_BYTES
        || input.license.is_empty()
        || input.license.len() as u64 > MAX_LICENSE_BYTES
        || input.inventory.is_empty()
        || input.inventory.len() as u64 > MAX_INVENTORY_BYTES
    {
        return Err(invalid("package input exceeds its release boundary"));
    }
    prepare_empty_directory(input.output)?;
    let binary_sha256 = sha256(input.binary);
    let metadata = canonical_json(&PackageMetadata {
        schema: 1,
        product: "ash",
        version: input.version,
        target: input.target,
        protocol: PROTOCOL_VERSION,
        ason: ASON_VERSION,
        commit: input.commit,
        build: input.build,
        binary_sha256: &binary_sha256,
    })?;
    let archive_name = archive_name(input.target);
    let archive_path = input.output.join(&archive_name);
    let binary_name = binary_name(input.target);
    let entries = [
        (binary_name, input.binary, 0o755),
        ("LICENSE", input.license, 0o644),
        ("THIRD-PARTY-LICENSES", input.inventory, 0o644),
        ("release.json", metadata.as_slice(), 0o644),
    ];
    if input.target.contains("windows") {
        write_zip(&archive_path, &entries)?;
    } else {
        write_tar_gz(&archive_path, &entries)?;
    }
    let (archive_size, archive_sha256) = sha256_file(&archive_path)?;
    if archive_size == 0 || archive_size > MAX_ARCHIVE_BYTES {
        return Err(invalid("release archive exceeds its hard limit"));
    }
    let descriptor = ArtifactDescriptor {
        schema: 1,
        target: input.target.to_owned(),
        archive: archive_name,
        archive_size,
        archive_sha256,
        binary_size: input.binary.len() as u64,
        binary_sha256,
    };
    write_new(
        &input
            .output
            .join(format!("ash-{}.artifact.json", input.target)),
        &canonical_json(&descriptor)?,
    )?;
    Ok(descriptor)
}

fn sign(input: SignInput<'_>) -> Result<()> {
    validate_release_values(RELEASE_TARGETS[0], input.version, input.commit, "release")?;
    if input.published_unix == 0 {
        return Err(invalid("published-unix must be positive"));
    }
    let trust = TrustStore::parse(input.trusted_keys)?;
    let sbom = read_bounded(input.sbom, MAX_SBOM_BYTES)?;
    validate_sbom(&sbom)?;
    let descriptors = load_descriptors(input.artifacts)?;
    prepare_empty_directory(input.output)?;
    for descriptor in &descriptors {
        let source = descriptor_path(input.artifacts, &descriptor.target)?
            .parent()
            .ok_or_else(|| invalid("descriptor has no parent"))?
            .join(&descriptor.archive);
        verify_identity(&source, descriptor.archive_size, &descriptor.archive_sha256)?;
        copy_new(&source, &input.output.join(&descriptor.archive))?;
    }
    copy_bytes_new(&input.output.join("sbom.spdx.json"), &sbom)?;

    let artifacts = descriptors
        .iter()
        .map(|descriptor| ArtifactDraft {
            target: &descriptor.target,
            archive: &descriptor.archive,
            archive_size: descriptor.archive_size,
            archive_sha256: &descriptor.archive_sha256,
            binary_size: descriptor.binary_size,
            binary_sha256: &descriptor.binary_sha256,
        })
        .collect();
    let draft = ManifestDraft {
        schema: 1,
        product: "ash",
        channel: "stable",
        sequence: release_sequence(input.version)?,
        version: input.version,
        published_unix: input.published_unix,
        source_commit: input.commit,
        protocol_major: 1,
        protocol_minor: 0,
        ason_major: 1,
        ason_minor: 0,
        minimum_updater: MINIMUM_UPDATER,
        rollback: false,
        key_id: input.key_id,
        artifacts,
    };
    let draft = serde_json::to_vec(&draft)?;
    let manifest: ReleaseManifest = serde_json::from_slice(&draft)?;
    let manifest = canonical_manifest(&manifest)?;
    let signing_key = read_signing_key(input.signing_key)?;
    let signature = signing_key.sign(&signing_payload(&manifest)).to_bytes();
    let signature = canonical_signature(&ReleaseSignature::new(input.key_id, signature)?)?;
    write_new(&input.output.join("release-manifest.json"), &manifest)?;
    write_new(&input.output.join("release-manifest.sig"), &signature)?;

    let checksums = release_checksums(input.output, &descriptors)?;
    write_new(&input.output.join("SHA256SUMS"), &checksums)?;
    let checksum_signature = signing_key.sign(&checksum_payload(&checksums)).to_bytes();
    let checksum_signature = canonical_json(&ChecksumSignature {
        schema: 1,
        key_id: input.key_id.to_owned(),
        signature: encode_hex(&checksum_signature),
    })?;
    write_new(&input.output.join("SHA256SUMS.sig"), &checksum_signature)?;
    drop(signing_key);
    verify_release_directory_with(input.output, &trust)
}

fn verify_release_directory(directory: &Path, trusted_keys: &str) -> Result<()> {
    let trust = TrustStore::parse(trusted_keys)?;
    verify_release_directory_with(directory, &trust)
}

fn verify_release_directory_with(directory: &Path, trust: &TrustStore) -> Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("release path is not a real directory"));
    }
    let manifest = read_bounded(&directory.join("release-manifest.json"), 64 * 1024)?;
    let signature = read_bounded(&directory.join("release-manifest.sig"), 1024)?;
    let parsed: ReleaseManifest = serde_json::from_slice(&manifest)?;
    let descriptors = parsed
        .artifacts()
        .iter()
        .map(|artifact| ArtifactDescriptor {
            schema: 1,
            target: artifact.target().to_owned(),
            archive: artifact.archive().to_owned(),
            archive_size: artifact.archive_size(),
            archive_sha256: artifact.archive_sha256().to_owned(),
            binary_size: artifact.binary_size(),
            binary_sha256: artifact.binary_sha256().to_owned(),
        })
        .collect::<Vec<_>>();
    if descriptors.len() != RELEASE_TARGETS.len() {
        return Err(invalid("release target matrix is incomplete"));
    }
    let extraction = tempdir()?;
    for (index, target) in RELEASE_TARGETS.into_iter().enumerate() {
        let release = verify_release(
            &manifest,
            &signature,
            trust,
            parsed.version(),
            parsed.version(),
            (1, 0),
            (1, 0),
            target,
            0,
            None,
        )?;
        extract_release_archive(
            &directory.join(release.artifact().archive()),
            &release,
            &extraction.path().join(index.to_string()),
        )?;
    }
    let sbom = read_bounded(&directory.join("sbom.spdx.json"), MAX_SBOM_BYTES)?;
    validate_sbom(&sbom)?;
    let expected_checksums = release_checksums(directory, &descriptors)?;
    let checksums = read_bounded(&directory.join("SHA256SUMS"), 64 * 1024)?;
    if checksums != expected_checksums {
        return Err(invalid("release checksums are stale or noncanonical"));
    }
    let encoded_signature = read_bounded(&directory.join("SHA256SUMS.sig"), 1024)?;
    let checksum_signature: ChecksumSignature = serde_json::from_slice(&encoded_signature)?;
    if canonical_json(&checksum_signature)? != encoded_signature
        || checksum_signature.schema != 1
        || !canonical_key_id(&checksum_signature.key_id)
        || checksum_signature.key_id != parsed.key_id()
    {
        return Err(invalid("checksum signature is noncanonical"));
    }
    trust.verify_detached(
        &checksum_signature.key_id,
        &checksum_payload(&checksums),
        decode_hex::<64>(&checksum_signature.signature)?,
    )?;
    validate_release_files(directory, &descriptors)?;
    Ok(())
}

fn validate_release_files(directory: &Path, descriptors: &[ArtifactDescriptor]) -> Result<()> {
    let mut expected = descriptors
        .iter()
        .map(|descriptor| descriptor.archive.clone())
        .collect::<HashSet<_>>();
    for name in [
        "release-manifest.json",
        "release-manifest.sig",
        "SHA256SUMS",
        "SHA256SUMS.sig",
        "sbom.spdx.json",
    ] {
        expected.insert(name.to_owned());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("release filename is not UTF-8"))?;
        if !metadata.is_file()
            || metadata.is_symlink()
            || (name != "provenance.sigstore.json" && !expected.remove(&name))
        {
            return Err(invalid("release directory contains an unexpected entry"));
        }
    }
    if !expected.is_empty() {
        return Err(invalid("release directory is missing an expected entry"));
    }
    Ok(())
}

fn load_descriptors(root: &Path) -> Result<Vec<ArtifactDescriptor>> {
    let mut paths = Vec::new();
    collect_descriptors(root, &mut paths, 0)?;
    if paths.len() != RELEASE_TARGETS.len() {
        return Err(invalid("exactly six artifact descriptors are required"));
    }
    let mut by_target = BTreeMap::new();
    for path in paths {
        let encoded = read_bounded(&path, MAX_DESCRIPTOR_BYTES)?;
        let descriptor: ArtifactDescriptor = serde_json::from_slice(&encoded)?;
        let expected_name = format!("ash-{}.artifact.json", descriptor.target);
        if canonical_json(&descriptor)? != encoded
            || descriptor.schema != 1
            || path.file_name() != Some(OsStr::new(&expected_name))
            || descriptor.archive != archive_name(&descriptor.target)
            || descriptor.archive_size == 0
            || descriptor.archive_size > MAX_ARCHIVE_BYTES
            || descriptor.binary_size == 0
            || descriptor.binary_size > MAX_BINARY_BYTES
            || !canonical_hex(&descriptor.archive_sha256, 64)
            || !canonical_hex(&descriptor.binary_sha256, 64)
            || !RELEASE_TARGETS.contains(&descriptor.target.as_str())
            || by_target
                .insert(descriptor.target.clone(), (descriptor, path))
                .is_some()
        {
            return Err(invalid("artifact descriptor is invalid"));
        }
    }
    RELEASE_TARGETS
        .iter()
        .map(|target| {
            by_target
                .remove(*target)
                .map(|(descriptor, _)| descriptor)
                .ok_or_else(|| invalid("release target matrix is incomplete"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()
}

fn descriptor_path(root: &Path, target: &str) -> Result<PathBuf> {
    let expected = format!("ash-{target}.artifact.json");
    let mut paths = Vec::new();
    collect_descriptors(root, &mut paths, 0)?;
    paths
        .into_iter()
        .find(|path| path.file_name() == Some(OsStr::new(&expected)))
        .ok_or_else(|| invalid("artifact descriptor was not found"))
}

fn collect_descriptors(root: &Path, output: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if depth > 2 || output.len() > RELEASE_TARGETS.len() {
        return Err(invalid("artifact directory exceeds its traversal limit"));
    }
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid("artifact root must be a real directory"));
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(invalid("artifact input may not contain symlinks"));
        }
        let path = entry.path();
        if kind.is_dir() {
            collect_descriptors(&path, output, depth + 1)?;
        } else if kind.is_file()
            && path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.ends_with(".artifact.json"))
        {
            output.push(path);
        }
    }
    Ok(())
}

fn release_checksums(root: &Path, descriptors: &[ArtifactDescriptor]) -> Result<Vec<u8>> {
    let mut names = descriptors
        .iter()
        .map(|descriptor| descriptor.archive.clone())
        .collect::<Vec<_>>();
    names.extend([
        "release-manifest.json".to_owned(),
        "release-manifest.sig".to_owned(),
        "sbom.spdx.json".to_owned(),
    ]);
    names.sort();
    let mut encoded = Vec::new();
    for name in names {
        if name.contains(['/', '\\']) {
            return Err(invalid("checksum filename is not flat"));
        }
        let (_, digest) = sha256_file(&root.join(&name))?;
        writeln!(encoded, "{digest}  {name}")?;
    }
    Ok(encoded)
}

fn validate_sbom(encoded: &[u8]) -> Result<()> {
    let value: serde_json::Value = serde_json::from_slice(encoded)?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid("SBOM must be a JSON object"))?;
    if object.get("spdxVersion").and_then(|value| value.as_str()) != Some("SPDX-2.3")
        || object.get("dataLicense").and_then(|value| value.as_str()) != Some("CC0-1.0")
        || object
            .get("documentNamespace")
            .and_then(|value| value.as_str())
            .is_none_or(str::is_empty)
        || object
            .get("packages")
            .and_then(|value| value.as_array())
            .is_none_or(Vec::is_empty)
    {
        return Err(invalid(
            "SBOM does not satisfy the SPDX 2.3 release contract",
        ));
    }
    Ok(())
}

fn probe_binary(
    binary: &Path,
    version: &str,
    target: &str,
    commit: &str,
    trust_fingerprint: &str,
) -> Result<()> {
    let metadata = fs::symlink_metadata(binary)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_BINARY_BYTES
    {
        return Err(invalid("release binary is not a bounded regular file"));
    }
    let output = Command::new(binary).arg("--build-info").output()?;
    let expected =
        format!("v:{version}\nt:{target}\np:1\na:1\nk:{trust_fingerprint}\nc:{commit}\n");
    if !output.status.success() || !output.stderr.is_empty() || output.stdout != expected.as_bytes()
    {
        return Err(invalid(
            "release binary build identity does not match packaging inputs",
        ));
    }
    Ok(())
}

fn validate_release_values(target: &str, version: &str, commit: &str, build: &str) -> Result<()> {
    if !RELEASE_TARGETS.contains(&target)
        || !is_stable_version(version)
        || !canonical_hex(commit, 40)
        || build.is_empty()
        || build.len() > 64
        || !build
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(invalid("release identity is invalid"));
    }
    Ok(())
}

fn is_stable_version(value: &str) -> bool {
    Version::parse(value).is_ok_and(|version| version.pre.is_empty() && version.build.is_empty())
}

fn release_sequence(value: &str) -> Result<u64> {
    let version = Version::parse(value)?;
    if !version.pre.is_empty()
        || !version.build.is_empty()
        || version.major > 999_999
        || version.minor > 999_999
        || version.patch > 999_999
    {
        return Err(invalid(
            "version cannot map to a monotonic release sequence",
        ));
    }
    let sequence = version
        .major
        .checked_mul(1_000_000_000_000)
        .and_then(|value| value.checked_add(version.minor * 1_000_000))
        .and_then(|value| value.checked_add(version.patch))
        .ok_or_else(|| invalid("release sequence overflow"))?;
    if sequence == 0 {
        return Err(invalid("release sequence must be positive"));
    }
    Ok(sequence)
}

fn read_signing_key(path: &Path) -> Result<SigningKey> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 130 {
        return Err(invalid("signing key must be a bounded regular file"));
    }
    let mut encoded = Zeroizing::new(fs::read(path)?);
    while encoded
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        encoded.pop();
    }
    if encoded.len() != 64 {
        return Err(invalid("signing key must be a 32-byte lowercase hex seed"));
    }
    let mut seed = decode_hex_bytes::<32>(&encoded)?;
    let key = SigningKey::from_bytes(&seed);
    seed.zeroize();
    Ok(key)
}

fn write_tar_gz(path: &Path, entries: &[(&str, &[u8], u32)]) -> Result<()> {
    let file = create_new(path)?;
    let encoder = GzBuilder::new().mtime(0).write(file, Compression::best());
    let mut archive = Builder::new(encoder);
    for (name, bytes, mode) in entries {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(*mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        archive.append_data(&mut header, name, *bytes)?;
    }
    archive.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

fn write_zip(path: &Path, entries: &[(&str, &[u8], u32)]) -> Result<()> {
    let file = create_new(path)?;
    let mut archive = ZipWriter::new(file);
    for (name, bytes, mode) in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(DateTime::default())
            .unix_permissions(*mode);
        archive.start_file(*name, options)?;
        archive.write_all(bytes)?;
    }
    archive.finish()?.sync_all()?;
    Ok(())
}

fn prepare_empty_directory(path: &Path) -> Result<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || fs::read_dir(path)?.next().is_some()
        {
            return Err(invalid(
                "output directory must be empty and may not be a symlink",
            ));
        }
    } else {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(invalid("input is not a bounded regular file"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("input exceeds its hard limit"));
    }
    Ok(bytes)
}

fn verify_identity(path: &Path, size: u64, digest: &str) -> Result<()> {
    let (actual_size, actual_digest) = sha256_file(path)?;
    if actual_size != size || actual_digest != digest {
        return Err(invalid("artifact identity does not match its descriptor"));
    }
    Ok(())
}

fn copy_new(source: &Path, destination: &Path) -> Result<()> {
    let bytes = read_bounded(source, MAX_ARCHIVE_BYTES)?;
    copy_bytes_new(destination, &bytes)
}

fn copy_bytes_new(destination: &Path, bytes: &[u8]) -> Result<()> {
    write_new(destination, bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = create_new(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn create_new(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().create_new(true).write(true).open(path)?)
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn checksum_payload(checksums: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(CHECKSUM_DOMAIN.len() + checksums.len());
    payload.extend_from_slice(CHECKSUM_DOMAIN);
    payload.extend_from_slice(checksums);
    payload
}

fn sha256(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn archive_name(target: &str) -> String {
    format!(
        "ash-{target}.{}",
        if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        }
    )
}

fn binary_name(target: &str) -> &'static str {
    if target.contains("windows") {
        "ash.exe"
    } else {
        "ash"
    }
}

fn canonical_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn canonical_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N]> {
    decode_hex_bytes(value.as_bytes())
}

fn decode_hex_bytes<const N: usize>(value: &[u8]) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        return Err(invalid("hex value has the wrong length"));
    }
    let mut bytes = [0_u8; N];
    for (index, pair) in value.chunks_exact(2).enumerate() {
        bytes[index] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(bytes)
}

fn nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(invalid("hex value is not canonical lowercase")),
    }
}

fn invalid(message: &'static str) -> Box<dyn Error> {
    std::io::Error::other(message).into()
}

fn usage() -> Box<dyn Error> {
    invalid("usage: a3s-ash-release package|sign|verify with the documented exact options")
}

struct Options {
    values: BTreeMap<String, OsString>,
}

impl Options {
    fn parse(arguments: &[OsString], names: &[&str]) -> Result<Self> {
        if !arguments.len().is_multiple_of(2) {
            return Err(usage());
        }
        let allowed = names.iter().copied().collect::<HashSet<_>>();
        let mut values = BTreeMap::new();
        for pair in arguments.chunks_exact(2) {
            let name = pair[0]
                .to_str()
                .and_then(|value| value.strip_prefix("--"))
                .filter(|value| allowed.contains(value))
                .ok_or_else(usage)?;
            if values.insert(name.to_owned(), pair[1].clone()).is_some() {
                return Err(invalid("duplicate release option"));
            }
        }
        if values.len() != names.len() {
            return Err(usage());
        }
        Ok(Self { values })
    }

    fn value(&self, name: &str) -> Result<&OsStr> {
        self.values
            .get(name)
            .map(OsString::as_os_str)
            .ok_or_else(usage)
    }

    fn text(&self, name: &str) -> Result<&str> {
        self.value(name)?
            .to_str()
            .ok_or_else(|| invalid("release text option is not UTF-8"))
    }

    fn path(&self, name: &str) -> Result<&Path> {
        Ok(Path::new(self.value(name)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sbom() -> Vec<u8> {
        br#"{"spdxVersion":"SPDX-2.3","dataLicense":"CC0-1.0","documentNamespace":"https://github.com/A3S-Lab/ash/test","packages":[{"name":"ash"}]}"#
            .iter()
            .copied()
            .chain(std::iter::once(b'\n'))
            .collect()
    }

    #[test]
    fn semantic_versions_map_to_monotonic_nonzero_sequences() {
        assert_eq!(release_sequence("0.0.1").expect("sequence"), 1);
        assert_eq!(release_sequence("0.1.0").expect("sequence"), 1_000_000);
        assert_eq!(
            release_sequence("1.0.0").expect("sequence"),
            1_000_000_000_000
        );
        assert!(release_sequence("0.0.0").is_err());
        assert!(release_sequence("1.0.0-alpha.1").is_err());
    }

    #[test]
    fn six_target_release_is_deterministic_signed_and_cross_format_verified() {
        let temporary = tempdir().expect("temporary directory");
        let artifacts = temporary.path().join("artifacts");
        fs::create_dir(&artifacts).expect("artifact root");
        let version = "0.2.3";
        let commit = "a".repeat(40);
        for target in RELEASE_TARGETS {
            let output = artifacts.join(target);
            package(PackageInput {
                target,
                binary: format!("signed-binary-{target}").as_bytes(),
                output: &output,
                version,
                commit: &commit,
                build: "test-build",
                license: b"MIT\n",
                inventory: b"inventory\n",
            })
            .expect("package");
        }
        let sbom_path = temporary.path().join("sbom.spdx.json");
        fs::write(&sbom_path, sbom()).expect("SBOM");
        let key_path = temporary.path().join("signing-key");
        let seed = [9_u8; 32];
        fs::write(&key_path, encode_hex(&seed)).expect("key");
        let signing = SigningKey::from_bytes(&seed);
        let trusted = format!(
            "release-1={}",
            encode_hex(signing.verifying_key().as_bytes())
        );
        let release = temporary.path().join("release");
        sign(SignInput {
            artifacts: &artifacts,
            output: &release,
            version,
            commit: &commit,
            published_unix: 1_800_000_000,
            key_id: "release-1",
            signing_key: &key_path,
            trusted_keys: &trusted,
            sbom: &sbom_path,
        })
        .expect("sign release");
        verify_release_directory(&release, &trusted).expect("verify release");

        for (index, target) in [RELEASE_TARGETS[0], RELEASE_TARGETS[1]]
            .into_iter()
            .enumerate()
        {
            let first = artifacts.join(target);
            let second = temporary.path().join(format!("repeat-{index}"));
            package(PackageInput {
                target,
                binary: format!("signed-binary-{target}").as_bytes(),
                output: &second,
                version,
                commit: &commit,
                build: "test-build",
                license: b"MIT\n",
                inventory: b"inventory\n",
            })
            .expect("repeat package");
            assert_eq!(
                fs::read(first.join(archive_name(target))).expect("first archive"),
                fs::read(second.join(archive_name(target))).expect("second archive")
            );
        }

        let archive = release.join(archive_name(RELEASE_TARGETS[0]));
        let mut altered = fs::read(&archive).expect("archive");
        altered[0] ^= 1;
        fs::write(archive, altered).expect("alter archive");
        assert!(verify_release_directory(&release, &trusted).is_err());
    }
}
