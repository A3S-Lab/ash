use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ash_engine::{CancellationToken, ComputePool, ParallelismError};
use ash_platform::{EntryKind, PlatformError, ResolvedPath, WalkOptions, Workspace};
use regex::{Regex, RegexBuilder};
use thiserror::Error;

/// Maximum bytes read from one file by the portable semantic read service.
pub const MAX_READ_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 1_000_000;
/// Maximum bytes scanned from one file by the portable semantic search service.
pub const MAX_SEARCH_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEARCH_MATCHES: usize = 1_000_000;
const MAX_MATCHES_PER_FILE: usize = 100_000;
const MAX_SEARCH_ENTRIES: usize = 1_000_000;

/// Typed failures produced before protocol projection or response encoding.
#[derive(Debug, Error)]
pub enum SemanticError {
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error(transparent)]
    Parallelism(#[from] ParallelismError),
    #[error(transparent)]
    Regex(#[from] regex::Error),
    #[error("semantic operation was cancelled")]
    Cancelled,
    #[error("semantic operation exceeded its bounded work ceiling")]
    WorkLimit,
}

/// A provider-owned path plus the byte key used for deterministic ordering.
///
/// Equal keys identify the same path for sorting and deduplication. Providers
/// must therefore use one collision-free key per path in their own namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticPath {
    path: PathBuf,
    stable_sort_key: Vec<u8>,
}

impl SemanticPath {
    /// Creates a path whose ordering is defined independently of host locale.
    #[must_use]
    pub fn new(path: PathBuf, stable_sort_key: Vec<u8>) -> Self {
        Self {
            path,
            stable_sort_key,
        }
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn stable_sort_key(&self) -> &[u8] {
        &self.stable_sort_key
    }
}

/// File kinds shared by listing and search providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Provider-neutral metadata returned by a bounded walk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticEntry {
    pub path: SemanticPath,
    pub kind: SemanticEntryKind,
    pub size: u64,
    pub modified_millis: Option<i64>,
}

/// Provider-neutral walk controls used by semantic services.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticWalkOptions {
    pub max_depth: u16,
    pub include_hidden: bool,
    pub max_entries: usize,
}

/// Synchronous filesystem primitives required by the portable semantic layer.
///
/// Implementations may represent workspace-confined logical paths or native
/// host paths. Paths returned from `walk` must be accepted by
/// `resolve_existing`. The associated resolved type is never exposed in
/// semantic results, so providers retain authority over revalidation.
pub trait SemanticFileSystem: Clone + Send + Sync + 'static {
    type ResolvedPath: Clone + Send + Sync + 'static;

    fn resolve_existing(&self, path: &Path) -> Result<Self::ResolvedPath, SemanticError>;

    fn semantic_path(&self, path: &Self::ResolvedPath) -> SemanticPath;

    fn read_limited(
        &self,
        path: &Self::ResolvedPath,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SemanticError>;

    fn walk(
        &self,
        root: &Self::ResolvedPath,
        options: SemanticWalkOptions,
    ) -> Result<Vec<SemanticEntry>, SemanticError>;
}

/// Unconfined native filesystem provider for human-facing portable commands.
///
/// Relative paths are resolved from the captured working directory. Access is
/// governed only by the operating-system identity running ash; this provider
/// does not apply the ASH/1 workspace capability used by machine operations.
#[derive(Clone, Debug)]
pub struct NativeFileSystem {
    cwd: PathBuf,
}

impl NativeFileSystem {
    pub fn new(cwd: impl AsRef<Path>) -> Result<Self, SemanticError> {
        let cwd = fs::canonicalize(cwd).map_err(PlatformError::from)?;
        if !fs::metadata(&cwd).map_err(PlatformError::from)?.is_dir() {
            return Err(PlatformError::InvalidWorkspace.into());
        }
        Ok(Self { cwd })
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

impl SemanticFileSystem for NativeFileSystem {
    type ResolvedPath = PathBuf;

    fn resolve_existing(&self, path: &Path) -> Result<Self::ResolvedPath, SemanticError> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            self.cwd.join(path)
        };
        Ok(fs::canonicalize(path).map_err(PlatformError::from)?)
    }

    fn semantic_path(&self, path: &Self::ResolvedPath) -> SemanticPath {
        SemanticPath::new(path.clone(), native_path_sort_key(path))
    }

