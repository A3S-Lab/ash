use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{Artifact, MAX_BINARY_BYTES, ReleaseManifest, UpdateError, VerifiedRelease};

const MAX_LICENSE_BYTES: u64 = 1024 * 1024;
const MAX_LICENSE_INVENTORY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RELEASE_METADATA_BYTES: u64 = 64 * 1024;
const COPY_BUFFER_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug)]
pub struct ExtractedPackage {
    root: PathBuf,
    binary: PathBuf,
}

impl ExtractedPackage {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

pub fn extract_release_archive(
    archive: &Path,
    release: &VerifiedRelease,
    destination: &Path,
) -> Result<ExtractedPackage, UpdateError> {
    verify_file(
        archive,
        release.artifact().archive_size(),
        release.artifact().archive_sha256(),
    )?;
    prepare_empty_destination(destination)?;
    extract_platform_archive(archive, destination, release.artifact())?;
    validate_package(destination, release.manifest(), release.artifact())
}

pub fn sha256_file(path: &Path) -> Result<(u64, String), UpdateError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).map_err(|_| UpdateError::Archive)?)
            .ok_or(UpdateError::Archive)?;
        hasher.update(&buffer[..read]);
    }
    Ok((size, encode_hex(&hasher.finalize())))
}

fn verify_file(path: &Path, size: u64, digest: &str) -> Result<(), UpdateError> {
    let (actual_size, actual_digest) = sha256_file(path)?;
    if actual_size != size || actual_digest != digest {
        return Err(UpdateError::Archive);
    }
    Ok(())
}

fn prepare_empty_destination(destination: &Path) -> Result<(), UpdateError> {
    if destination.exists() {
        let metadata = fs::symlink_metadata(destination)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(UpdateError::Archive);
        }
        if fs::read_dir(destination)?.next().is_some() {
            return Err(UpdateError::Archive);
        }
    } else {
        fs::create_dir(destination)?;
    }
    Ok(())
}

#[cfg(unix)]
fn extract_platform_archive(
    archive: &Path,
    destination: &Path,
    artifact: &Artifact,
) -> Result<(), UpdateError> {
    use flate2::read::MultiGzDecoder;
    use tar::Archive;

    let decoder = MultiGzDecoder::new(File::open(archive)?);
    let mut archive = Archive::new(decoder);
    let expected = expected_files(artifact);
    let mut seen = HashSet::with_capacity(expected.len());
    for entry in archive.entries().map_err(|_| UpdateError::Archive)? {
        let mut entry = entry.map_err(|_| UpdateError::Archive)?;
        if !entry.header().entry_type().is_file() {
            return Err(UpdateError::Archive);
        }
        let path = entry.path().map_err(|_| UpdateError::Archive)?.into_owned();
        let name = path.to_str().ok_or(UpdateError::Archive)?.to_owned();
        if path.components().count() != 1
            || !expected.contains_key(name.as_str())
            || !seen.insert(name.clone())
        {
            return Err(UpdateError::Archive);
        }
        let ceiling = expected[name.as_str()];
        if entry.size() > ceiling {
            return Err(UpdateError::Archive);
        }
        let size = entry.size();
        write_bounded(&mut entry, &destination.join(&name), ceiling, Some(size))?;
    }
    if seen.len() != expected.len() {
        return Err(UpdateError::Archive);
    }
    set_executable(&destination.join("ash"))?;
    Ok(())
}

#[cfg(windows)]
fn extract_platform_archive(
    archive: &Path,
    destination: &Path,
    artifact: &Artifact,
) -> Result<(), UpdateError> {
    let mut archive =
        zip::ZipArchive::new(File::open(archive)?).map_err(|_| UpdateError::Archive)?;
    let expected = expected_files(artifact);
    if archive.len() != expected.len() {
        return Err(UpdateError::Archive);
    }
    let mut seen = HashSet::with_capacity(expected.len());
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| UpdateError::Archive)?;
        let name = entry.name().to_owned();
        if entry.is_dir() || !expected.contains_key(name.as_str()) || !seen.insert(name.clone()) {
            return Err(UpdateError::Archive);
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170_000;
            if kind != 0 && kind != 0o100_000 {
                return Err(UpdateError::Archive);
            }
        }
        let ceiling = expected[name.as_str()];
        if entry.size() > ceiling {
            return Err(UpdateError::Archive);
        }
        let size = entry.size();
        write_bounded(&mut entry, &destination.join(&name), ceiling, Some(size))?;
    }
    if seen.len() != expected.len() {
        return Err(UpdateError::Archive);
    }
    Ok(())
}

