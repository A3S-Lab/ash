use std::cmp::Ordering;
use std::sync::LazyLock;

use ash_update::{
    RELEASE_TARGETS, ReleaseSignature, TrustStore, UpdateDecision, UpdateError,
    canonical_signature, signing_payload, verify_release,
};
use ed25519_dalek::{Signer, SigningKey};

const KEY_ID: &str = "fuzz-1";
const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const COMMIT: &str = "cccccccccccccccccccccccccccccccccccccccc";

static SIGNING_KEY: LazyLock<SigningKey> = LazyLock::new(|| SigningKey::from_bytes(&[0x5a; 32]));
static TRUST: LazyLock<TrustStore> = LazyLock::new(|| {
    TrustStore::parse(&format!(
        "{KEY_ID}={}",
        encode_hex(SIGNING_KEY.verifying_key().as_bytes())
    ))
    .expect("fixed fuzz trust root")
});

pub fn exercise(data: &[u8]) {
    let installed = version(data, 0);
    let next = version(data, 3);
    let updater = version(data, 6);
    let minimum = version(data, 9);
    let sequence = 1 + u64::from(byte(data, 12));
    let highest_sequence = u64::from(byte(data, 13));
    let rollback = byte(data, 14) & 1 != 0;
    let invalid_mode = byte(data, 15) % 16;
    let target_index = usize::from(byte(data, 16)) % (RELEASE_TARGETS.len() + 1);
    let target = RELEASE_TARGETS
        .get(target_index)
        .copied()
        .unwrap_or("unsupported-target");
    let protocol = if byte(data, 17) & 3 == 0 {
        (2, 0)
    } else {
        (1, 0)
    };
    let ason = if byte(data, 18) & 3 == 0 {
        (1, 1)
    } else {
        (1, 0)
    };

    let manifest = manifest(
        &next.text,
        &minimum.text,
        sequence,
        rollback,
        invalid_mode,
        byte(data, 19),
    );
    let signature = SIGNING_KEY.sign(&signing_payload(&manifest)).to_bytes();
    let signature = canonical_signature(
        &ReleaseSignature::new(KEY_ID, signature).expect("fixed key id is valid"),
    )
    .expect("signature encoding");

    let result = verify_release(
        &manifest,
        &signature,
        &TRUST,
        &installed.text,
        &updater.text,
        protocol,
        ason,
        target,
        highest_sequence,
        None,
    );
    let repeated = verify_release(
        &manifest,
        &signature,
        &TRUST,
        &installed.text,
        &updater.text,
        protocol,
        ason,
        target,
        highest_sequence,
        None,
    );
    assert_eq!(outcome(&result), outcome(&repeated));

    let Ok(verified) = result else {
        return;
    };
    assert_eq!(invalid_mode, 0);
    assert_eq!(protocol, (1, 0));
    assert_eq!(ason, (1, 0));
    assert!(target_index < RELEASE_TARGETS.len());
    assert!(updater.components >= minimum.components);
    assert!(sequence > highest_sequence || highest_sequence == 0);
    assert_eq!(verified.artifact().target(), target);
    assert_eq!(verified.manifest().sequence(), sequence);
    assert_eq!(
        verified.decision(),
        match next.components.cmp(&installed.components) {
            Ordering::Less => {
                assert!(rollback);
                UpdateDecision::SignedRollback
            }
            Ordering::Equal => UpdateDecision::Current,
            Ordering::Greater => UpdateDecision::Update,
        }
    );

    let manifest_sha256 = verified.manifest_sha256().to_owned();
    assert!(
        verify_release(
            &manifest,
            &signature,
            &TRUST,
            &installed.text,
            &updater.text,
            protocol,
            ason,
            target,
            sequence,
            Some(&manifest_sha256),
        )
        .is_ok()
    );
    assert!(matches!(
        verify_release(
            &manifest,
            &signature,
            &TRUST,
            &installed.text,
            &updater.text,
            protocol,
            ason,
            target,
            sequence,
            Some(HASH_A),
        ),
        Err(UpdateError::SequenceRollback)
    ));
    assert!(matches!(
        verify_release(
            &manifest,
            &signature,
            &TRUST,
            &installed.text,
            &updater.text,
            protocol,
            ason,
            target,
            sequence + 1,
            None,
        ),
        Err(UpdateError::SequenceRollback)
    ));

    let mut tampered = manifest.clone();
    let commit = tampered
        .windows(COMMIT.len())
        .position(|window| window == COMMIT.as_bytes())
        .expect("source commit in generated manifest");
    tampered[commit] = b'd';
    assert!(matches!(
        verify_release(
            &tampered,
            &signature,
            &TRUST,
            &installed.text,
            &updater.text,
            protocol,
            ason,
            target,
            0,
            None,
        ),
        Err(UpdateError::Signature)
    ));
}

#[derive(Clone)]
struct VersionInput {
    components: [u8; 3],
    text: String,
}