    fn read_limited(
        &self,
        path: &Self::ResolvedPath,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SemanticError> {
        let size = fs::metadata(path).map_err(PlatformError::from)?.len();
        if size > max_bytes {
            return Err(PlatformError::InputTooLarge {
                size,
                max: max_bytes,
            }
            .into());
        }
        Ok(fs::read(path).map_err(PlatformError::from)?)
    }

    fn walk(
        &self,
        root: &Self::ResolvedPath,
        options: SemanticWalkOptions,
    ) -> Result<Vec<SemanticEntry>, SemanticError> {
        if options.max_entries == 0 {
            return Err(PlatformError::EntryLimit { max: 0 }.into());
        }
        let mut entries = Vec::new();
        native_walk_inner(root, 0, options, &mut entries)?;
        entries.sort_unstable_by(|left, right| {
            left.path.stable_sort_key.cmp(&right.path.stable_sort_key)
        });
        Ok(entries)
    }
}

fn native_walk_inner(
    path: &Path,
    depth: u16,
    options: SemanticWalkOptions,
    output: &mut Vec<SemanticEntry>,
) -> Result<(), PlatformError> {
    let metadata = fs::symlink_metadata(path)?;
    if depth > 0 && native_hidden(path, &metadata) && !options.include_hidden {
        return Ok(());
    }
    if output.len() >= options.max_entries {
        return Err(PlatformError::EntryLimit {
            max: options.max_entries,
        });
    }
    let kind = native_entry_kind(&metadata);
    output.push(SemanticEntry {
        path: SemanticPath::new(path.to_owned(), native_path_sort_key(path)),
        kind,
        size: if kind == SemanticEntryKind::File {
            metadata.len()
        } else {
            0
        },
        modified_millis: native_modified_millis(&metadata),
    });
    if kind != SemanticEntryKind::Directory || depth >= options.max_depth {
        return Ok(());
    }

    let mut children: Vec<_> = fs::read_dir(path)?.collect::<Result<_, _>>()?;
    children.sort_unstable_by(|left, right| {
        native_path_sort_key(&left.path()).cmp(&native_path_sort_key(&right.path()))
    });
    for child in children {
        native_walk_inner(&child.path(), depth + 1, options, output)?;
    }
    Ok(())
}

fn native_entry_kind(metadata: &fs::Metadata) -> SemanticEntryKind {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        SemanticEntryKind::File
    } else if file_type.is_dir() {
        SemanticEntryKind::Directory
    } else if file_type.is_symlink() {
        SemanticEntryKind::Symlink
    } else {
        SemanticEntryKind::Other
    }
}

fn native_modified_millis(metadata: &fs::Metadata) -> Option<i64> {
    let duration = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

#[cfg(unix)]
fn native_path_sort_key(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn native_path_sort_key(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_be_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn native_path_sort_key(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn native_hidden(path: &Path, _metadata: &fs::Metadata) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.file_name()
        .is_some_and(|name| name.as_bytes().starts_with(b"."))
}

#[cfg(windows)]
fn native_hidden(path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
        || path
            .file_name()
            .and_then(|name| name.encode_wide().next())
            .is_some_and(|unit| unit == u16::from(b'.'))
}

#[cfg(not(any(unix, windows)))]
fn native_hidden(path: &Path, _metadata: &fs::Metadata) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

impl SemanticFileSystem for Workspace {
    type ResolvedPath = ResolvedPath;

    fn resolve_existing(&self, path: &Path) -> Result<Self::ResolvedPath, SemanticError> {
        let logical = path.to_str().ok_or(PlatformError::NonUtf8Path)?;
        Ok(Workspace::resolve_existing(self, logical)?)
    }

    fn semantic_path(&self, path: &Self::ResolvedPath) -> SemanticPath {
        SemanticPath::new(
            PathBuf::from(path.logical()),
            path.logical().as_bytes().to_vec(),
        )
    }

    fn read_limited(
        &self,
        path: &Self::ResolvedPath,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SemanticError> {
        Ok(self.read_limited_sync(path, max_bytes)?)
    }

    fn walk(
        &self,
        root: &Self::ResolvedPath,
        options: SemanticWalkOptions,
    ) -> Result<Vec<SemanticEntry>, SemanticError> {
        let entries = Workspace::walk(
            self,
            root,
            WalkOptions {
                max_depth: options.max_depth,
                include_hidden: options.include_hidden,
                max_entries: options.max_entries,
            },
        )?;
        Ok(entries
            .into_iter()
            .map(|entry| SemanticEntry {
                path: SemanticPath::new(
                    PathBuf::from(&entry.logical),
                    entry.logical.as_bytes().to_vec(),
                ),
                kind: match entry.kind {
                    EntryKind::File => SemanticEntryKind::File,
                    EntryKind::Directory => SemanticEntryKind::Directory,
                    EntryKind::Symlink => SemanticEntryKind::Symlink,
                    EntryKind::Other => SemanticEntryKind::Other,
                },
                size: entry.size,
                modified_millis: entry.modified_millis,
            })
            .collect())
    }
}

/// Selects byte offsets or one-based line ranges for a semantic read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticReadMode {
    Bytes,
    Lines,
}

/// Protocol-independent input for one or more bounded reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadQuery {
    paths: Vec<PathBuf>,
    mode: SemanticReadMode,
    offset: u64,
    length: u64,
}

impl ReadQuery {
    #[must_use]
    pub fn new(paths: Vec<PathBuf>, mode: SemanticReadMode, offset: u64, length: u64) -> Self {
        Self {
            paths,
            mode,
            offset,
            length,
        }
    }

    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    #[must_use]
    pub const fn mode(&self) -> SemanticReadMode {
        self.mode
    }

    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }
}

/// One raw read record. The digest always covers the complete file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticRead {
    pub path: SemanticPath,
    pub digest: String,
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub length: u64,
}

/// Complete raw read output before any response projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticReadResult {
    pub reads: Vec<SemanticRead>,
}

/// Kind selection for a semantic list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticListFilter {
    All,
    Files,
    Directories,
}

/// Protocol-independent input for one or more bounded directory walks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListQuery {
    paths: Vec<PathBuf>,
    depth: u16,
    include_hidden: bool,
    filter: SemanticListFilter,
}

impl ListQuery {
    #[must_use]
    pub fn new(
        paths: Vec<PathBuf>,
        depth: u16,
        include_hidden: bool,
        filter: SemanticListFilter,
    ) -> Self {
        Self {
            paths,
            depth,
            include_hidden,
            filter,
        }
    }

    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    #[must_use]
    pub const fn depth(&self) -> u16 {
        self.depth
    }