fn expected_files(artifact: &Artifact) -> std::collections::HashMap<&'static str, u64> {
    let binary = if artifact.target().contains("windows") {
        "ash.exe"
    } else {
        "ash"
    };
    [
        (binary, artifact.binary_size().min(MAX_BINARY_BYTES)),
        ("LICENSE", MAX_LICENSE_BYTES),
        ("THIRD-PARTY-LICENSES", MAX_LICENSE_INVENTORY_BYTES),
        ("release.json", MAX_RELEASE_METADATA_BYTES),
    ]
    .into_iter()
    .collect()
}

fn write_bounded(
    input: &mut impl Read,
    destination: &Path,
    ceiling: u64,
    expected_size: Option<u64>,
) -> Result<(), UpdateError> {
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut limited = input.take(ceiling.saturating_add(1));
    let copied = io::copy(&mut limited, &mut output)?;
    if copied > ceiling || expected_size.is_some_and(|expected| expected != copied) {
        return Err(UpdateError::Archive);
    }
    output.flush()?;
    output.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PackageMetadata {
    schema: u8,
    product: String,
    version: String,
    target: String,
    protocol: String,
    ason: String,
    commit: String,
    build: String,
    binary_sha256: String,
}

fn validate_package(
    destination: &Path,
    manifest: &ReleaseManifest,
    artifact: &Artifact,
) -> Result<ExtractedPackage, UpdateError> {
    let binary_name = if artifact.target().contains("windows") {
        "ash.exe"
    } else {
        "ash"
    };
    let binary = destination.join(binary_name);
    verify_file(&binary, artifact.binary_size(), artifact.binary_sha256())?;
    for name in [
        binary_name,
        "LICENSE",
        "THIRD-PARTY-LICENSES",
        "release.json",
    ] {
        let metadata = fs::symlink_metadata(destination.join(name))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(UpdateError::Archive);
        }
    }

    let metadata_bytes = read_bounded(
        &destination.join("release.json"),
        MAX_RELEASE_METADATA_BYTES,
    )?;
    let metadata: PackageMetadata =
        serde_json::from_slice(&metadata_bytes).map_err(|_| UpdateError::Package)?;
    if canonical_json(&metadata)? != metadata_bytes
        || metadata.schema != 1
        || metadata.product != "ash"
        || metadata.version != manifest.version()
        || metadata.target != artifact.target()
        || metadata.protocol != version_text(manifest.protocol_version())
        || metadata.ason != version_text(manifest.ason_version())
        || metadata.commit != manifest.source_commit()
        || metadata.binary_sha256 != artifact.binary_sha256()
        || !valid_build(&metadata.build)
    {
        return Err(UpdateError::Package);
    }
    Ok(ExtractedPackage {
        root: destination.to_owned(),
        binary,
    })
}

pub(crate) fn validate_local_version(
    root: &Path,
    expected_version: &str,
    expected_target: &str,
) -> Result<PathBuf, UpdateError> {
    let binary_name = if expected_target.contains("windows") {
        "ash.exe"
    } else {
        "ash"
    };
    let binary = root.join(binary_name);
    let metadata = fs::symlink_metadata(&binary)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_BINARY_BYTES
    {
        return Err(UpdateError::Package);
    }
    let release_path = root.join("release.json");
    let release_metadata = fs::symlink_metadata(&release_path)?;
    if !release_metadata.is_file() || release_metadata.file_type().is_symlink() {
        return Err(UpdateError::Package);
    }
    let metadata_bytes = read_bounded(&release_path, MAX_RELEASE_METADATA_BYTES)?;
    let release: PackageMetadata =
        serde_json::from_slice(&metadata_bytes).map_err(|_| UpdateError::Package)?;
    if canonical_json(&release)? != metadata_bytes
        || release.schema != 1
        || release.product != "ash"
        || release.version != expected_version
        || release.target != expected_target
        || release.protocol != "1"
        || release.ason != "1"
        || !canonical_lower_hex(&release.commit, 40)
        || !valid_build(&release.build)
        || !canonical_lower_hex(&release.binary_sha256, 64)
    {
        return Err(UpdateError::Package);
    }
    verify_file(&binary, metadata.len(), &release.binary_sha256)?;
    Ok(binary)
}