fn version(data: &[u8], offset: usize) -> VersionInput {
    let components = [
        byte(data, offset) % 8,
        byte(data, offset + 1) % 8,
        byte(data, offset + 2) % 8,
    ];
    VersionInput {
        text: format!("{}.{}.{}", components[0], components[1], components[2]),
        components,
    }
}

fn byte(data: &[u8], index: usize) -> u8 {
    data.get(index).copied().unwrap_or_default()
}

fn manifest(
    version: &str,
    minimum_updater: &str,
    sequence: u64,
    rollback: bool,
    invalid_mode: u8,
    size_seed: u8,
) -> Vec<u8> {
    let schema = if invalid_mode == 1 { 2 } else { 1 };
    let product = if invalid_mode == 2 { "other" } else { "ash" };
    let channel = if invalid_mode == 3 { "edge" } else { "stable" };
    let sequence = if invalid_mode == 4 { 0 } else { sequence };
    let published_unix = if invalid_mode == 5 { 0 } else { 1_800_000_000 };
    let source_commit = if invalid_mode == 6 { "CC" } else { COMMIT };
    let protocol_major = if invalid_mode == 7 { 3 } else { 1 };
    let ason_minor = if invalid_mode == 8 { 2 } else { 0 };
    let version = if invalid_mode == 9 {
        "1.0.0-alpha"
    } else {
        version
    };
    let minimum_updater = if invalid_mode == 10 {
        "1.0.0+build"
    } else {
        minimum_updater
    };
    let key_id = if invalid_mode == 11 { "FUZZ" } else { KEY_ID };
    let artifact_count = if invalid_mode == 12 {
        RELEASE_TARGETS.len() - 1
    } else {
        RELEASE_TARGETS.len()
    };
    let mut artifacts = String::new();
    for (index, target) in RELEASE_TARGETS[..artifact_count].iter().enumerate() {
        if index != 0 {
            artifacts.push(',');
        }
        let target_value = if invalid_mode == 13 && index == 0 {
            RELEASE_TARGETS[1]
        } else {
            target
        };
        let extension = if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        };
        let archive_size = if invalid_mode == 14 && index == 0 {
            0
        } else {
            1 + u64::from(size_seed)
        };
        let archive_hash = if invalid_mode == 15 && index == 0 {
            HASH_A.to_ascii_uppercase()
        } else {
            HASH_A.to_owned()
        };
        artifacts.push_str(&format!(
            "{{\"target\":\"{target_value}\",\"archive\":\"ash-{target}.{extension}\",\"archive_size\":{archive_size},\"archive_sha256\":\"{archive_hash}\",\"binary_size\":{},\"binary_sha256\":\"{HASH_B}\"}}",
            1 + u64::from(size_seed) / 2
        ));
    }
    format!(
        "{{\"schema\":{schema},\"product\":\"{product}\",\"channel\":\"{channel}\",\"sequence\":{sequence},\"version\":\"{version}\",\"published_unix\":{published_unix},\"source_commit\":\"{source_commit}\",\"protocol_major\":{protocol_major},\"protocol_minor\":0,\"ason_major\":1,\"ason_minor\":{ason_minor},\"minimum_updater\":\"{minimum_updater}\",\"rollback\":{rollback},\"key_id\":\"{key_id}\",\"artifacts\":[{artifacts}]}}\n"
    )
    .into_bytes()
}

fn outcome(result: &Result<ash_update::VerifiedRelease, UpdateError>) -> String {
    match result {
        Ok(release) => format!(
            "ok:{:?}:{}:{}",
            release.decision(),
            release.manifest().sequence(),
            release.artifact().target()
        ),
        Err(error) => format!("error:{error:?}"),
    }
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
    use super::exercise;

    #[test]
    fn valid_current_update_and_rollback_paths_hold() {
        let mut current = [0_u8; 20];
        current[0..3].copy_from_slice(&[1, 2, 3]);
        current[3..6].copy_from_slice(&[1, 2, 3]);
        current[6..9].copy_from_slice(&[2, 0, 0]);
        current[9..12].copy_from_slice(&[1, 0, 0]);
        current[12] = 8;
        current[13] = 7;
        current[16] = 0;
        current[17] = 1;
        current[18] = 1;
        exercise(&current);

        let mut update = current;
        update[3..6].copy_from_slice(&[2, 0, 0]);
        exercise(&update);

        let mut rollback = current;
        rollback[3..6].copy_from_slice(&[0, 9, 0]);
        rollback[14] = 1;
        exercise(&rollback);
    }

    #[test]
    fn signed_semantic_rejection_matrix_is_deterministic() {
        for invalid_mode in 0..16 {
            for target in 0..=6 {
                for protocol_selector in 0..=1 {
                    for ason_selector in 0..=1 {
                        let mut data = [0_u8; 20];
                        data[3] = 1;
                        data[6] = 2;
                        data[9] = 1;
                        data[12] = 4;
                        data[13] = 3;
                        data[14] = invalid_mode & 1;
                        data[15] = invalid_mode;
                        data[16] = target;
                        data[17] = protocol_selector;
                        data[18] = ason_selector;
                        data[19] = 31;
                        exercise(&data);
                    }
                }
            }
        }
    }
}
