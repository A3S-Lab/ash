use std::collections::HashSet;

use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MANIFEST_DOMAIN: &[u8] = b"ash-release-manifest-v1\0";
const PRODUCT: &str = "ash";
const CHANNEL: &str = "stable";
const MAX_KEY_ID_BYTES: usize = 32;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_SIGNATURE_BYTES: usize = 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

pub const RELEASE_TARGETS: [&str; 6] = [
    "aarch64-apple-darwin",
    "aarch64-pc-windows-msvc",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-musl",
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    schema: u8,
    product: String,
    channel: String,
    sequence: u64,
    version: String,
    published_unix: u64,
    source_commit: String,
    protocol_major: u16,
    protocol_minor: u16,
    ason_major: u16,
    ason_minor: u16,
    minimum_updater: String,
    rollback: bool,
    key_id: String,
    artifacts: Vec<Artifact>,
}

impl ReleaseManifest {
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    #[must_use]
    pub fn source_commit(&self) -> &str {
        &self.source_commit
    }

    #[must_use]
    pub const fn protocol_version(&self) -> (u16, u16) {
        (self.protocol_major, self.protocol_minor)
    }

    #[must_use]
    pub const fn ason_version(&self) -> (u16, u16) {
        (self.ason_major, self.ason_minor)
    }

    #[must_use]
    pub fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub(crate) target: String,
    pub(crate) archive: String,
    pub(crate) archive_size: u64,
    pub(crate) archive_sha256: String,
    pub(crate) binary_size: u64,
    pub(crate) binary_sha256: String,
}

impl Artifact {
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    #[must_use]
    pub fn archive(&self) -> &str {
        &self.archive
    }

    #[must_use]
    pub const fn archive_size(&self) -> u64 {
        self.archive_size
    }

    #[must_use]
    pub fn archive_sha256(&self) -> &str {
        &self.archive_sha256
    }

    #[must_use]
    pub const fn binary_size(&self) -> u64 {
        self.binary_size
    }

