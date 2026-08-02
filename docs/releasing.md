# Release operator contract

Status: workflow and release tooling implemented and locally verified; protected credentials and the first release are not provisioned

This document is the operator contract for `.github/workflows/release.yml` and `a3s-ash-release`. It separates code readiness from release authority: merging the workflow cannot produce a trusted release without an existing annotated tag, the complete secret set, valid platform identities, and all six clean-host gates.

## 1. Trust and repository configuration

Configure a GitHub environment named `stable-release` with required reviewers, deployment restricted to protected version tags, and no administrator bypass. Repository administrators must protect `main`, protect `v*` tags, require the normal CI workflow, and restrict who may create release tags.

The release workflow requires these secrets:

| Secret | Contract |
| --- | --- |
| `ASH_RELEASE_KEY_ID` | Lowercase identifier of at most 32 ASCII characters, such as `release-1`. |
| `ASH_RELEASE_SIGNING_KEY` | Exactly 64 lowercase hexadecimal characters encoding one 32-byte Ed25519 seed. |
| `ASH_RELEASE_TRUSTED_KEYS` | Ordered `key-id=public-key-hex` entries separated by semicolons; every release binary embeds this exact value. |
| `APPLE_CERTIFICATE_P12_BASE64` | Base64 Developer ID Application PKCS#12 bundle. |
| `APPLE_CERTIFICATE_PASSWORD` | Password for that PKCS#12 bundle. |
| `APPLE_SIGNING_IDENTITY` | Exact Developer ID Application identity passed to `codesign`. |
| `APPLE_ID` | Apple account used only by `notarytool`. |
| `APPLE_TEAM_ID` | Apple developer team identifier. |
| `APPLE_APP_PASSWORD` | App-specific notarization password. |
| `WINDOWS_CERTIFICATE_PFX_BASE64` | Base64 code-signing PFX with its private key. |
| `WINDOWS_CERTIFICATE_PASSWORD` | Password for that PFX. |

Generate and escrow the Ed25519 seed outside GitHub. Keep an offline recovery copy, record the public-key fingerprint in the release approval, and never place a seed in a command line, repository file, workflow output, artifact, or log. The workflow writes it with a restrictive umask to an ephemeral runner file and the Rust tool zeroizes its decoded buffers.

Key rotation is overlapping: first release binaries containing both the old and new ordered public keys, then switch `ASH_RELEASE_KEY_ID` and the signing seed in a later release. Removing the old key before an overlap release would make existing installations unable to verify the update and is therefore rejected operationally.

## 2. Tag contract

A release tag must be annotated and exactly match `vMAJOR.MINOR.PATCH`. The version must equal the `a3s-ash` workspace package version, the tag must resolve to the checked-out commit, and the worktree must be clean. Lightweight tags, prerelease/build suffixes, a mismatched Cargo version, or a reused GitHub Release all fail before compilation.

Create the tag only after required CI is green:

```sh
git switch main
git pull --ff-only origin main
git tag -s v0.1.0 -m "ash v0.1.0"
git push origin v0.1.0
```

Use an organization-approved signing identity for the Git tag. The workflow's `workflow_dispatch` entrypoint accepts only an already-existing annotated tag and is intended for a controlled retry, not for selecting an arbitrary commit.

Manifest sequence is derived without mutable server state:

```text
major * 1,000,000,000,000 + minor * 1,000,000 + patch
```

Each component is limited to `999999`, and sequence zero is invalid. Stable semantic-version ordering therefore implies monotonic update ordering.

## 3. Release state machine

```text
annotated tag
  -> 6 native builds
  -> macOS notarization / Windows Authenticode
  -> deterministic target packages
  -> complete-matrix Ed25519 manifest + signed checksums
  -> SPDX SBOM + Sigstore provenance
  -> draft GitHub Release
  -> 6 clean-host install/update/rollback/uninstall gates
  -> protected stable-release approval
  -> published latest release
```

The matrix uses native x86-64 and ARM64 runners for Linux, macOS, and Windows. Linux packages are rejected if the ELF binary contains a dynamic interpreter. Packaging runs the binary and requires an exact build identity containing version, Rust target, ASH/ASON versions, trusted-key fingerprint, and the tagged source commit. Platform signatures are applied before hashing and packaging.

The assembly job accepts exactly six canonical descriptors. It recomputes every archive size and SHA-256, creates an exact four-file tar or zip package, signs `release-manifest.json` with the same domain separator used by the updater, signs canonical `SHA256SUMS` with a distinct domain separator, validates the SPDX document, and uses the production verifier to extract every archive format on one host. A signing key not present under the requested key ID in `ASH_RELEASE_TRUSTED_KEYS` fails final verification.

GitHub creates a Sigstore provenance attestation over every subject in `SHA256SUMS`; the serialized bundle is published as `provenance.sigstore.json`. A draft is created only after these checks. Clean-host jobs verify provenance, install without elevated privileges, confirm signed metadata through `ash self update --from`, and uninstall. Starting with the second stable release, they must also install the previous release, update to the candidate, and roll back on all six targets before promotion.

## 4. Local tool interface

`a3s-ash-release` has three exact-option commands:

- `package` probes one native signed binary and emits its deterministic archive plus canonical descriptor;
- `sign` requires all six descriptors, the SBOM, release identity, trusted-key specification, and a seed file, then emits and verifies the complete release directory;
- `verify` rechecks manifest and checksum signatures, exact file inventory, SBOM shape, archive identities, embedded release metadata, and extracted binary identities without access to a private key.

The implementation and its cross-format tests are in `tools/release`. Run:

```sh
cargo test -p a3s-ash-release --all-targets --locked
cargo clippy -p a3s-ash-release --all-targets --locked -- -D warnings
```

Normal CI compiles and tests this tooling but never receives release secrets. The release workflow and every third-party action are pinned to exact commits.

The release quality job runs 1,000 deterministic cases for each checked-in fuzz corpus: ASON decoding, framed typed decoding, arbitrary update metadata, and validly signed update semantics. This is independent of the twice-weekly sustained workflow, whose evolving corpora and 90-day evidence bundles are documented in [`fuzz/README.md`](../fuzz/README.md). A release reviewer must inspect the accumulated run summaries and every retained finding; a green 1,000-case tag gate does not replace soak evidence.

## 5. Failure and recovery

- Failure before draft creation leaves only short-lived workflow artifacts. Fix code or credentials, delete no source tag, and use controlled dispatch against the same tag only when the source commit is unchanged.
- If a draft exists, the workflow intentionally refuses to overwrite it. Inspect its assets and attestations; an authorized release operator must delete a rejected draft before retrying.
- Failure in any clean-host job leaves the release as a draft. Never publish it manually to bypass a failed matrix target.
- The `stable-release` approval is granted only after reviewing tag identity, key fingerprint, platform signing identities, six target results, SBOM, and provenance links.
- A published release is immutable. Correct a defect with a higher patch version; never replace assets or reuse a sequence.
- Compromise of a signing key or platform identity follows `SECURITY.md`, pauses tag creation and promotion, revokes the affected identity, and requires an explicitly reviewed rotation release. Do not silently remove a trusted key from already-published metadata.
