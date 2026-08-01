use std::fs;
use std::io::Read;
use std::path::Path;

use crate::{PlatformError, ResolvedPath, Workspace};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub size: u64,
    pub digest: [u8; 32],
}

impl Workspace {
    /// Hashes one file with a fixed scratch buffer and an observed-size guard.
    pub fn hash_file_limited_sync(
        &self,
        path: &ResolvedPath,
        max_bytes: u64,
    ) -> Result<FileIdentity, PlatformError> {
        let native = self.revalidate(path)?;
        let metadata = fs::metadata(&native)?;
        if !metadata.is_file() {
            return Err(PlatformError::InvalidMutationTarget);
        }
        if metadata.len() > max_bytes {
            return Err(PlatformError::InputTooLarge {
                size: metadata.len(),
                max: max_bytes,
            });
        }
        let mut file = fs::File::open(native)?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut size = 0_u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            size = size
                .checked_add(read as u64)
                .ok_or(PlatformError::InputTooLarge {
                    size: u64::MAX,
                    max: max_bytes,
                })?;
            if size > max_bytes {
                return Err(PlatformError::InputTooLarge {
                    size,
                    max: max_bytes,
                });
            }
            hasher.update(&buffer[..read]);
        }
        Ok(FileIdentity {
            size,
            digest: *hasher.finalize().as_bytes(),
        })
    }

    /// Hashes the link target representation without following the link.
    pub fn symlink_digest(&self, logical: &str) -> Result<[u8; 32], PlatformError> {
        super::workspace::validate_logical(logical)?;
        if logical == "." {
            return Err(PlatformError::InvalidLogicalPath);
        }
        let native = logical
            .split('/')
            .fold(self.native_root().to_path_buf(), |path, component| {
                path.join(component)
            });
        let parent = native.parent().ok_or(PlatformError::WorkspaceEscape)?;
        let parent = fs::canonicalize(parent)?;
        self.ensure_contained(&parent)?;
        if !fs::symlink_metadata(&native)?.file_type().is_symlink() {
            return Err(PlatformError::InvalidMutationTarget);
        }
        let target = fs::read_link(native)?;
        Ok(*blake3::hash(&path_bytes(&target)).as_bytes())
    }
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::Workspace;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-identity-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn file_hashing_is_streamed_and_size_guarded() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("file"), b"content").expect("write");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let path = workspace.resolve_existing("file").expect("resolve");
        let identity = workspace
            .hash_file_limited_sync(&path, 7)
            .expect("identity");
        assert_eq!(identity.size, 7);
        assert_eq!(identity.digest, *blake3::hash(b"content").as_bytes());
        assert!(workspace.hash_file_limited_sync(&path, 6).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_identity_hashes_the_target_without_following_it() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        symlink("target", directory.0.join("link")).expect("symlink");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        assert_eq!(
            workspace.symlink_digest("link").expect("identity"),
            *blake3::hash(b"target").as_bytes()
        );
    }
}