    #[must_use]
    pub fn binary_sha256(&self) -> &str {
        &self.binary_sha256
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSignature {
    schema: u8,
    key_id: String,
    signature: String,
}

impl ReleaseSignature {
    pub fn new(key_id: &str, signature: [u8; 64]) -> Result<Self, UpdateError> {
        validate_key_id(key_id)?;
        Ok(Self {
            schema: 1,
            key_id: key_id.to_owned(),
            signature: encode_hex(&signature),
        })
    }
}

#[derive(Clone, Debug)]
pub struct TrustStore {
    keys: Vec<(String, VerifyingKey)>,
    fingerprint: String,
}

impl TrustStore {
    pub fn parse(specification: &str) -> Result<Self, UpdateError> {
        if specification.is_empty() {
            return Err(UpdateError::TrustUnavailable);
        }
        let mut keys = Vec::new();
        let mut previous = None;
        for entry in specification.split(';') {
            let (key_id, encoded) = entry.split_once('=').ok_or(UpdateError::Trust)?;
            validate_key_id(key_id)?;
            if previous.is_some_and(|value: &str| value >= key_id) {
                return Err(UpdateError::Trust);
            }
            let bytes = decode_hex_array::<32>(encoded).map_err(|_| UpdateError::Trust)?;
            let key = VerifyingKey::from_bytes(&bytes).map_err(|_| UpdateError::Trust)?;
            if key.is_weak() {
                return Err(UpdateError::Trust);
            }
            keys.push((key_id.to_owned(), key));
            previous = Some(key_id);
        }
        let fingerprint = sha256_hex(specification.as_bytes());
        Ok(Self { keys, fingerprint })
    }

    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn key(&self, key_id: &str) -> Result<&VerifyingKey, UpdateError> {
        self.keys
            .iter()
            .find_map(|(candidate, key)| (candidate == key_id).then_some(key))
            .ok_or(UpdateError::UntrustedKey)
    }
}

pub fn embedded_trust_store() -> Result<TrustStore, UpdateError> {
    option_env!("ASH_RELEASE_TRUSTED_KEYS")
        .ok_or(UpdateError::TrustUnavailable)
        .and_then(TrustStore::parse)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateDecision {
    Current,
    Update,
    SignedRollback,
}

#[derive(Clone, Debug)]
pub struct VerifiedRelease {
    manifest: ReleaseManifest,
    artifact: Artifact,
    manifest_sha256: String,
    decision: UpdateDecision,
}

impl VerifiedRelease {
    #[must_use]
    pub fn manifest(&self) -> &ReleaseManifest {
        &self.manifest
    }

    #[must_use]
    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    #[must_use]
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    #[must_use]
    pub const fn decision(&self) -> UpdateDecision {
        self.decision
    }
}

#[allow(clippy::too_many_arguments)]
pub fn verify_release(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    trust: &TrustStore,
    installed_version: &str,
    updater_version: &str,
    current_protocol: (u16, u16),
    current_ason: (u16, u16),
    target: &str,
    highest_sequence: u64,
    highest_manifest_sha256: Option<&str>,
) -> Result<VerifiedRelease, UpdateError> {
    if manifest_bytes.is_empty() || manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UpdateError::ManifestSize);
    }
    if signature_bytes.is_empty() || signature_bytes.len() > MAX_SIGNATURE_BYTES {
        return Err(UpdateError::SignatureSize);
    }
    let manifest: ReleaseManifest =
        serde_json::from_slice(manifest_bytes).map_err(|_| UpdateError::Manifest)?;
    if canonical_manifest(&manifest)? != manifest_bytes {
        return Err(UpdateError::NonCanonical);
    }
    let signature: ReleaseSignature =
        serde_json::from_slice(signature_bytes).map_err(|_| UpdateError::Signature)?;
    if canonical_signature(&signature)? != signature_bytes {
        return Err(UpdateError::NonCanonical);
    }
    if signature.schema != 1 || signature.key_id != manifest.key_id {
        return Err(UpdateError::Signature);
    }
    let signature_value = Signature::from_bytes(
        &decode_hex_array::<64>(&signature.signature).map_err(|_| UpdateError::Signature)?,
    );
    trust
        .key(&signature.key_id)?
        .verify_strict(&signing_payload(manifest_bytes), &signature_value)
        .map_err(|_| UpdateError::Signature)?;

    validate_manifest(&manifest, current_protocol, current_ason)?;
    let current = Version::parse(installed_version).map_err(|_| UpdateError::Version)?;
    let updater = Version::parse(updater_version).map_err(|_| UpdateError::Version)?;
    let next = Version::parse(&manifest.version).map_err(|_| UpdateError::Version)?;
    let minimum = Version::parse(&manifest.minimum_updater).map_err(|_| UpdateError::Version)?;
    if updater < minimum {
        return Err(UpdateError::UpdaterTooOld);
    }
    let decision = if next == current {
        UpdateDecision::Current
    } else if next > current {
        UpdateDecision::Update
    } else if manifest.rollback {
        UpdateDecision::SignedRollback
    } else {
        return Err(UpdateError::RollbackDenied);
    };
    let manifest_sha256 = sha256_hex(manifest_bytes);
    if manifest.sequence < highest_sequence
        || (manifest.sequence == highest_sequence
            && highest_sequence != 0
            && highest_manifest_sha256 != Some(manifest_sha256.as_str()))
    {
        return Err(UpdateError::SequenceRollback);
    }
    let artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.target == target)
        .cloned()
        .ok_or(UpdateError::Target)?;
    Ok(VerifiedRelease {
        manifest,
        artifact,
        manifest_sha256,
        decision,
    })
}

pub fn canonical_manifest(manifest: &ReleaseManifest) -> Result<Vec<u8>, UpdateError> {
    canonical_json(manifest)
}

pub fn canonical_signature(signature: &ReleaseSignature) -> Result<Vec<u8>, UpdateError> {
    canonical_json(signature)
}