    #[must_use]
    pub const fn include_hidden(&self) -> bool {
        self.include_hidden
    }

    #[must_use]
    pub const fn filter(&self) -> SemanticListFilter {
        self.filter
    }
}

/// Complete stable, deduplicated list output before response projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticListResult {
    pub entries: Vec<SemanticEntry>,
}

/// Pattern interpretation for a semantic search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticSearchPattern {
    Literal,
    Regex,
}

/// Protocol-independent input for a bounded text search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    query: String,
    paths: Vec<PathBuf>,
    pattern: SemanticSearchPattern,
    case_insensitive: bool,
    include_hidden: bool,
}

impl SearchQuery {
    #[must_use]
    pub fn new(
        query: String,
        paths: Vec<PathBuf>,
        pattern: SemanticSearchPattern,
        case_insensitive: bool,
        include_hidden: bool,
    ) -> Self {
        Self {
            query,
            paths,
            pattern,
            case_insensitive,
            include_hidden,
        }
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    #[must_use]
    pub const fn pattern(&self) -> SemanticSearchPattern {
        self.pattern
    }

    #[must_use]
    pub const fn case_insensitive(&self) -> bool {
        self.case_insensitive
    }

    #[must_use]
    pub const fn include_hidden(&self) -> bool {
        self.include_hidden
    }
}

/// One raw text match with one-based line and byte-column coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticSearchMatch {
    pub path: SemanticPath,
    pub line: u64,
    pub column: u64,
    pub text: String,
}

/// Complete raw search output plus truthful text-normalization flags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticSearchResult {
    pub matches: Vec<SemanticSearchMatch>,
    pub normalized_text: bool,
    pub partial: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SemanticLimits {
    max_read_file_bytes: u64,
    max_list_entries: usize,
    max_search_file_bytes: u64,
    max_search_matches: usize,
    max_matches_per_file: usize,
    max_search_entries: usize,
}

impl Default for SemanticLimits {
    fn default() -> Self {
        Self {
            max_read_file_bytes: MAX_READ_FILE_BYTES,
            max_list_entries: MAX_LIST_ENTRIES,
            max_search_file_bytes: MAX_SEARCH_FILE_BYTES,
            max_search_matches: MAX_SEARCH_MATCHES,
            max_matches_per_file: MAX_MATCHES_PER_FILE,
            max_search_entries: MAX_SEARCH_ENTRIES,
        }
    }
}

/// Reusable portable read, list, and search semantics over a filesystem provider.
///
/// Callers own governor permits and deadlines. These services use only the
/// supplied compute pool and cancellation token and return complete raw data;
/// they do not consume output budgets, retain evidence, or encode responses.
#[derive(Clone, Debug)]
pub struct SemanticServices<F> {
    filesystem: F,
    limits: SemanticLimits,
}