fn read_bounded(path: &Path, ceiling: u64) -> Result<Vec<u8>, UpdateError> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(ceiling.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| UpdateError::Archive)? > ceiling {
        return Err(UpdateError::Archive);
    }
    Ok(bytes)
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, UpdateError> {
    let mut encoded = serde_json::to_vec(value).map_err(|_| UpdateError::Package)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn version_text(version: (u16, u16)) -> String {
    if version.1 == 0 {
        version.0.to_string()
    } else {
        format!("{}.{}", version.0, version.1)
    }
}

fn valid_build(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn canonical_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::io::Write;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{extract_platform_archive, sha256_file};
    use crate::Artifact;

    fn artifact(size: u64) -> Artifact {
        Artifact {
            target: if cfg!(windows) {
                "x86_64-pc-windows-msvc".to_owned()
            } else {
                "x86_64-unknown-linux-musl".to_owned()
            },
            archive: String::new(),
            archive_size: 1,
            archive_sha256: "a".repeat(64),
            binary_size: size,
            binary_sha256: "b".repeat(64),
        }
    }

    #[cfg(windows)]
    fn write_archive(path: &Path, extra: bool) {
        use std::fs::File;

        use zip::write::SimpleFileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let mut archive = ZipWriter::new(File::create(path).expect("archive"));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, contents) in [
            ("ash.exe", b"binary".as_slice()),
            ("LICENSE", b"license".as_slice()),
            ("THIRD-PARTY-LICENSES", b"inventory".as_slice()),
            ("release.json", b"{}\n".as_slice()),
        ] {
            archive.start_file(name, options).expect("entry");
            archive.write_all(contents).expect("contents");
        }
        if extra {
            archive.start_file("../escape", options).expect("entry");
            archive.write_all(b"bad").expect("contents");
        }
        archive.finish().expect("finish");
    }

    #[cfg(unix)]
    fn write_archive(path: &Path, extra: bool) {
        use std::fs::File;

        use flate2::Compression;
        use flate2::write::GzEncoder;
        use tar::{Builder, Header};

        let encoder = GzEncoder::new(File::create(path).expect("archive"), Compression::default());
        let mut archive = Builder::new(encoder);
        for (name, contents) in [
            ("ash", b"binary".as_slice()),
            ("LICENSE", b"license".as_slice()),
            ("THIRD-PARTY-LICENSES", b"inventory".as_slice()),
            ("release.json", b"{}\n".as_slice()),
        ] {
            let mut header = Header::new_gnu();
            header.set_size(u64::try_from(contents.len()).expect("size"));
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, name, contents)
                .expect("entry");
        }
        if extra {
            let mut header = Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "escape", b"bad".as_slice())
                .expect("entry");
        }
        archive
            .into_inner()
            .expect("finish")
            .finish()
            .expect("gzip");
    }

    #[test]
    fn platform_archive_requires_the_exact_bounded_shape() {
        let directory = tempdir().expect("temporary directory");
        let archive = directory.path().join(if cfg!(windows) {
            "package.zip"
        } else {
            "package.tar.gz"
        });
        write_archive(&archive, false);
        let destination = directory.path().join("extract");
        std::fs::create_dir(&destination).expect("destination");
        extract_platform_archive(&archive, &destination, &artifact(6)).expect("extract");
        assert_eq!(
            std::fs::read(destination.join(if cfg!(windows) { "ash.exe" } else { "ash" }))
                .expect("binary"),
            b"binary"
        );
        assert_eq!(
            sha256_file(&archive).expect("hash").0,
            archive.metadata().expect("metadata").len()
        );

        let extra_archive = directory.path().join(if cfg!(windows) {
            "extra.zip"
        } else {
            "extra.tar.gz"
        });
        write_archive(&extra_archive, true);
        let extra_destination = directory.path().join("extra");
        std::fs::create_dir(&extra_destination).expect("destination");
        assert!(
            extract_platform_archive(&extra_archive, &extra_destination, &artifact(6)).is_err()
        );
    }
}
