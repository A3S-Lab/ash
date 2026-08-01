use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use atomicwrites::{AllowOverwrite, AtomicFile};

use crate::{PlatformError, ResolvedPath, Workspace};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplaceOutcome {
    Committed {
        previous_digest: [u8; 32],
        new_digest: [u8; 32],
    },
    Conflict {
        actual_digest: [u8; 32],
    },
}

impl Workspace {
    /// Atomically replaces one regular file when its current BLAKE3 identity
    /// matches the caller's observed identity.
    pub fn compare_and_swap_replace(
        &self,
        logical: &str,
        expected_digest: [u8; 32],
        contents: &[u8],
        max_existing_bytes: u64,
    ) -> Result<ReplaceOutcome, PlatformError> {
        let target = self.resolve_mutation_file(logical)?;
        let current = self.read_mutation_file(&target, max_existing_bytes)?;
        let actual_digest = *blake3::hash(&current).as_bytes();
        if actual_digest != expected_digest {
            return Ok(ReplaceOutcome::Conflict { actual_digest });
        }
        let permissions = fs::metadata(&target.native)?.permissions();
        let new_digest = *blake3::hash(contents).as_bytes();
        let atomic = AtomicFile::new(&target.native, AllowOverwrite);
        match atomic.write(|file| {
            file.write_all(contents).map_err(WriteAbort::Io)?;
            file.set_permissions(permissions.clone())
                .map_err(WriteAbort::Io)?;
            let current = self
                .read_mutation_file(&target, max_existing_bytes)
                .map_err(WriteAbort::Platform)?;
            let observed = *blake3::hash(&current).as_bytes();
            if observed != expected_digest {
                return Err(WriteAbort::Conflict(observed));
            }
            Ok(())
        }) {
            Ok(()) => Ok(ReplaceOutcome::Committed {
                previous_digest: actual_digest,
                new_digest,
            }),
            Err(atomicwrites::Error::Internal(error)) => Err(error.into()),
            Err(atomicwrites::Error::User(WriteAbort::Io(error))) => Err(error.into()),
            Err(atomicwrites::Error::User(WriteAbort::Platform(error))) => Err(error),
            Err(atomicwrites::Error::User(WriteAbort::Conflict(actual_digest))) => {
                Ok(ReplaceOutcome::Conflict { actual_digest })
            }
        }
    }

    fn resolve_mutation_file(&self, logical: &str) -> Result<ResolvedPath, PlatformError> {
        let native = self.checked_mutation_path(logical, true)?;
        Ok(ResolvedPath {
            logical: logical.to_owned(),
            native,
        })
    }

    fn read_mutation_file(
        &self,
        target: &ResolvedPath,
        max_bytes: u64,
    ) -> Result<Vec<u8>, PlatformError> {
        let native = self.revalidate_mutation_file(target)?;
        let size = fs::metadata(&native)?.len();
        if size > max_bytes {
            return Err(PlatformError::InputTooLarge {
                size,
                max: max_bytes,
            });
        }
        Ok(fs::read(native)?)
    }

    fn revalidate_mutation_file(&self, target: &ResolvedPath) -> Result<PathBuf, PlatformError> {
        let native = self.checked_mutation_path(target.logical(), true)?;
        if native == target.native {
            Ok(native)
        } else {
            Err(PlatformError::InvalidMutationTarget)
        }
    }

    pub(crate) fn checked_mutation_path(
        &self,
        logical: &str,
        must_exist: bool,
    ) -> Result<PathBuf, PlatformError> {
        super::workspace::validate_logical(logical)?;
        if logical == "." {
            return Err(PlatformError::InvalidMutationTarget);
        }
        let mut native = self.native_root().to_path_buf();
        let components: Vec<_> = logical.split('/').collect();
        for (index, component) in components.iter().enumerate() {
            native.push(component);
            let final_component = index + 1 == components.len();
            match fs::symlink_metadata(&native) {
                Ok(metadata) => {
                    if is_link_like(&native, &metadata)
                        || (final_component && !metadata.is_file())
                        || (!final_component && !metadata.is_dir())
                    {
                        return Err(PlatformError::InvalidMutationTarget);
                    }
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && final_component
                        && !must_exist =>
                {
                    return Ok(native);
                }
                Err(error) => return Err(error.into()),
            }
        }
        let canonical = fs::canonicalize(&native)?;
        if canonical.starts_with(self.native_root()) {
            Ok(canonical)
        } else {
            Err(PlatformError::WorkspaceEscape)
        }
    }
}

enum WriteAbort {
    Io(io::Error),
    Platform(PlatformError),
    Conflict([u8; 32]),
}

#[cfg(unix)]
fn is_link_like(_path: &Path, metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_like(_path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::ReplaceOutcome;
    #[cfg(unix)]
    use crate::PlatformError;
    use crate::Workspace;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-mutation-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn replacement_is_atomic_and_compare_and_swap_guarded() {
        let directory = TestDirectory::new();
        let path = directory.0.join("file.txt");
        fs::write(&path, b"before").expect("write");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let expected = *blake3::hash(b"before").as_bytes();
        let outcome = workspace
            .compare_and_swap_replace("file.txt", expected, b"after", 1024)
            .expect("replace");
        assert_eq!(
            outcome,
            ReplaceOutcome::Committed {
                previous_digest: expected,
                new_digest: *blake3::hash(b"after").as_bytes(),
            }
        );
        assert_eq!(fs::read(&path).expect("read"), b"after");

        let outcome = workspace
            .compare_and_swap_replace("file.txt", expected, b"lost", 1024)
            .expect("conflict");
        assert_eq!(
            outcome,
            ReplaceOutcome::Conflict {
                actual_digest: *blake3::hash(b"after").as_bytes(),
            }
        );
        assert_eq!(fs::read(&path).expect("read"), b"after");
    }

    #[cfg(unix)]
    #[test]
    fn mutation_rejects_symlink_traversal() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        fs::write(directory.0.join("target"), b"target").expect("write");
        symlink("target", directory.0.join("link")).expect("symlink");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        assert!(matches!(
            workspace.compare_and_swap_replace(
                "link",
                *blake3::hash(b"target").as_bytes(),
                b"changed",
                1024,
            ),
            Err(PlatformError::InvalidMutationTarget)
        ));
    }
}