impl<F> SemanticServices<F>
where
    F: SemanticFileSystem,
{
    #[must_use]
    pub fn new(filesystem: F) -> Self {
        Self {
            filesystem,
            limits: SemanticLimits::default(),
        }
    }

    #[must_use]
    pub const fn filesystem(&self) -> &F {
        &self.filesystem
    }

    pub async fn read(
        &self,
        query: &ReadQuery,
        compute_pool: &ComputePool,
        cancellation: &CancellationToken,
    ) -> Result<SemanticReadResult, SemanticError> {
        check_cancelled(cancellation)?;
        let resolved = query
            .paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path))
            .collect::<Result<Vec<_>, _>>()?;
        let filesystem = self.filesystem.clone();
        let worker_cancellation = cancellation.clone();
        let mode = query.mode;
        let offset = query.offset;
        let length = query.length;
        let max_bytes = self.limits.max_read_file_bytes;
        let results = compute_pool
            .map_ordered_owned(resolved, move |path| {
                if worker_cancellation.is_cancelled() {
                    return Ok(None);
                }
                Ok::<_, SemanticError>(Some(read_resolved(
                    &filesystem,
                    path,
                    mode,
                    offset,
                    length,
                    max_bytes,
                )?))
            })
            .await?;
        let mut reads = Vec::with_capacity(results.len());
        for result in results {
            let Some(read) = result? else {
                return Err(SemanticError::Cancelled);
            };
            reads.push(read);
        }
        check_cancelled(cancellation)?;
        Ok(SemanticReadResult { reads })
    }

    /// Executes read semantics serially for the current synchronous human
    /// frontend. This retains the same provider bound, digest, and range
    /// selection as [`Self::read`] but has no compute-pool scheduling or
    /// built-in cancellation point.
    pub fn read_serial(&self, query: &ReadQuery) -> Result<SemanticReadResult, SemanticError> {
        self.read_serial_limited(query, self.limits.max_read_file_bytes)
    }

    /// Executes a serial read with a caller-selected per-file ceiling no larger
    /// than the semantic service ceiling.
    pub fn read_serial_limited(
        &self,
        query: &ReadQuery,
        max_bytes: u64,
    ) -> Result<SemanticReadResult, SemanticError> {
        let resolved = query
            .paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path))
            .collect::<Result<Vec<_>, _>>()?;
        let max_bytes = max_bytes.min(self.limits.max_read_file_bytes);
        let reads = resolved
            .iter()
            .map(|path| {
                read_resolved(
                    &self.filesystem,
                    path,
                    query.mode,
                    query.offset,
                    query.length,
                    max_bytes,
                )
            })
            .collect::<Result<_, _>>()?;
        Ok(SemanticReadResult { reads })
    }

    pub async fn list(
        &self,
        query: &ListQuery,
        compute_pool: &ComputePool,
        cancellation: &CancellationToken,
    ) -> Result<SemanticListResult, SemanticError> {
        check_cancelled(cancellation)?;
        let roots = query
            .paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path))
            .collect::<Result<Vec<_>, _>>()?;
        let filesystem = self.filesystem.clone();
        let options = SemanticWalkOptions {
            max_depth: query.depth,
            include_hidden: query.include_hidden,
            max_entries: self.limits.max_list_entries,
        };
        let batches = compute_pool
            .map_ordered_owned(roots, move |root| filesystem.walk(root, options))
            .await?;
        let mut entries = Vec::new();
        for batch in batches {
            extend_list_entries(&mut entries, batch?, self.limits.max_list_entries)?;
        }
        check_cancelled(cancellation)?;
        Ok(finish_list(entries, query.filter))
    }

    /// Executes list semantics serially for the current synchronous human
    /// frontend. This retains the same provider bounds, filtering, ordering,
    /// and deduplication as [`Self::list`] but has no compute-pool scheduling or
    /// built-in cancellation point.
    pub fn list_serial(&self, query: &ListQuery) -> Result<SemanticListResult, SemanticError> {
        let roots = query
            .paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path))
            .collect::<Result<Vec<_>, _>>()?;
        let options = SemanticWalkOptions {
            max_depth: query.depth,
            include_hidden: query.include_hidden,
            max_entries: self.limits.max_list_entries,
        };
        let mut entries = Vec::new();
        for root in roots {
            let batch = self.filesystem.walk(&root, options)?;
            extend_list_entries(&mut entries, batch, self.limits.max_list_entries)?;
        }
        Ok(finish_list(entries, query.filter))
    }

    pub async fn search(
        &self,
        query: &SearchQuery,
        compute_pool: &ComputePool,
        cancellation: &CancellationToken,
    ) -> Result<SemanticSearchResult, SemanticError> {
        check_cancelled(cancellation)?;
        let matcher = Matcher::new(query)?;
        let roots = query
            .paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path))
            .collect::<Result<Vec<_>, _>>()?;
        let filesystem_for_walk = self.filesystem.clone();
        let options = SemanticWalkOptions {
            max_depth: 64,
            include_hidden: query.include_hidden,
            max_entries: self.limits.max_search_entries,
        };
        let batches = compute_pool
            .map_ordered_owned(roots, move |root| filesystem_for_walk.walk(root, options))
            .await?;
        let mut paths = Vec::new();
        for batch in batches {
            paths.extend(
                batch?
                    .into_iter()
                    .filter(|entry| entry.kind == SemanticEntryKind::File)
                    .map(|entry| entry.path),
            );
        }
        sort_deduplicate_search_paths(&mut paths);
        check_cancelled(cancellation)?;

        let resolved = paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path.as_path()))
            .collect::<Result<Vec<_>, _>>()?;
        let filesystem = self.filesystem.clone();
        let worker_cancellation = cancellation.clone();
        let max_file_bytes = self.limits.max_search_file_bytes;
        let max_matches_per_file = self.limits.max_matches_per_file;
        let scanned = compute_pool
            .map_ordered_owned(resolved, move |path| {
                if worker_cancellation.is_cancelled() {
                    return Ok(FileMatches::cancelled());
                }
                let bytes = filesystem.read_limited(path, max_file_bytes)?;
                Ok::<_, SemanticError>(scan_file(
                    filesystem.semantic_path(path),
                    &bytes,
                    &matcher,
                    max_matches_per_file,
                ))
            })
            .await?;
        let result = finish_search(scanned, self.limits.max_search_matches)?;
        check_cancelled(cancellation)?;
        Ok(result)
    }

    /// Executes search semantics serially for the current synchronous human
    /// frontend. This retains the same provider bounds, matching, text flags,
    /// ordering, and deduplication as [`Self::search`] but has no compute-pool
    /// scheduling or built-in cancellation point.
    pub fn search_serial(
        &self,
        query: &SearchQuery,
    ) -> Result<SemanticSearchResult, SemanticError> {
        let matcher = Matcher::new(query)?;
        let roots = query
            .paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path))
            .collect::<Result<Vec<_>, _>>()?;
        let options = SemanticWalkOptions {
            max_depth: 64,
            include_hidden: query.include_hidden,
            max_entries: self.limits.max_search_entries,
        };
        let mut paths = Vec::new();
        for root in roots {
            paths.extend(
                self.filesystem
                    .walk(&root, options)?
                    .into_iter()
                    .filter(|entry| entry.kind == SemanticEntryKind::File)
                    .map(|entry| entry.path),
            );
        }
        sort_deduplicate_search_paths(&mut paths);

        let resolved = paths
            .iter()
            .map(|path| self.filesystem.resolve_existing(path.as_path()))
            .collect::<Result<Vec<_>, _>>()?;
        let scanned = resolved.iter().map(|path| {
            let bytes = self
                .filesystem
                .read_limited(path, self.limits.max_search_file_bytes)?;
            Ok(scan_file(
                self.filesystem.semantic_path(path),
                &bytes,
                &matcher,
                self.limits.max_matches_per_file,
            ))
        });
        finish_search(scanned, self.limits.max_search_matches)
    }
}

