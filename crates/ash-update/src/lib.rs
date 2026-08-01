#![forbid(unsafe_code)]

//! Signed release verification, bounded package extraction, and activation.

mod archive;
mod manifest;

pub use archive::{ExtractedPackage, extract_release_archive, sha256_file};
pub use manifest::{
    Artifact, ReleaseManifest, ReleaseSignature, TrustStore, UpdateDecision, UpdateError,
    VerifiedRelease, canonical_manifest, canonical_signature, embedded_trust_store,
    signing_payload, verify_release,
};
