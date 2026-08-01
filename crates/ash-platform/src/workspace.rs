use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::PlatformError;

/// Canonical workspace capability used for all native path access.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

/// Existing path that was resolved and checked against a workspace root.
#[derive(Clone, Debug)]
pub struct ResolvedPath {
    logical: String,
    pub(crate) native: PathBuf,
}

impl ResolvedPath {
    #[must_use]
    pub fn logical(&self) -> &str {
        &self.logical
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalkOptions {
    pub max_depth: u16,
    pub include_hidden: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeEntry {
    pub logical: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified_millis: Option<i64>,
}

impl Workspace {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, PlatformError> {
        let root = fs::canonicalize(root)?;
        if !fs::metadata(&root)?.is_dir() {
            return Err(PlatformError::InvalidWorkspace);
        }
        Ok(Self { root })
    }

    #[must_use]
    pub fn native_root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_existing(&self, logical: &str) -> Result<ResolvedPath, PlatformError> {
        validate_logical(logical)?;
        let joined = if logical == "." {
            self.root.clone()
        } else {
            logical
                .split('/')
                .fold(self.root.clone(), |path, component| path.join(component))
        };
        let native = fs::canonicalize(joined)?;
        self.ensure_contained(&native)?;
        Ok(ResolvedPath {
            logical: logical.to_owned(),
            native,
        })
    }

    pub async fn read(&self, path: &ResolvedPath) -> Result<Vec<u8>, PlatformError> {
        let native = self.revalidate(path)?;
        Ok(tokio::fs::read(native).await?)
    }

    pub fn read_sync(&self, path: &ResolvedPath) -> Result<Vec<u8>, PlatformError> {
        let native = self.revalidate(path)?;
        Ok(fs::read(native)?)
    }

    pub fn read_limited_sync(
        &self,
        path: &ResolvedPath,
        max_bytes: u64,
    ) -> Result<Vec<u8>, PlatformError> {
        let native = self.revalidate(path)?;
        let size = fs::metadata(&native)?.len();
        if size > max_bytes {
            return Err(PlatformError::InputTooLarge {
                size,
                max: max_bytes,
            });
        }
        Ok(fs::read(native)?)
    }

    pub fn walk(
        &self,
        root: &ResolvedPath,
        options: WalkOptions,
    ) -> Result<Vec<NativeEntry>, PlatformError> {
        let native = self.revalidate(root)?;
        let mut entries = Vec::new();
        self.walk_inner(&native, 0, options, &mut entries)?;
        entries
            .sort_unstable_by(|left, right| left.logical.as_bytes().cmp(right.logical.as_bytes()));
        Ok(entries)
    }

    fn walk_inner(
        &self,
        native: &Path,
        depth: u16,
        options: WalkOptions,
        output: &mut Vec<NativeEntry>,
    ) -> Result<(), PlatformError> {
        let metadata = fs::symlink_metadata(native)?;
        let hidden = depth > 0 && is_hidden(native, &metadata);
        if hidden && !options.include_hidden {
            return Ok(());
        }
        let kind = classify(&metadata);
        output.push(NativeEntry {
            logical: self.logical_from_native(native)?,
            kind,
            size: if kind == EntryKind::File {
                metadata.len()
            } else {
                0
            },
            modified_millis: modified_millis(&metadata),
        });
        if kind != EntryKind::Directory || depth >= options.max_depth {
            return Ok(());
        }

        let verified = fs::canonicalize(native)?;
        self.ensure_contained(&verified)?;
        let mut children: Vec<_> = fs::read_dir(verified)?.collect::<Result<_, _>>()?;
        children.sort_unstable_by_key(|entry| entry.file_name());
        for child in children {
            self.walk_inner(&child.path(), depth + 1, options, output)?;
        }
        Ok(())
    }

    fn logical_from_native(&self, native: &Path) -> Result<String, PlatformError> {
        let relative = native
            .strip_prefix(&self.root)
            .map_err(|_| PlatformError::WorkspaceEscape)?;
        if relative.as_os_str().is_empty() {
            return Ok(".".to_owned());
        }
        let components: Result<Vec<_>, _> = relative
            .components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(PlatformError::NonUtf8Path)
            })
            .collect();
        Ok(components?.join("/"))
    }

    fn revalidate(&self, path: &ResolvedPath) -> Result<PathBuf, PlatformError> {
        let native = fs::canonicalize(&path.native)?;
        self.ensure_contained(&native)?;
        Ok(native)
    }

    fn ensure_contained(&self, native: &Path) -> Result<(), PlatformError> {
        if native.starts_with(&self.root) {
            Ok(())
        } else {
            Err(PlatformError::WorkspaceEscape)
        }
    }
}

fn validate_logical(logical: &str) -> Result<(), PlatformError> {
    if logical.is_empty()
        || logical.len() > 4096
        || logical.contains(['\0', '\\', ':'])
        || logical.starts_with('/')
        || logical.ends_with('/')
        || (logical != "."
            && logical
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == ".."))
    {
        Err(PlatformError::InvalidLogicalPath)
    } else {
        Ok(())
    }
}

fn classify(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}

fn modified_millis(metadata: &fs::Metadata) -> Option<i64> {
    let duration = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

#[cfg(unix)]
fn is_hidden(path: &Path, _metadata: &fs::Metadata) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

#[cfg(windows)]
fn is_hidden(path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{EntryKind, WalkOptions, Workspace};
    use crate::PlatformError;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-platform-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn workspace_reads_and_walks_in_stable_logical_order() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("src")).expect("mkdir");
        fs::write(directory.0.join("src").join("b.rs"), b"b").expect("write");
        fs::write(directory.0.join("src").join("a.rs"), b"a").expect("write");
        fs::write(directory.0.join(".hidden"), b"h").expect("write");
        let workspace = Workspace::new(&directory.0).expect("workspace");

        let file = workspace.resolve_existing("src/a.rs").expect("resolve");
        assert_eq!(workspace.read(&file).await.expect("read"), b"a");
        let root = workspace.resolve_existing(".").expect("root");
        let entries = workspace
            .walk(
                &root,
                WalkOptions {
                    max_depth: 2,
                    include_hidden: false,
                },
            )
            .expect("walk");
        let paths: Vec<_> = entries.iter().map(|entry| entry.logical.as_str()).collect();
        assert_eq!(paths, [".", "src", "src/a.rs", "src/b.rs"]);
        assert_eq!(entries[0].kind, EntryKind::Directory);
    }

    #[test]
    fn lexical_escape_and_noncanonical_separators_are_rejected() {
        let directory = TestDirectory::new();
        let workspace = Workspace::new(&directory.0).expect("workspace");
        for path in ["", "/tmp", "../outside", "a/../b", "a//b", "a\\b", "C:/x"] {
            assert!(matches!(
                workspace.resolve_existing(path),
                Err(PlatformError::InvalidLogicalPath)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_outside_the_workspace_are_rejected() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let outside = TestDirectory::new();
        fs::write(outside.0.join("secret"), b"secret").expect("write outside file");
        symlink(outside.0.join("secret"), directory.0.join("escape")).expect("symlink");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        assert!(matches!(
            workspace.resolve_existing("escape"),
            Err(PlatformError::WorkspaceEscape)
        ));
    }
}
