# Cross-platform distribution

Status: installers, signed self-update, release assembly, and six-native-target workflow implemented; credential provisioning and a published signed binary release remain open

This document defines how `ash` is built, installed, updated, rolled back, and removed on Linux, macOS, and Windows. The source tree contains offline-testable `install.sh` and `install.ps1` implementations plus an `ash-update` trust core and `ash self` coordinator that strictly verify canonical Ed25519-signed metadata, stream bounded release archives, activate healthy candidates, recover interrupted journals, and roll back. The online commands remain unavailable in ordinary development builds until a release public key is embedded and signed artifacts exist.

## 1. Distribution goals

- One native executable with no language runtime installation.
- User-scoped installation by default; no `sudo` or administrator token.
- One command for Linux/macOS and one command for Windows.
- Deterministic target detection and artifact selection.
- Mandatory checksum validation before activation.
- Signed update metadata, atomic activation, and rollback.
- Idempotent reinstall and PATH mutation.
- Clean uninstall backed by an installation receipt.
- Native x86-64 and ARM64 artifacts for every supported operating system.

## 2. Release target matrix

| Operating system | Architecture | Rust target | Archive |
| --- | --- | --- | --- |
| Linux | x86-64 | `x86_64-unknown-linux-musl` | `.tar.gz` |
| Linux | ARM64 | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS | Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS | Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows | x86-64 | `x86_64-pc-windows-msvc` | `.zip` |
| Windows | ARM64 | `aarch64-pc-windows-msvc` | `.zip` |

Linux release binaries must avoid dynamically installed OpenSSL, libc, or C++ runtime dependencies. macOS artifacts are signed and notarized. Windows artifacts are Authenticode-signed.

## 3. Artifact contract

Each GitHub Release contains stable target names so the `latest/download` route does not require JSON parsing:

```text
ash-x86_64-unknown-linux-musl.tar.gz
ash-aarch64-unknown-linux-musl.tar.gz
ash-x86_64-apple-darwin.tar.gz
ash-aarch64-apple-darwin.tar.gz
ash-x86_64-pc-windows-msvc.zip
ash-aarch64-pc-windows-msvc.zip
SHA256SUMS
SHA256SUMS.sig
release-manifest.json
release-manifest.sig
sbom.spdx.json
provenance.sigstore.json
```

Every archive contains exactly:

```text
ash or ash.exe
LICENSE
THIRD-PARTY-LICENSES
release.json
```

`release.json` records the product version, ASH protocol range, ASON format range, Rust target, source commit, build identifier, and binary digest. Release metadata remains JSON because it is machine-to-machine supply-chain data, not LLM prompt data.

## 4. One-click entrypoints

Linux and macOS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/A3S-Lab/ash/main/install.ps1 | iex
```

These commands must not be added to the primary README installation section until the corresponding scripts, signed release, and clean-host tests exist.

## 5. Installer behavior

Both installers implement the same state machine:

1. Parse installer options and environment overrides.
2. Detect operating system, architecture, and emulation state.
3. Resolve latest stable, requested version, or requested channel.
4. Construct the exact release asset name.
5. Create a user-owned temporary staging directory.
6. Download the archive and checksum file over HTTPS.
7. Verify the archive SHA-256 before extraction.
8. Validate archive shape and embedded `release.json`.
9. Verify that the binary reports the expected version and target.
10. Move the staged version into a versioned install directory.
11. Atomically switch the active executable.
12. Update the user PATH only when required and permitted.
13. Write an installation receipt atomically.
14. Remove staging data and print one compact success record.

Failure before activation leaves the active installation untouched. Failure after activation restores the prior launcher, version directory, receipt, and installer-owned PATH state. The CI smoke suite exercises fresh install, idempotent reinstall, forced reinstall, checksum and archive-shape rejection, lock contention, rollback before activation, paths containing spaces and Unicode, PATH ownership, and uninstall.

## 6. Target detection

Canonical installer target mapping:

| Detected OS | Detected machine | Target |
| --- | --- | --- |
| Linux | `x86_64`, `amd64` | `x86_64-unknown-linux-musl` |
| Linux | `aarch64`, `arm64` | `aarch64-unknown-linux-musl` |
| Darwin | `x86_64` | `x86_64-apple-darwin` |
| Darwin | `arm64` | `aarch64-apple-darwin` |
| Windows | AMD64 | `x86_64-pc-windows-msvc` |
| Windows | ARM64 | `aarch64-pc-windows-msvc` |

The installer rejects unknown architectures rather than guessing. On macOS and Windows it distinguishes a native process from an emulated shell so the selected artifact matches the operating system's executable architecture policy.

## 7. Install locations

### 7.1 Linux and macOS

Default layout:

```text
${ASH_HOME:-$HOME/.local/share/ash}/
|-- versions/<version>/ash
|-- active/ash
`-- install-receipt.json

${ASH_BIN_DIR:-$HOME/.local/bin}/ash -> ../share/ash/active/ash
```

