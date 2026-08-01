#![forbid(unsafe_code)]

//! Signed release verification, bounded package extraction, and activation.

mod archive;
mod install;
mod manifest;

pub use archive::{ExtractedPackage, extract_release_archive, sha256_file};
pub use install::{
    ActivationOutcome, InstallationInfo, RecoveryOutcome, complete_pending_activation,
    confirm_current_release, inspect_installation, install_release, recover_installation,
    rollback_installation,
};
pub use manifest::{
    Artifact, MAX_ARCHIVE_BYTES, MAX_BINARY_BYTES, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES,
    RELEASE_TARGETS, ReleaseManifest, ReleaseSignature, TrustStore, UpdateDecision, UpdateError,
    VerifiedRelease, canonical_manifest, canonical_signature, embedded_trust_store,
    signing_payload, verify_release,
};