fn read_resolved<F>(
    filesystem: &F,
    path: &F::ResolvedPath,
    mode: SemanticReadMode,
    offset: u64,
    length: u64,
    max_bytes: u64,
) -> Result<SemanticRead, SemanticError>
where
    F: SemanticFileSystem,
{
    let bytes = filesystem.read_limited(path, max_bytes)?;
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let (slice, actual_offset, actual_length) = select_range(&bytes, mode, offset, length);
    let selected = if mode == SemanticReadMode::Bytes
        && actual_offset == 0
        && actual_length == bytes.len() as u64
    {
        bytes
    } else {
        slice.to_vec()
    };
    Ok(SemanticRead {
        path: filesystem.semantic_path(path),
        digest,
        bytes: selected,
        offset: actual_offset,
        length: actual_length,
    })
}

fn extend_list_entries(
    entries: &mut Vec<SemanticEntry>,
    batch: Vec<SemanticEntry>,
    max_entries: usize,
) -> Result<(), SemanticError> {
    entries.extend(batch);
    if entries.len() > max_entries {
        Err(SemanticError::WorkLimit)
    } else {
        Ok(())
    }
}

fn finish_list(mut entries: Vec<SemanticEntry>, filter: SemanticListFilter) -> SemanticListResult {
    entries.retain(|entry| selected(entry, filter));
    entries
        .sort_unstable_by(|left, right| left.path.stable_sort_key.cmp(&right.path.stable_sort_key));
    entries.dedup_by(|left, right| left.path.stable_sort_key == right.path.stable_sort_key);
    SemanticListResult { entries }
}

fn selected(entry: &SemanticEntry, filter: SemanticListFilter) -> bool {
    match filter {
        SemanticListFilter::All => true,
        SemanticListFilter::Files => entry.kind == SemanticEntryKind::File,
        SemanticListFilter::Directories => entry.kind == SemanticEntryKind::Directory,
    }
}

fn sort_deduplicate_search_paths(paths: &mut Vec<SemanticPath>) {
    paths.sort_unstable_by(|left, right| left.stable_sort_key.cmp(&right.stable_sort_key));
    paths.dedup_by(|left, right| left.stable_sort_key == right.stable_sort_key);
}

fn finish_search(
    scanned: impl IntoIterator<Item = Result<FileMatches, SemanticError>>,
    max_matches: usize,
) -> Result<SemanticSearchResult, SemanticError> {
    let mut matches = Vec::new();
    let mut normalized_text = false;
    let mut partial = false;
    for result in scanned {
        let result = result?;
        if result.cancelled {
            return Err(SemanticError::Cancelled);
        }
        if result.overflowed {
            return Err(SemanticError::WorkLimit);
        }
        normalized_text |= result.normalized;
        partial |= result.binary;
        matches.extend(result.matches);
        if matches.len() > max_matches {
            return Err(SemanticError::WorkLimit);
        }
    }
    matches.sort_unstable_by(|left, right| {
        left.path
            .stable_sort_key
            .cmp(&right.path.stable_sort_key)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
    });
    matches.dedup();
    Ok(SemanticSearchResult {
        matches,
        normalized_text,
        partial,
    })
}

fn select_range(
    bytes: &[u8],
    mode: SemanticReadMode,
    offset: u64,
    length: u64,
) -> (&[u8], u64, u64) {
    match mode {
        SemanticReadMode::Bytes => {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let requested = usize::try_from(length).unwrap_or(usize::MAX);
            let end = start.saturating_add(requested).min(bytes.len());
            (&bytes[start..end], start as u64, (end - start) as u64)
        }
        SemanticReadMode::Lines => select_lines(bytes, offset, length),
    }
}

fn select_lines(bytes: &[u8], offset: u64, length: u64) -> (&[u8], u64, u64) {
    if bytes.is_empty() {
        return (&[], offset, 0);
    }
    let mut starts = vec![0_usize];
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && index + 1 < bytes.len() {
            starts.push(index + 1);
        }
    }
    let requested_start = usize::try_from(offset.saturating_sub(1)).unwrap_or(usize::MAX);
    if requested_start >= starts.len() {
        return (&[], offset, 0);
    }
    let count = usize::try_from(length).unwrap_or(usize::MAX);
    let end_line = requested_start.saturating_add(count).min(starts.len());
    let start_byte = starts[requested_start];
    let end_byte = starts.get(end_line).copied().unwrap_or(bytes.len());
    (
        &bytes[start_byte..end_byte],
        requested_start as u64 + 1,
        (end_line - requested_start) as u64,
    )
}

#[derive(Clone)]
enum Matcher {
    Literal(String),
    Regex(Regex),
}

impl Matcher {
    fn new(query: &SearchQuery) -> Result<Self, SemanticError> {
        if query.pattern == SemanticSearchPattern::Regex || query.case_insensitive {
            let pattern = if query.pattern == SemanticSearchPattern::Regex {
                query.query.clone()
            } else {
                regex::escape(&query.query)
            };
            Ok(Self::Regex(
                RegexBuilder::new(&pattern)
                    .case_insensitive(query.case_insensitive)
                    .size_limit(16 * 1024 * 1024)
                    .build()?,
            ))
        } else {
            Ok(Self::Literal(query.query.clone()))
        }
    }

    fn find(&self, line: &str) -> Option<usize> {
        match self {
            Self::Literal(query) => line.find(query),
            Self::Regex(regex) => regex.find(line).map(|matched| matched.start()),
        }
    }
}