The exact link target is calculated rather than assumed when custom directories are used. The current Unix installer requires symbolic-link support and rejects an unowned launcher instead of replacing it.

### 7.2 Windows

Default layout:

```text
%LOCALAPPDATA%\Programs\ash\
|-- versions\<version>\ash.exe
|-- active\ash.exe
`-- install-receipt.json
```

`%LOCALAPPDATA%\Programs\ash\active` is added to the user PATH. Registry updates are user-scoped and followed by an environment-change notification. The installer never mutates the machine PATH unless explicitly run in a future system-install mode.

## 8. Installer options

The shell and PowerShell installers expose equivalent options:

| Capability | Shell | PowerShell |
| --- | --- | --- |
| pinned version | `--version <v>` | `-Version <v>` |
| release channel | `--channel <name>` | `-Channel <name>` |
| install root | `--prefix <path>` | `-Prefix <path>` |
| binary directory | `--bin-dir <path>` | `-BinDir <path>` |
| skip PATH update | `--no-path` | `-NoPath` |
| replace current version | `--force` | `-Force` |
| offline archive | `--archive <path>` | `-Archive <path>` |
| expected checksum | `--sha256 <hex>` | `-Sha256 <hex>` |
| uninstall | `--uninstall` | `-Uninstall` |

Environment variables use the `ASH_INSTALL_` prefix and never override an explicit option.

## 9. PATH policy

PATH changes are:

- user-scoped;
- idempotent;
- resolved to an absolute path;
- performed only when the directory is absent;
- recorded in the receipt;
- reversed on uninstall only when the installer originally added the entry.

The Unix installer recognizes common POSIX shell profiles but does not append duplicate commands to every profile. It updates one selected profile and records the exact line. A non-interactive environment may opt out and consume the installed path directly.

## 10. Integrity and trust

### 10.1 Bootstrap install

The one-click bootstrap trusts HTTPS delivery of the source-controlled installer. The installer then validates the downloaded archive against `SHA256SUMS`. A signature bundle and build provenance are published for independent verification.

Checksums fetched from the same release origin protect against corruption and accidental substitution but do not by themselves defend against compromise of that origin. `SHA256SUMS.sig` is a canonical Ed25519 signature for independent verification, and GitHub's Sigstore bundle binds the listed assets to the release workflow. The bootstrap installer intentionally relies only on HTTPS plus the checksum; documentation must state this boundary rather than describe checksums as complete supply-chain verification.

### 10.2 Self-update

The installed binary contains the trusted release-metadata public key. `ash self update` verifies:

- metadata signature;
- channel and rollback policy;
- artifact target and version;
- archive digest and extracted binary digest;
- protocol compatibility;
- installation provenance and owned paths.

The initial build embeds an ordered key set through `ASH_RELEASE_TRUSTED_KEYS`; builds without it fail before network access. Threshold key rotation is a future manifest-version extension and is not claimed by the version-one verifier.

## 11. Update and rollback

The install root keeps the active version and at least one previous healthy version. Update flow:

1. Acquire an install-root lock.
2. Recover or reject any incomplete prior journal.
3. Resolve and verify signed release metadata.
4. Download and verify into a new version directory.
5. Run `ash self check --candidate <path>`.
6. Atomically switch the active pointer or launcher.
7. Run a protocol handshake health check.
8. Commit the receipt and monotonic update state while retaining the prior healthy version.

Implemented machine entrypoints are:

```text
ash self status [--prefix <path>]
ash self check --candidate <path>
ash self update [--prefix <path>] [--from <release-directory>]
ash self rollback [--prefix <path>]
ash self recover [--prefix <path>]
```

`--from` runs the identical signature, archive, activation, and health path without network access. Success and failure output is canonical ASON; update failures use diagnostic family `11`.

If the health check fails, activation returns to the previous version and records the failed candidate.

On Windows, a running executable cannot reliably replace itself. The updater launches the verified candidate in a private replacement mode, validates the owned receipt and state-bound journal, exits the parent, performs the switch with bounded retries, runs the health check, and completes or rolls back the receipt.

## 12. Uninstall

Uninstall reads the receipt and removes only paths owned by that installation. It refuses recursive removal when ownership, resolved root, or receipt integrity cannot be established.

The flow removes the active launcher, installed versions, updater state, and installer-added PATH entry. Session caches are removed only with an explicit `--purge` option. On Windows, the uninstall helper completes deletion after the running process exits.

## 13. Release workflow

The implemented tag-triggered workflow performs the following fail-closed sequence:

1. Verify a clean, annotated version tag.
2. Run protocol, unit, integration, fuzz-smoke, and platform contract gates.
3. Build each target from a pinned Rust toolchain and locked dependencies.
4. Strip release binaries without removing required signing metadata.
5. Generate deterministic archive contents, signed checksums, an SPDX 2.3 SBOM, and Sigstore provenance.
6. Sign macOS and Windows binaries.
7. Notarize macOS artifacts.
8. Sign checksums and the release manifest.
9. Publish a draft GitHub Release.
10. Run clean-host installer, signed-current confirmation, uninstall, and, when a prior stable release exists, upgrade and rollback tests against the draft assets.
11. Promote the release only after all required target evidence passes.

No target is silently omitted from a stable release. A missing target blocks promotion or requires a documented platform-support change before tagging.

Linux x86-64 and ARM64, macOS Intel and Apple Silicon, and Windows x86-64 and ARM64 build on matching GitHub-hosted native runners. macOS artifacts must pass Developer ID signing and notarization; Windows artifacts must pass Authenticode signing and timestamp verification. The assembly job accepts exactly one descriptor for every target, recomputes archive identities, signs a monotonic manifest derived from the stable semantic version, then cross-extracts all tar and zip packages with the production update verifier before a draft can exist. The protected `stable-release` environment is the final promotion boundary. See the [release operator contract](./releasing.md) for exact repository configuration and recovery rules.

## 14. Installer test matrix

Required scenarios include:

- fresh user account with no administrator privilege;
- install path containing spaces and Unicode;
- PATH already containing the target directory;
- shell profile or user PATH not writable;
- latest, pinned, and offline installation;
- rerun of the same version;
- forced reinstall;
- interrupted download and corrupted archive;
- checksum mismatch;
- concurrent installer lock contention;
- upgrade from the previous stable release;
- candidate health-check failure and rollback;
- uninstall with and without cache purge;
- Windows replacement while the old binary is running.

Every scenario asserts that temporary files, helper processes, locks, and PATH entries are left in a known state.

## 15. Package managers

Homebrew, WinGet, Scoop, and system packages may be added after the standalone release path is proven. Package-manager installs record their provenance and defer upgrades and uninstallation to their owner. `ash self update` must refuse to replace a package-manager-owned binary and return a structured handoff result.