pub fn signing_payload(manifest_bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(MANIFEST_DOMAIN.len() + manifest_bytes.len());
    payload.extend_from_slice(MANIFEST_DOMAIN);
    payload.extend_from_slice(manifest_bytes);
    payload
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, UpdateError> {
    let mut encoded = serde_json::to_vec(value).map_err(|_| UpdateError::Manifest)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn validate_manifest(
    manifest: &ReleaseManifest,
    protocol: (u16, u16),
    ason: (u16, u16),
) -> Result<(), UpdateError> {
    validate_key_id(&manifest.key_id)?;
    if manifest.schema != 1
        || manifest.product != PRODUCT
        || manifest.channel != CHANNEL
        || manifest.sequence == 0
        || manifest.published_unix == 0
        || (manifest.protocol_major, manifest.protocol_minor) != protocol
        || (manifest.ason_major, manifest.ason_minor) != ason
        || !canonical_hex(&manifest.source_commit, 40)
    {
        return Err(UpdateError::Manifest);
    }
    let version = Version::parse(&manifest.version).map_err(|_| UpdateError::Version)?;
    let minimum = Version::parse(&manifest.minimum_updater).map_err(|_| UpdateError::Version)?;
    if !version.pre.is_empty()
        || !version.build.is_empty()
        || !minimum.pre.is_empty()
        || !minimum.build.is_empty()
    {
        return Err(UpdateError::Version);
    }
    if manifest.artifacts.len() != RELEASE_TARGETS.len() {
        return Err(UpdateError::Artifacts);
    }
    let mut targets = HashSet::with_capacity(RELEASE_TARGETS.len());
    for (artifact, expected_target) in manifest.artifacts.iter().zip(RELEASE_TARGETS) {
        if artifact.target != expected_target
            || !targets.insert(artifact.target.as_str())
            || artifact.archive != archive_name(expected_target)
            || artifact.archive_size == 0
            || artifact.archive_size > MAX_ARCHIVE_BYTES
            || artifact.binary_size == 0
            || artifact.binary_size > MAX_BINARY_BYTES
            || !canonical_hex(&artifact.archive_sha256, 64)
            || !canonical_hex(&artifact.binary_sha256, 64)
        {
            return Err(UpdateError::Artifacts);
        }
    }
    Ok(())
}

fn archive_name(target: &str) -> String {
    let extension = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("ash-{target}.{extension}")
}

fn validate_key_id(value: &str) -> Result<(), UpdateError> {
    if value.is_empty()
        || value.len() > MAX_KEY_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Err(UpdateError::Trust)
    } else {
        Ok(())
    }
}

fn canonical_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(value: &[u8]) -> String {
    encode_hex(&Sha256::digest(value))
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

fn decode_hex_array<const N: usize>(value: &str) -> Result<[u8; N], UpdateError> {
    if value.len() != N * 2 || !canonical_hex(value, N * 2) {
        return Err(UpdateError::Signature);
    }
    let mut bytes = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Result<u8, UpdateError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(UpdateError::Signature),
    }
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("release trust is not configured in this build")]
    TrustUnavailable,
    #[error("release trust configuration is invalid")]
    Trust,
    #[error("release manifest exceeds its hard size ceiling")]
    ManifestSize,
    #[error("release signature exceeds its hard size ceiling")]
    SignatureSize,
    #[error("release manifest is invalid")]
    Manifest,
    #[error("release signature document is invalid")]
    Signature,
    #[error("release metadata is not canonical JSON")]
    NonCanonical,
    #[error("release signing key is not trusted")]
    UntrustedKey,
    #[error("release version is invalid")]
    Version,
    #[error("this ash binary is too old to verify the release")]
    UpdaterTooOld,
    #[error("unsigned or unauthorized version rollback was refused")]
    RollbackDenied,
    #[error("release sequence rollback or equivocation was refused")]
    SequenceRollback,
    #[error("release target matrix is incomplete or invalid")]
    Artifacts,
    #[error("release has no artifact for this target")]
    Target,
    #[error("release archive or package shape is invalid")]
    Archive,
    #[error("embedded release metadata is invalid")]
    Package,
    #[error("installation receipt, state, path ownership, or journal is invalid")]
    Installation,
    #[error("another installer or updater owns the installation lock")]
    InstallLock,
    #[error("an incomplete update must be recovered before this action")]
    PendingUpdate,
    #[error("the candidate or rollback health check failed")]
    Health,
    #[error("no validated previous version is available for rollback")]
    NoRollback,
    #[error("activation or rollback could not establish a provable state")]
    Activation,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::{
        Artifact, RELEASE_TARGETS, ReleaseManifest, ReleaseSignature, TrustStore, UpdateDecision,
        UpdateError, canonical_manifest, canonical_signature, encode_hex, signing_payload,
        verify_release,
    };

    fn manifest() -> ReleaseManifest {
        ReleaseManifest {
            schema: 1,
            product: "ash".to_owned(),
            channel: "stable".to_owned(),
            sequence: 7,
            version: "1.2.3".to_owned(),
            published_unix: 1_800_000_000,
            source_commit: "a".repeat(40),
            protocol_major: 1,
            protocol_minor: 0,
            ason_major: 1,
            ason_minor: 0,
            minimum_updater: "0.1.0".to_owned(),
            rollback: false,
            key_id: "test-1".to_owned(),
            artifacts: RELEASE_TARGETS
                .into_iter()
                .map(|target| Artifact {
                    target: target.to_owned(),
                    archive: super::archive_name(target),
                    archive_size: 1024,
                    archive_sha256: "b".repeat(64),
                    binary_size: 512,
                    binary_sha256: "c".repeat(64),
                })
                .collect(),
        }
    }

    fn signed(manifest: &ReleaseManifest) -> (Vec<u8>, Vec<u8>, TrustStore) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let manifest = canonical_manifest(manifest).expect("manifest");
        let signature = signing.sign(&signing_payload(&manifest)).to_bytes();
        let signature = canonical_signature(
            &ReleaseSignature::new("test-1", signature).expect("signature document"),
        )
        .expect("signature");
        let trust = TrustStore::parse(&format!(
            "test-1={}",
            encode_hex(signing.verifying_key().as_bytes())
        ))
        .expect("trust");
        (manifest, signature, trust)
    }

    #[test]
    fn signed_manifest_is_target_version_and_sequence_bound() {
        let (manifest, signature, trust) = signed(&manifest());
        let verified = verify_release(
            &manifest,
            &signature,
            &trust,
            "1.0.0",
            "1.0.0",
            (1, 0),
            (1, 0),
            "x86_64-pc-windows-msvc",
            6,
            None,
        )
        .expect("verified release");
        assert_eq!(verified.manifest().version(), "1.2.3");
        assert_eq!(verified.artifact().target(), "x86_64-pc-windows-msvc");
        assert_eq!(verified.decision(), UpdateDecision::Update);

        assert!(matches!(
            verify_release(
                &manifest,
                &signature,
                &trust,
                "1.0.0",
                "1.0.0",
                (1, 0),
                (1, 0),
                "x86_64-pc-windows-msvc",
                8,
                None,
            ),
            Err(UpdateError::SequenceRollback)
        ));
    }

    #[test]
    fn altered_noncanonical_and_untrusted_metadata_fail_closed() {
        let (mut encoded, signature, trust) = signed(&manifest());
        let position = encoded
            .windows(b"1.2.3".len())
            .position(|window| window == b"1.2.3")
            .expect("version");
        encoded[position] = b'2';
        assert!(matches!(
            verify_release(
                &encoded,
                &signature,
                &trust,
                "1.0.0",
                "1.0.0",
                (1, 0),
                (1, 0),
                "x86_64-pc-windows-msvc",
                0,
                None,
            ),
            Err(UpdateError::Signature)
        ));

        let (mut encoded, signature, trust) = signed(&manifest());
        encoded.insert(1, b' ');
        assert!(matches!(
            verify_release(
                &encoded,
                &signature,
                &trust,
                "1.0.0",
                "1.0.0",
                (1, 0),
                (1, 0),
                "x86_64-pc-windows-msvc",
                0,
                None,
            ),
            Err(UpdateError::NonCanonical)
        ));

        let other = SigningKey::from_bytes(&[8; 32]);
        let other = TrustStore::parse(&format!(
            "other={}",
            encode_hex(other.verifying_key().as_bytes())
        ))
        .expect("other trust");
        assert!(matches!(
            verify_release(
                &canonical_manifest(&manifest()).expect("manifest"),
                &signature,
                &other,
                "1.0.0",
                "1.0.0",
                (1, 0),
                (1, 0),
                "x86_64-pc-windows-msvc",
                0,
                None,
            ),
            Err(UpdateError::UntrustedKey)
        ));
    }

    #[test]
    fn downgrade_requires_a_signed_rollback_declaration() {
        let mut release = manifest();
        release.version = "0.9.0".to_owned();
        let (encoded, signature, trust) = signed(&release);
        assert!(matches!(
            verify_release(
                &encoded,
                &signature,
                &trust,
                "1.0.0",
                "1.0.0",
                (1, 0),
                (1, 0),
                "x86_64-unknown-linux-musl",
                0,
                None,
            ),
            Err(UpdateError::RollbackDenied)
        ));

        release.rollback = true;
        let (encoded, signature, trust) = signed(&release);
        assert_eq!(
            verify_release(
                &encoded,
                &signature,
                &trust,
                "1.0.0",
                "1.0.0",
                (1, 0),
                (1, 0),
                "x86_64-unknown-linux-musl",
                0,
                None,
            )
            .expect("signed rollback")
            .decision(),
            UpdateDecision::SignedRollback
        );
    }

    #[test]
    fn minimum_version_applies_to_the_verifier_not_the_installed_receipt() {
        let mut release = manifest();
        release.minimum_updater = "2.0.0".to_owned();
        release.version = "4.0.0".to_owned();
        let (encoded, signature, trust) = signed(&release);
        assert!(
            verify_release(
                &encoded,
                &signature,
                &trust,
                "3.0.0",
                "2.0.0",
                (1, 0),
                (1, 0),
                "x86_64-unknown-linux-musl",
                0,
                None,
            )
            .is_ok()
        );
        assert!(matches!(
            verify_release(
                &encoded,
                &signature,
                &trust,
                "3.0.0",
                "1.9.9",
                (1, 0),
                (1, 0),
                "x86_64-unknown-linux-musl",
                0,
                None,
            ),
            Err(UpdateError::UpdaterTooOld)
        ));
    }
}