struct FileMatches {
    matches: Vec<SemanticSearchMatch>,
    normalized: bool,
    binary: bool,
    overflowed: bool,
    cancelled: bool,
}

impl FileMatches {
    const fn cancelled() -> Self {
        Self {
            matches: Vec::new(),
            normalized: false,
            binary: false,
            overflowed: false,
            cancelled: true,
        }
    }
}

fn scan_file(
    path: SemanticPath,
    bytes: &[u8],
    matcher: &Matcher,
    max_matches: usize,
) -> FileMatches {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return FileMatches {
            matches: Vec::new(),
            normalized: false,
            binary: true,
            overflowed: false,
            cancelled: false,
        };
    };
    let mut matches = Vec::new();
    let mut normalized = false;
    for (index, raw_line) in text.split_terminator('\n').enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        normalized |= line.len() != raw_line.len();
        if let Some(column) = matcher.find(line) {
            matches.push(SemanticSearchMatch {
                path: path.clone(),
                line: index as u64 + 1,
                column: column as u64 + 1,
                text: line.to_owned(),
            });
            if matches.len() > max_matches {
                return FileMatches {
                    matches,
                    normalized,
                    binary: false,
                    overflowed: true,
                    cancelled: false,
                };
            }
        }
    }
    FileMatches {
        matches,
        normalized,
        binary: false,
        overflowed: false,
        cancelled: false,
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), SemanticError> {
    if cancellation.is_cancelled() {
        Err(SemanticError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use ash_engine::{CancellationToken, ComputePool, Parallelism};
    use ash_platform::{PlatformError, Workspace};

    use super::{
        ListQuery, NativeFileSystem, ReadQuery, SearchQuery, SemanticEntryKind, SemanticError,
        SemanticLimits, SemanticListFilter, SemanticReadMode, SemanticSearchPattern,
        SemanticServices,
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ash-semantic-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create semantic test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn pool() -> ComputePool {
        ComputePool::new(Parallelism::for_available_cpus(4)).expect("compute pool")
    }

    #[tokio::test]
    async fn raw_reads_select_byte_and_line_ranges_from_one_full_digest() {
        let directory = TestDirectory::new();
        let contents = b"one\ntwo\nthree";
        fs::write(directory.0.join("sample.txt"), contents).expect("write sample");
        let services = SemanticServices::new(Workspace::new(&directory.0).expect("workspace"));
        let pool = pool();
        let cancellation = CancellationToken::default();

        let bytes = services
            .read(
                &ReadQuery::new(
                    vec![PathBuf::from("sample.txt")],
                    SemanticReadMode::Bytes,
                    4,
                    3,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect("byte read");
        let lines = services
            .read(
                &ReadQuery::new(
                    vec![PathBuf::from("sample.txt")],
                    SemanticReadMode::Lines,
                    2,
                    2,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect("line read");
        let bounded = services
            .read(
                &ReadQuery::new(
                    vec![PathBuf::from("sample.txt")],
                    SemanticReadMode::Bytes,
                    u64::MAX,
                    u64::MAX,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect("bounded read");
        let serial = services
            .read_serial(&ReadQuery::new(
                vec![PathBuf::from("sample.txt")],
                SemanticReadMode::Bytes,
                0,
                u64::MAX,
            ))
            .expect("serial read");

        assert_eq!(bytes.reads[0].bytes, b"two");
        assert_eq!((bytes.reads[0].offset, bytes.reads[0].length), (4, 3));
        assert_eq!(lines.reads[0].bytes, b"two\nthree");
        assert_eq!((lines.reads[0].offset, lines.reads[0].length), (2, 2));
        assert!(bounded.reads[0].bytes.is_empty());
        assert_eq!(
            (bounded.reads[0].offset, bounded.reads[0].length),
            (contents.len() as u64, 0)
        );
        assert_eq!(bytes.reads[0].digest, lines.reads[0].digest);
        assert_eq!(serial.reads[0].bytes, contents);
        assert_eq!(serial.reads[0].digest, bytes.reads[0].digest);
        assert_eq!(
            bytes.reads[0].digest,
            blake3::hash(contents).to_hex().to_string()
        );
    }

    #[tokio::test]
    async fn raw_lists_filter_sort_and_deduplicate_overlapping_roots() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("src")).expect("mkdir");
        fs::write(directory.0.join("src/b.rs"), b"b").expect("write b");
        fs::write(directory.0.join("src/a.rs"), b"a").expect("write a");
        fs::write(directory.0.join("src/.hidden"), b"h").expect("write hidden");
        let services = SemanticServices::new(Workspace::new(&directory.0).expect("workspace"));
        let pool = pool();

        let result = services
            .list(
                &ListQuery::new(
                    vec![PathBuf::from("src"), PathBuf::from("src")],
                    1,
                    false,
                    SemanticListFilter::Files,
                ),
                &pool,
                &CancellationToken::default(),
            )
            .await
            .expect("list");
        let paths: Vec<_> = result
            .entries
            .iter()
            .map(|entry| entry.path.as_path())
            .collect();
        assert_eq!(
            paths,
            [PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]
        );

        let with_hidden = services
            .list(
                &ListQuery::new(
                    vec![PathBuf::from("src")],
                    1,
                    true,
                    SemanticListFilter::Files,
                ),
                &pool,
                &CancellationToken::default(),
            )
            .await
            .expect("list hidden");
        let paths: Vec<_> = with_hidden
            .entries
            .iter()
            .map(|entry| entry.path.as_path())
            .collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("src/.hidden"),
                PathBuf::from("src/a.rs"),
                PathBuf::from("src/b.rs"),
            ]
        );
    }

    #[test]
    fn serial_native_lists_resolve_relative_paths_and_bound_depth() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("nested")).expect("mkdir nested");
        fs::write(directory.0.join("b.txt"), b"b").expect("write b");
        fs::write(directory.0.join("a.txt"), b"a").expect("write a");
        fs::write(directory.0.join(".hidden"), b"h").expect("write hidden");
        fs::write(directory.0.join("nested/deep.txt"), b"deep").expect("write deep");
        let root = fs::canonicalize(&directory.0).expect("canonical root");
        let services =
            SemanticServices::new(NativeFileSystem::new(&directory.0).expect("native filesystem"));

        let visible = services
            .list_serial(&ListQuery::new(
                vec![PathBuf::from(".")],
                1,
                false,
                SemanticListFilter::All,
            ))
            .expect("visible list");
        let relative: Vec<_> = visible
            .entries
            .iter()
            .map(|entry| {
                entry
                    .path
                    .as_path()
                    .strip_prefix(&root)
                    .expect("entry below root")
                    .to_owned()
            })
            .collect();
        assert_eq!(
            relative,
            [
                PathBuf::new(),
                PathBuf::from("a.txt"),
                PathBuf::from("b.txt"),
                PathBuf::from("nested"),
            ]
        );
        assert_eq!(visible.entries[0].kind, SemanticEntryKind::Directory);

        let all = services
            .list_serial(&ListQuery::new(
                vec![PathBuf::from(".")],
                1,
                true,
                SemanticListFilter::All,
            ))
            .expect("all list");
        assert!(
            all.entries
                .iter()
                .any(|entry| entry.path.as_path().ends_with(".hidden"))
        );
        assert!(
            all.entries
                .iter()
                .all(|entry| !entry.path.as_path().ends_with("deep.txt"))
        );
    }

    #[tokio::test]
    async fn serial_and_parallel_lists_share_postprocessing() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("src")).expect("mkdir src");
        fs::write(directory.0.join("src/b.rs"), b"b").expect("write b");
        fs::write(directory.0.join("src/a.rs"), b"a").expect("write a");
        let services = SemanticServices::new(Workspace::new(&directory.0).expect("workspace"));
        let query = ListQuery::new(
            vec![PathBuf::from("src"), PathBuf::from("src")],
            1,
            false,
            SemanticListFilter::Files,
        );

        let serial = services.list_serial(&query).expect("serial list");
        let parallel = services
            .list(&query, &pool(), &CancellationToken::default())
            .await
            .expect("parallel list");

        assert_eq!(serial, parallel);
    }

    #[cfg(unix)]
    #[test]
    fn native_walk_records_symlinks_without_recursing_through_them() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("target")).expect("mkdir target");
        fs::write(directory.0.join("target/nested.txt"), b"nested").expect("write nested");
        symlink("target", directory.0.join("link")).expect("create symlink");
        let services =
            SemanticServices::new(NativeFileSystem::new(&directory.0).expect("native filesystem"));

        let result = services
            .list_serial(&ListQuery::new(
                vec![PathBuf::from(".")],
                2,
                false,
                SemanticListFilter::All,
            ))
            .expect("list symlink");

        assert!(result.entries.iter().any(|entry| {
            entry.path.as_path().ends_with("link") && entry.kind == SemanticEntryKind::Symlink
        }));
        assert!(
            result
                .entries
                .iter()
                .all(|entry| !entry.path.as_path().ends_with("link/nested.txt"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_sort_keys_preserve_unix_path_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let bytes = b"native-\xff-name".to_vec();
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes.clone()));
        assert_eq!(super::native_path_sort_key(&path), bytes);
    }

    #[cfg(windows)]
    #[test]
    fn native_sort_keys_preserve_windows_code_units_without_collisions() {
        use std::os::windows::ffi::OsStringExt;

        let units = [0x0061, 0xd800, 0x0100];
        let path = PathBuf::from(std::ffi::OsString::from_wide(&units));
        assert_eq!(
            super::native_path_sort_key(&path),
            [0x00, 0x61, 0xd8, 0x00, 0x01, 0x00]
        );
    }

    #[tokio::test]
    async fn raw_search_reports_crlf_normalization_binary_partiality_and_regex_matches() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("src")).expect("mkdir");
        fs::write(directory.0.join("src/a.txt"), b"zero\r\nNeedle alpha\r\n").expect("write a");
        fs::write(directory.0.join("src/b.txt"), b"needle beta\n").expect("write b");
        fs::write(directory.0.join("src/c.bin"), [0xff, b'n', b'e']).expect("write binary");
        let services = SemanticServices::new(Workspace::new(&directory.0).expect("workspace"));
        let pool = pool();
        let cancellation = CancellationToken::default();

        let literal = services
            .search(
                &SearchQuery::new(
                    "needle".to_owned(),
                    vec![PathBuf::from("src"), PathBuf::from("src")],
                    SemanticSearchPattern::Literal,
                    true,
                    false,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect("literal search");
        assert!(literal.normalized_text);
        assert!(literal.partial);
        assert_eq!(literal.matches.len(), 2);
        assert_eq!(
            literal.matches[0].path.as_path(),
            PathBuf::from("src/a.txt")
        );
        assert_eq!((literal.matches[0].line, literal.matches[0].column), (2, 1));
        assert_eq!(literal.matches[0].text, "Needle alpha");
        assert_eq!(
            literal.matches[1].path.as_path(),
            PathBuf::from("src/b.txt")
        );
        assert_eq!(literal.matches[1].text, "needle beta");
        let serial = services
            .search_serial(&SearchQuery::new(
                "needle".to_owned(),
                vec![PathBuf::from("src"), PathBuf::from("src")],
                SemanticSearchPattern::Literal,
                true,
                false,
            ))
            .expect("serial literal search");
        assert_eq!(serial, literal);

        let regex = services
            .search(
                &SearchQuery::new(
                    "alpha$".to_owned(),
                    vec![PathBuf::from("src")],
                    SemanticSearchPattern::Regex,
                    false,
                    false,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect("regex search");
        assert_eq!(regex.matches.len(), 1);
        assert_eq!(regex.matches[0].text, "Needle alpha");
    }

    #[tokio::test]
    async fn semantic_services_observe_cancellation_without_protocol_state() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("sample.txt"), b"sample").expect("write sample");
        let services = SemanticServices::new(Workspace::new(&directory.0).expect("workspace"));
        let cancellation = CancellationToken::default();
        assert!(cancellation.cancel());

        let error = services
            .read(
                &ReadQuery::new(
                    vec![PathBuf::from("sample.txt")],
                    SemanticReadMode::Bytes,
                    0,
                    6,
                ),
                &pool(),
                &cancellation,
            )
            .await
            .expect_err("cancelled read");
        assert!(matches!(error, SemanticError::Cancelled));
    }

    #[tokio::test]
    async fn semantic_limits_bound_files_walks_and_matches() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("src")).expect("mkdir");
        fs::write(directory.0.join("src/a.txt"), b"needle\nneedle\n").expect("write a");
        fs::write(directory.0.join("src/b.txt"), b"b").expect("write b");
        let workspace = Workspace::new(&directory.0).expect("workspace");
        let pool = pool();
        let cancellation = CancellationToken::default();

        let services = SemanticServices {
            filesystem: workspace.clone(),
            limits: SemanticLimits {
                max_read_file_bytes: 2,
                ..SemanticLimits::default()
            },
        };
        let error = services
            .read(
                &ReadQuery::new(
                    vec![PathBuf::from("src/a.txt")],
                    SemanticReadMode::Bytes,
                    0,
                    1,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect_err("oversized read");
        assert!(matches!(
            error,
            SemanticError::Platform(PlatformError::InputTooLarge { .. })
        ));
        let error = services
            .read_serial(&ReadQuery::new(
                vec![PathBuf::from("src/a.txt")],
                SemanticReadMode::Bytes,
                0,
                1,
            ))
            .expect_err("oversized serial read");
        assert!(matches!(
            error,
            SemanticError::Platform(PlatformError::InputTooLarge { .. })
        ));
        let error = services
            .read_serial_limited(
                &ReadQuery::new(
                    vec![PathBuf::from("src/b.txt")],
                    SemanticReadMode::Bytes,
                    0,
                    1,
                ),
                0,
            )
            .expect_err("caller-limited serial read");
        assert!(matches!(
            error,
            SemanticError::Platform(PlatformError::InputTooLarge { size: 1, max: 0 })
        ));

        let services = SemanticServices {
            filesystem: workspace.clone(),
            limits: SemanticLimits {
                max_list_entries: 3,
                ..SemanticLimits::default()
            },
        };
        let error = services
            .list(
                &ListQuery::new(
                    vec![PathBuf::from("src"), PathBuf::from("src")],
                    1,
                    false,
                    SemanticListFilter::All,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect_err("aggregate list limit");
        assert!(matches!(error, SemanticError::WorkLimit));
        let error = services
            .list_serial(&ListQuery::new(
                vec![PathBuf::from("src"), PathBuf::from("src")],
                1,
                false,
                SemanticListFilter::All,
            ))
            .expect_err("serial aggregate list limit");
        assert!(matches!(error, SemanticError::WorkLimit));

        let services = SemanticServices {
            filesystem: workspace,
            limits: SemanticLimits {
                max_matches_per_file: 1,
                ..SemanticLimits::default()
            },
        };
        let error = services
            .search(
                &SearchQuery::new(
                    "needle".to_owned(),
                    vec![PathBuf::from("src")],
                    SemanticSearchPattern::Literal,
                    false,
                    false,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect_err("per-file match limit");
        assert!(matches!(error, SemanticError::WorkLimit));
        let error = services
            .search_serial(&SearchQuery::new(
                "needle".to_owned(),
                vec![PathBuf::from("src")],
                SemanticSearchPattern::Literal,
                false,
                false,
            ))
            .expect_err("serial per-file match limit");
        assert!(matches!(error, SemanticError::WorkLimit));

        let error = services
            .search(
                &SearchQuery::new(
                    "[".to_owned(),
                    vec![PathBuf::from("src")],
                    SemanticSearchPattern::Regex,
                    false,
                    false,
                ),
                &pool,
                &cancellation,
            )
            .await
            .expect_err("invalid regex");
        assert!(matches!(error, SemanticError::Regex(_)));
        let error = services
            .search_serial(&SearchQuery::new(
                "[".to_owned(),
                vec![PathBuf::from("src")],
                SemanticSearchPattern::Regex,
                false,
                false,
            ))
            .expect_err("invalid serial regex");
        assert!(matches!(error, SemanticError::Regex(_)));
    }
}
