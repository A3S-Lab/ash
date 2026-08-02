use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::io::{Cursor, Read, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rayon::prelude::*;
use tempfile::{NamedTempFile, TempDir, TempPath};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Maximum complete stream kept in memory before capture changes to a
/// session-private disk spool.
pub const DEFAULT_CAPTURE_MEMORY_BYTES: usize = 4 * 1024 * 1024;

/// Full immutable BLAKE3 identity used internally for retained content.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentId([u8; 32]);

impl ContentId {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Hard session-local store ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreLimits {
    pub max_bytes: u64,
    pub max_entries: usize,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024 * 1024,
            max_entries: 4096,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreUsage {
    pub bytes: u64,
    pub entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreResidency {
    Memory,
    Disk,
}

/// Thread-safe immutable content store. Numeric aliases are never reused.
pub struct ResultStore {
    limits: StoreLimits,
    state: Mutex<State>,
    spool_root: Arc<SpoolRoot>,
}

/// In-flight retained-byte reservation owned by one capture stream.
///
/// Reservations count against the session quota before bytes become visible
/// through an alias. Dropping an uncommitted reservation releases its charge.
struct StoreReservation {
    store: Arc<ResultStore>,
    bytes: u64,
}

/// Mutable stream capture which spills once its bounded memory head is full.
pub struct StoreCapture {
    reservation: StoreReservation,
    memory_limit: u64,
    length: u64,
    state: CaptureState,
}

/// Completed, immutable stream which can be committed atomically with peers.
pub struct CapturedContent {
    pending: PendingContent,
    sample: Option<CaptureSample>,
}

/// A lease over immutable retained content.
///
/// Holding a lease prevents explicit release from unlinking a disk spool.
#[derive(Clone)]
pub struct ResultLease {
    content: Arc<Content>,
}

pub enum CapturedView<'a> {
    Complete(&'a [u8]),
    Sampled {
        head: &'a [u8],
        head_next: Option<u8>,
        tail: &'a [u8],
    },
}

struct State {
    next_alias: u64,
    bytes: u64,
    pending_bytes: u64,
    by_alias: BTreeMap<u64, Entry>,
    by_content: HashMap<ContentId, u64>,
}

struct Entry {
    content_id: ContentId,
    content: Arc<Content>,
}

struct Content {
    length: u64,
    storage: Storage,
}

enum Storage {
    Memory(Arc<[u8]>),
    Disk {
        path: TempPath,
        _root: Arc<SpoolRoot>,
    },
}

struct SpoolRoot {
    directory: TempDir,
}

struct PendingContent {
    content_id: Option<ContentId>,
    content: Option<Content>,
    reservation: Option<StoreReservation>,
}

enum CaptureState {
    Memory(Vec<u8>),
    Disk {
        file: tokio::fs::File,
        path: TempPath,
        root: Arc<SpoolRoot>,
        sample: CaptureSample,
    },
}

struct CaptureSample {
    head: Vec<u8>,
    head_next: Option<u8>,
    tail: VecDeque<u8>,
    head_limit: usize,
    tail_limit: usize,
}

impl ResultStore {
    pub fn new(limits: StoreLimits) -> Result<Self, StoreError> {
        if limits.max_bytes == 0 || limits.max_entries == 0 {
            return Err(StoreError::InvalidLimits);
        }
        let spool_root = Arc::new(SpoolRoot::new()?);
        Ok(Self {
            limits,
            state: Mutex::new(State {
                next_alias: 1,
                bytes: 0,
                pending_bytes: 0,
                by_alias: BTreeMap::new(),
                by_content: HashMap::new(),
            }),
            spool_root,
        })
    }

    /// Starts an empty stream capture with a caller-selected memory ceiling.
    ///
    /// The ceiling is clamped to the session byte quota. A zero ceiling spills
    /// the first non-empty chunk immediately.
    #[must_use]
    pub fn capture(self: &Arc<Self>, memory_limit: usize) -> StoreCapture {
        let memory_limit = u64::try_from(memory_limit)
            .unwrap_or(u64::MAX)
            .min(self.limits.max_bytes);
        StoreCapture {
            reservation: StoreReservation {
                store: Arc::clone(self),
                bytes: 0,
            },
            memory_limit,
            length: 0,
            state: CaptureState::Memory(Vec::new()),
        }
    }

    /// Retains immutable bytes and returns their session alias.
    ///
    /// Identical content is deduplicated and returns the existing alias without
    /// consuming more quota. Hash collisions fail closed.
    pub fn retain(&self, bytes: Vec<u8>) -> Result<u64, StoreError> {
        self.retain_many(vec![bytes])?
            .into_iter()
            .next()
            .ok_or(StoreError::Invariant)
    }

    /// Atomically retains a stable input-ordered group of immutable values.
    ///
    /// Quotas, alias exhaustion, collisions, and duplicate content are fully
    /// validated before the store changes. First occurrences receive aliases
    /// in input order; duplicates reuse the same alias.
    pub fn retain_many(&self, contents: Vec<Vec<u8>>) -> Result<Vec<u64>, StoreError> {
        let mut pending = contents
            .into_iter()
            .map(|bytes| self.prepare_bytes(bytes))
            .collect::<Result<Vec<_>, _>>()?;
        self.retain_pending(&mut pending)
    }

    /// Atomically commits completed stream captures in input order.
    pub fn retain_captures(&self, captures: Vec<CapturedContent>) -> Result<Vec<u64>, StoreError> {
        let mut pending = captures
            .into_iter()
            .map(|capture| capture.pending)
            .collect::<Vec<_>>();
        self.retain_pending(&mut pending)
    }

    fn prepare_bytes(&self, bytes: Vec<u8>) -> Result<PendingContent, StoreError> {
        let length = u64::try_from(bytes.len()).map_err(|_| StoreError::ContentTooLarge)?;
        let content_id = ContentId(*blake3::hash(&bytes).as_bytes());
        let storage = if bytes.len() <= DEFAULT_CAPTURE_MEMORY_BYTES {
            Storage::Memory(Arc::from(bytes))
        } else {
            let mut file = create_spool_file(self.spool_root.path())?;
            file.write_all(&bytes).map_err(|_| StoreError::Io)?;
            file.flush().map_err(|_| StoreError::Io)?;
            let (_, path) = file.into_parts();
            Storage::Disk {
                path,
                _root: Arc::clone(&self.spool_root),
            }
        };
        Ok(PendingContent {
            content_id: Some(content_id),
            content: Some(Content { length, storage }),
            reservation: None,
        })
    }

    fn retain_pending(&self, pending: &mut [PendingContent]) -> Result<Vec<u64>, StoreError> {
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        for value in pending.iter() {
            let content = value.content.as_ref().ok_or(StoreError::Invariant)?;
            if let Some(reservation) = &value.reservation
                && (!std::ptr::eq(Arc::as_ptr(&reservation.store), self)
                    || reservation.bytes > content.length)
            {
                return Err(StoreError::ReservationMismatch);
            }
        }
        pending.par_iter_mut().try_for_each(|value| {
            if value.content_id.is_none() {
                let content = value.content.as_ref().ok_or(StoreError::Invariant)?;
                value.content_id = Some(content.digest()?);
            }
            Ok::<(), StoreError>(())
        })?;
        let content_ids = pending
            .iter()
            .map(|value| value.content_id.ok_or(StoreError::Invariant))
            .collect::<Result<Vec<_>, _>>()?;
        let mut state = self.lock()?;
        let reserved = pending.iter().try_fold(0_u64, |total, value| {
            total
                .checked_add(
                    value
                        .reservation
                        .as_ref()
                        .map_or(0, StoreReservation::bytes),
                )
                .ok_or(StoreError::ContentTooLarge)
        })?;
        let pending_after = state
            .pending_bytes
            .checked_sub(reserved)
            .ok_or(StoreError::Invariant)?;

        enum Placement {
            Existing(u64),
            New(usize),
        }

        let mut placements = Vec::with_capacity(pending.len());
        let mut first_new: HashMap<ContentId, usize> = HashMap::new();
        let mut incoming = 0_u64;
        for (index, (value, content_id)) in pending.iter().zip(content_ids.iter()).enumerate() {
            let content = value.content.as_ref().ok_or(StoreError::Invariant)?;
            if let Some(alias) = state.by_content.get(content_id).copied() {
                let existing = state.by_alias.get(&alias).ok_or(StoreError::Invariant)?;
                if !existing.content.equals(content)? {
                    return Err(StoreError::DigestCollision);
                }
                placements.push(Placement::Existing(alias));
            } else if let Some(first) = first_new.get(content_id).copied() {
                let first_content = pending[first]
                    .content
                    .as_ref()
                    .ok_or(StoreError::Invariant)?;
                if !first_content.equals(content)? {
                    return Err(StoreError::DigestCollision);
                }
                placements.push(Placement::New(first));
            } else {
                incoming = incoming
                    .checked_add(content.length)
                    .ok_or(StoreError::ContentTooLarge)?;
                first_new.insert(*content_id, index);
                placements.push(Placement::New(index));
            }
        }
        let current = state
            .bytes
            .checked_add(pending_after)
            .ok_or(StoreError::ContentTooLarge)?;
        let total = current
            .checked_add(incoming)
            .ok_or(StoreError::ContentTooLarge)?;
        if total > self.limits.max_bytes {
            return Err(StoreError::ByteQuota {
                current,
                incoming,
                max: self.limits.max_bytes,
            });
        }
        let new_entries = first_new.len();
        if state.by_alias.len().saturating_add(new_entries) > self.limits.max_entries {
            return Err(StoreError::EntryQuota {
                max: self.limits.max_entries,
            });
        }
        let next_alias = state
            .next_alias
            .checked_add(u64::try_from(new_entries).map_err(|_| StoreError::AliasExhausted)?)
            .ok_or(StoreError::AliasExhausted)?;

        let mut aliases = vec![0_u64; pending.len()];
        let mut allocated = HashMap::with_capacity(new_entries);
        let mut alias = state.next_alias;
        for (index, placement) in placements.into_iter().enumerate() {
            match placement {
                Placement::Existing(existing) => aliases[index] = existing,
                Placement::New(first) if first != index => {
                    aliases[index] = *allocated.get(&first).ok_or(StoreError::Invariant)?;
                }
                Placement::New(_) => {
                    let content_id = content_ids[index];
                    let content = pending[index].content.take().ok_or(StoreError::Invariant)?;
                    aliases[index] = alias;
                    allocated.insert(index, alias);
                    state.by_content.insert(content_id, alias);
                    state.by_alias.insert(
                        alias,
                        Entry {
                            content_id,
                            content: Arc::new(content),
                        },
                    );
                    alias = alias.checked_add(1).ok_or(StoreError::Invariant)?;
                }
            }
        }
        if alias != next_alias {
            return Err(StoreError::Invariant);
        }
        state.next_alias = next_alias;
        state.bytes = state
            .bytes
            .checked_add(incoming)
            .ok_or(StoreError::Invariant)?;
        state.pending_bytes = pending_after;
        for value in pending {
            if let Some(reservation) = &mut value.reservation {
                reservation.disarm();
            }
        }
        Ok(aliases)
    }

    #[must_use = "a missing or poisoned reference must be handled"]
    pub fn get(&self, alias: u64) -> Result<ResultLease, StoreError> {
        self.lock()?
            .by_alias
            .get(&alias)
            .map(|entry| ResultLease {
                content: Arc::clone(&entry.content),
            })
            .ok_or(StoreError::UnknownAlias(alias))
    }

    pub fn content_id(&self, alias: u64) -> Result<ContentId, StoreError> {
        self.lock()?
            .by_alias
            .get(&alias)
            .map(|entry| entry.content_id)
            .ok_or(StoreError::UnknownAlias(alias))
    }

    /// Releases content early. Released numeric aliases remain retired.
    pub fn release(&self, alias: u64) -> Result<(), StoreError> {
        let mut state = self.lock()?;
        let entry = state
            .by_alias
            .get(&alias)
            .ok_or(StoreError::UnknownAlias(alias))?;
        if Arc::strong_count(&entry.content) != 1 {
            return Err(StoreError::InUse(alias));
        }
        let entry = state.by_alias.remove(&alias).ok_or(StoreError::Invariant)?;
        state.by_content.remove(&entry.content_id);
        state.bytes = state
            .bytes
            .checked_sub(entry.content.length)
            .ok_or(StoreError::Invariant)?;
        Ok(())
    }

    pub fn usage(&self) -> Result<StoreUsage, StoreError> {
        let state = self.lock()?;
        Ok(StoreUsage {
            bytes: state.bytes,
            entries: state.by_alias.len(),
        })
    }

    #[must_use]
    pub const fn limits(&self) -> StoreLimits {
        self.limits
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, StoreError> {
        self.state.lock().map_err(|_| StoreError::Poisoned)
    }
}

impl StoreCapture {
    /// Appends one stream chunk. Quota is reserved before any spill write.
    pub async fn append(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let incoming = u64::try_from(bytes.len()).map_err(|_| StoreError::ContentTooLarge)?;
        let next_length = self
            .length
            .checked_add(incoming)
            .ok_or(StoreError::ContentTooLarge)?;

        match &mut self.state {
            CaptureState::Memory(memory) if next_length <= self.memory_limit => {
                memory
                    .try_reserve(bytes.len())
                    .map_err(|_| StoreError::ContentTooLarge)?;
                memory.extend_from_slice(bytes);
            }
            CaptureState::Memory(_) => {
                self.reservation.reserve_u64(next_length)?;
                let named = create_spool_file(self.reservation.store.spool_root.path())?;
                let (file, path) = named.into_parts();
                let root = Arc::clone(&self.reservation.store.spool_root);
                let previous =
                    match std::mem::replace(&mut self.state, CaptureState::Memory(Vec::new())) {
                        CaptureState::Memory(previous) => previous,
                        CaptureState::Disk { .. } => return Err(StoreError::Invariant),
                    };
                let sample_limit = usize::try_from(self.memory_limit).unwrap_or(usize::MAX);
                let mut sample = CaptureSample::new(sample_limit)?;
                sample.observe(&previous);
                sample.observe(bytes);
                let mut file = tokio::fs::File::from_std(file);
                file.write_all(&previous)
                    .await
                    .map_err(|_| StoreError::Io)?;
                file.write_all(bytes).await.map_err(|_| StoreError::Io)?;
                self.state = CaptureState::Disk {
                    file,
                    path,
                    root,
                    sample,
                };
            }
            CaptureState::Disk { file, sample, .. } => {
                self.reservation.reserve_u64(incoming)?;
                file.write_all(bytes).await.map_err(|_| StoreError::Io)?;
                sample.observe(bytes);
            }
        }
        self.length = next_length;
        Ok(())
    }

    /// Flushes a capture and transfers ownership of its immutable content.
    pub async fn finish(self) -> Result<CapturedContent, StoreError> {
        let StoreCapture {
            reservation,
            length,
            state,
            ..
        } = self;
        let (storage, sample) = match state {
            CaptureState::Memory(bytes) => (Storage::Memory(Arc::from(bytes)), None),
            CaptureState::Disk {
                mut file,
                path,
                root,
                mut sample,
            } => {
                file.flush().await.map_err(|_| StoreError::Io)?;
                drop(file);
                sample.normalize();
                (Storage::Disk { path, _root: root }, Some(sample))
            }
        };
        Ok(CapturedContent {
            pending: PendingContent {
                content_id: None,
                content: Some(Content { length, storage }),
                reservation: Some(reservation),
            },
            sample,
        })
    }
}

impl CapturedContent {
    #[must_use]
    pub fn len(&self) -> u64 {
        self.pending
            .content
            .as_ref()
            .map_or(0, |content| content.length)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn residency(&self) -> StoreResidency {
        self.pending
            .content
            .as_ref()
            .map_or(StoreResidency::Memory, Content::residency)
    }

    #[must_use]
    pub fn view(&self) -> CapturedView<'_> {
        match (&self.pending.content, &self.sample) {
            (
                Some(Content {
                    storage: Storage::Memory(bytes),
                    ..
                }),
                _,
            ) => CapturedView::Complete(bytes),
            (
                Some(Content {
                    storage: Storage::Disk { .. },
                    ..
                }),
                Some(sample),
            ) => CapturedView::Sampled {
                head: &sample.head,
                head_next: sample.head_next,
                tail: sample.tail.as_slices().0,
            },
            _ => CapturedView::Complete(&[]),
        }
    }
}

impl ResultLease {
    #[must_use]
    pub fn len(&self) -> u64 {
        self.content.length
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.content.length == 0
    }

    #[must_use]
    pub fn residency(&self) -> StoreResidency {
        self.content.residency()
    }

    /// Reads the full value only when it fits the caller's independent bound.
    pub async fn read_all(&self, max_bytes: u64) -> Result<Arc<[u8]>, StoreError> {
        if self.content.length > max_bytes {
            return Err(StoreError::ReadLimit {
                requested: self.content.length,
                max: max_bytes,
            });
        }
        match &self.content.storage {
            Storage::Memory(bytes) => Ok(Arc::clone(bytes)),
            Storage::Disk { path, .. } => {
                let capacity = usize::try_from(self.content.length)
                    .map_err(|_| StoreError::ContentTooLarge)?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(capacity)
                    .map_err(|_| StoreError::ContentTooLarge)?;
                let file = tokio::fs::File::open(path)
                    .await
                    .map_err(|_| StoreError::Io)?;
                let limit = self
                    .content
                    .length
                    .checked_add(1)
                    .ok_or(StoreError::ContentTooLarge)?;
                let mut reader = file.take(limit);
                reader
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| StoreError::Io)?;
                if bytes.len() != capacity {
                    return Err(StoreError::Io);
                }
                Ok(Arc::from(bytes))
            }
        }
    }

    /// Reads one byte range without materializing the complete retained value.
    pub async fn read_range(
        &self,
        offset: u64,
        length: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>, StoreError> {
        if length > max_bytes {
            return Err(StoreError::ReadLimit {
                requested: length,
                max: max_bytes,
            });
        }
        let start = offset.min(self.content.length);
        let actual = length.min(self.content.length.saturating_sub(start));
        let capacity = usize::try_from(actual).map_err(|_| StoreError::ContentTooLarge)?;
        match &self.content.storage {
            Storage::Memory(bytes) => {
                let start = usize::try_from(start).map_err(|_| StoreError::ContentTooLarge)?;
                let end = start
                    .checked_add(capacity)
                    .ok_or(StoreError::ContentTooLarge)?;
                Ok(bytes[start..end].to_vec())
            }
            Storage::Disk { path, .. } => {
                let mut file = tokio::fs::File::open(path)
                    .await
                    .map_err(|_| StoreError::Io)?;
                file.seek(SeekFrom::Start(start))
                    .await
                    .map_err(|_| StoreError::Io)?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(capacity)
                    .map_err(|_| StoreError::ContentTooLarge)?;
                bytes.resize(capacity, 0);
                file.read_exact(&mut bytes)
                    .await
                    .map_err(|_| StoreError::Io)?;
                Ok(bytes)
            }
        }
    }

    #[cfg(test)]
    fn spool_path(&self) -> Option<&Path> {
        match &self.content.storage {
            Storage::Memory(_) => None,
            Storage::Disk { path, .. } => Some(path.as_ref()),
        }
    }
}

impl Content {
    fn residency(&self) -> StoreResidency {
        match &self.storage {
            Storage::Memory(_) => StoreResidency::Memory,
            Storage::Disk { .. } => StoreResidency::Disk,
        }
    }

    fn reader(&self) -> Result<Box<dyn Read + '_>, StoreError> {
        match &self.storage {
            Storage::Memory(bytes) => Ok(Box::new(Cursor::new(bytes.as_ref()))),
            Storage::Disk { path, .. } => std::fs::File::open(path)
                .map(|file| Box::new(std::io::BufReader::new(file)) as Box<dyn Read>)
                .map_err(|_| StoreError::Io),
        }
    }

    fn digest(&self) -> Result<ContentId, StoreError> {
        let mut hasher = blake3::Hasher::new();
        match &self.storage {
            Storage::Memory(bytes) => {
                hasher.update_rayon(bytes);
            }
            Storage::Disk { path, .. } => {
                let file = std::fs::File::open(path).map_err(|_| StoreError::Io)?;
                let mut reader = std::io::BufReader::new(file);
                let mut buffer = Vec::new();
                buffer
                    .try_reserve_exact(DEFAULT_CAPTURE_MEMORY_BYTES)
                    .map_err(|_| StoreError::ContentTooLarge)?;
                buffer.resize(DEFAULT_CAPTURE_MEMORY_BYTES, 0);
                let mut observed = 0_u64;
                loop {
                    let read = reader.read(&mut buffer).map_err(|_| StoreError::Io)?;
                    if read == 0 {
                        break;
                    }
                    observed = observed
                        .checked_add(u64::try_from(read).map_err(|_| StoreError::ContentTooLarge)?)
                        .ok_or(StoreError::ContentTooLarge)?;
                    hasher.update_rayon(&buffer[..read]);
                }
                if observed != self.length {
                    return Err(StoreError::Io);
                }
            }
        }
        Ok(ContentId(*hasher.finalize().as_bytes()))
    }

    fn equals(&self, other: &Self) -> Result<bool, StoreError> {
        if self.length != other.length {
            return Ok(false);
        }
        if let (Storage::Memory(left), Storage::Memory(right)) = (&self.storage, &other.storage) {
            return Ok(left == right);
        }
        let mut left = self.reader()?;
        let mut right = other.reader()?;
        let mut left_buffer = [0_u8; 64 * 1024];
        let mut right_buffer = [0_u8; 64 * 1024];
        let mut remaining = self.length;
        while remaining > 0 {
            let chunk = usize::try_from(remaining.min(left_buffer.len() as u64))
                .map_err(|_| StoreError::ContentTooLarge)?;
            left.read_exact(&mut left_buffer[..chunk])
                .map_err(|_| StoreError::Io)?;
            right
                .read_exact(&mut right_buffer[..chunk])
                .map_err(|_| StoreError::Io)?;
            if left_buffer[..chunk] != right_buffer[..chunk] {
                return Ok(false);
            }
            remaining -= chunk as u64;
        }
        let mut left_end = [0_u8; 1];
        let mut right_end = [0_u8; 1];
        if left.read(&mut left_end).map_err(|_| StoreError::Io)? != 0
            || right.read(&mut right_end).map_err(|_| StoreError::Io)? != 0
        {
            return Err(StoreError::Io);
        }
        Ok(true)
    }
}

impl CaptureSample {
    fn new(limit: usize) -> Result<Self, StoreError> {
        let head_limit = limit / 2;
        let tail_limit = limit.saturating_sub(head_limit);
        let mut head = Vec::new();
        head.try_reserve_exact(head_limit)
            .map_err(|_| StoreError::ContentTooLarge)?;
        let mut tail = VecDeque::new();
        tail.try_reserve_exact(tail_limit)
            .map_err(|_| StoreError::ContentTooLarge)?;
        Ok(Self {
            head,
            head_next: None,
            tail,
            head_limit,
            tail_limit,
        })
    }

    fn observe(&mut self, bytes: &[u8]) {
        let head_remaining = self.head_limit.saturating_sub(self.head.len());
        let head_bytes = head_remaining.min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_bytes]);
        if self.head.len() == self.head_limit
            && self.head_next.is_none()
            && head_bytes < bytes.len()
        {
            self.head_next = Some(bytes[head_bytes]);
        }
        if self.tail_limit == 0 {
            return;
        }
        let tail_bytes = &bytes[head_bytes..];
        if tail_bytes.len() >= self.tail_limit {
            self.tail.clear();
            self.tail
                .extend(&tail_bytes[tail_bytes.len() - self.tail_limit..]);
            return;
        }
        let excess = self
            .tail
            .len()
            .saturating_add(tail_bytes.len())
            .saturating_sub(self.tail_limit);
        self.tail.drain(..excess);
        self.tail.extend(tail_bytes);
    }

    fn normalize(&mut self) {
        self.tail.make_contiguous();
    }
}

impl SpoolRoot {
    fn new() -> Result<Self, StoreError> {
        let directory = tempfile::Builder::new()
            .prefix("ash-store-")
            .tempdir()
            .map_err(|_| StoreError::Io)?;
        secure_directory(directory.path())?;
        Ok(Self { directory })
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }
}

fn create_spool_file(directory: &Path) -> Result<NamedTempFile, StoreError> {
    let file = tempfile::Builder::new()
        .prefix("stream-")
        .tempfile_in(directory)
        .map_err(|_| StoreError::Io)?;
    secure_file(file.as_file())?;
    Ok(file)
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| StoreError::Io)
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(file: &std::fs::File) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|_| StoreError::Io)
}

#[cfg(not(unix))]
fn secure_file(_file: &std::fs::File) -> Result<(), StoreError> {
    Ok(())
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum StoreError {
    #[error("result-store limits must be non-zero")]
    InvalidLimits,
    #[error("retained content does not fit the platform size model")]
    ContentTooLarge,
    #[error("retaining {incoming} bytes with {current} already used exceeds {max}")]
    ByteQuota {
        current: u64,
        incoming: u64,
        max: u64,
    },
    #[error("retained entry quota of {max} is exhausted")]
    EntryQuota { max: usize },
    #[error("retained alias space is exhausted")]
    AliasExhausted,
    #[error("unknown retained result alias {0}")]
    UnknownAlias(u64),
    #[error("retained result alias {0} is in use")]
    InUse(u64),
    #[error("retained-byte reservation does not belong to this store or value")]
    ReservationMismatch,
    #[error("reading {requested} retained bytes exceeds the caller limit of {max}")]
    ReadLimit { requested: u64, max: u64 },
    #[error("result-store disk I/O failed")]
    Io,
    #[error("a BLAKE3 digest collision was detected")]
    DigestCollision,
    #[error("result-store lock was poisoned")]
    Poisoned,
    #[error("result-store invariant failed")]
    Invariant,
}

impl StoreReservation {
    fn reserve_u64(&mut self, incoming: u64) -> Result<(), StoreError> {
        if incoming == 0 {
            return Ok(());
        }
        let reservation_total = self
            .bytes
            .checked_add(incoming)
            .ok_or(StoreError::ContentTooLarge)?;
        let mut state = self.store.lock()?;
        let current = state
            .bytes
            .checked_add(state.pending_bytes)
            .ok_or(StoreError::ContentTooLarge)?;
        let total = current
            .checked_add(incoming)
            .ok_or(StoreError::ContentTooLarge)?;
        if total > self.store.limits.max_bytes {
            return Err(StoreError::ByteQuota {
                current,
                incoming,
                max: self.store.limits.max_bytes,
            });
        }
        state.pending_bytes = state
            .pending_bytes
            .checked_add(incoming)
            .ok_or(StoreError::Invariant)?;
        self.bytes = reservation_total;
        Ok(())
    }

    #[must_use]
    const fn bytes(&self) -> u64 {
        self.bytes
    }

    fn disarm(&mut self) {
        self.bytes = 0;
    }
}

impl Drop for StoreReservation {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        if let Ok(mut state) = self.store.state.lock() {
            state.pending_bytes = state.pending_bytes.saturating_sub(self.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::{
        Content, ResultStore, Storage, StoreError, StoreLimits, StoreResidency, StoreUsage,
    };

    #[tokio::test]
    async fn immutable_content_is_deduplicated_and_aliases_are_not_reused() {
        let store = ResultStore::new(StoreLimits {
            max_bytes: 16,
            max_entries: 2,
        })
        .expect("store");
        let first = store.retain(b"same".to_vec()).expect("retain");
        assert_eq!(store.retain(b"same".to_vec()).expect("deduplicate"), first);
        assert_eq!(
            store
                .get(first)
                .expect("get")
                .read_all(4)
                .await
                .expect("read")
                .as_ref(),
            b"same"
        );
        assert_eq!(
            store.usage().expect("usage"),
            StoreUsage {
                bytes: 4,
                entries: 1
            }
        );

        store.release(first).expect("release");
        let second = store.retain(b"same".to_vec()).expect("retain again");
        assert!(second > first);
    }

    #[test]
    fn quota_failures_do_not_partially_mutate_usage() {
        let store = ResultStore::new(StoreLimits {
            max_bytes: 4,
            max_entries: 1,
        })
        .expect("store");
        store.retain(b"1234".to_vec()).expect("retain");
        assert!(matches!(
            store.retain(b"5".to_vec()),
            Err(StoreError::ByteQuota { .. })
        ));
        assert_eq!(store.usage().expect("usage").bytes, 4);
    }

    #[tokio::test]
    async fn grouped_retention_allocates_stable_aliases_and_reuses_duplicates() {
        let store = ResultStore::new(StoreLimits::default()).expect("store");
        let aliases = store
            .retain_many(vec![b"beta".to_vec(), b"beta".to_vec(), b"alpha".to_vec()])
            .expect("retain group");

        assert_eq!(aliases, vec![1, 1, 2]);
        assert_eq!(
            store
                .get(1)
                .expect("first")
                .read_all(4)
                .await
                .expect("read first")
                .as_ref(),
            b"beta"
        );
        assert_eq!(
            store
                .get(2)
                .expect("second")
                .read_all(5)
                .await
                .expect("read second")
                .as_ref(),
            b"alpha"
        );
        assert_eq!(
            store.usage().expect("usage"),
            StoreUsage {
                bytes: 9,
                entries: 2,
            }
        );
    }

    #[tokio::test]
    async fn grouped_quota_failure_is_atomic() {
        let store = ResultStore::new(StoreLimits {
            max_bytes: 6,
            max_entries: 3,
        })
        .expect("store");
        let existing = store.retain(b"ok".to_vec()).expect("retain existing");
        let before = store.usage().expect("usage before");

        assert!(matches!(
            store.retain_many(vec![b"abc".to_vec(), b"def".to_vec()]),
            Err(StoreError::ByteQuota { .. })
        ));
        assert_eq!(store.usage().expect("usage after"), before);
        assert_eq!(
            store
                .get(existing)
                .expect("existing")
                .read_all(2)
                .await
                .expect("read existing")
                .as_ref(),
            b"ok"
        );
        assert_eq!(
            store.retain(b"new".to_vec()).expect("first unused alias"),
            2
        );
    }

    #[tokio::test]
    async fn in_flight_captures_count_toward_quota_and_release_on_drop() {
        let store = Arc::new(
            ResultStore::new(StoreLimits {
                max_bytes: 8,
                max_entries: 2,
            })
            .expect("store"),
        );
        let mut capture = store.capture(0);
        capture
            .append(b"123456")
            .await
            .expect("reserve capture bytes");
        assert!(matches!(
            store.retain(b"new".to_vec()),
            Err(StoreError::ByteQuota {
                current: 6,
                incoming: 3,
                max: 8,
            })
        ));
        assert_eq!(store.usage().expect("committed usage").bytes, 0);

        drop(capture);
        assert_eq!(store.retain(b"new".to_vec()).expect("quota released"), 1);
    }

    #[tokio::test]
    async fn captured_values_commit_atomically_without_double_charging() {
        let store = Arc::new(
            ResultStore::new(StoreLimits {
                max_bytes: 10,
                max_entries: 2,
            })
            .expect("store"),
        );
        let mut first = store.capture(0);
        let mut second = store.capture(0);
        first.append(b"first").await.expect("capture first");
        second.append(b"other").await.expect("capture second");
        let first = first.finish().await.expect("finish first");
        let second = second.finish().await.expect("finish second");

        let aliases = store
            .retain_captures(vec![first, second])
            .expect("commit captures");
        assert_eq!(aliases, vec![1, 2]);
        assert_eq!(
            store.usage().expect("usage"),
            StoreUsage {
                bytes: 10,
                entries: 2,
            }
        );
        assert_eq!(
            store
                .get(1)
                .expect("first")
                .read_all(5)
                .await
                .expect("read first")
                .as_ref(),
            b"first"
        );
        assert_eq!(
            store
                .get(2)
                .expect("second")
                .read_all(5)
                .await
                .expect("read second")
                .as_ref(),
            b"other"
        );
    }

    #[tokio::test]
    async fn failed_capture_commit_releases_charge_and_does_not_consume_aliases() {
        let store = Arc::new(
            ResultStore::new(StoreLimits {
                max_bytes: 8,
                max_entries: 2,
            })
            .expect("store"),
        );
        let mut reserved = store.capture(0);
        reserved.append(b"1234").await.expect("reserved capture");
        let reserved = reserved.finish().await.expect("finish reserved");
        let mut memory = store.capture(8);
        memory.append(b"56789").await.expect("memory capture");
        let memory = memory.finish().await.expect("finish memory");
        assert!(matches!(
            store.retain_captures(vec![reserved, memory]),
            Err(StoreError::ByteQuota { .. })
        ));
        assert_eq!(
            store.usage().expect("unchanged usage"),
            StoreUsage {
                bytes: 0,
                entries: 0,
            }
        );
        assert_eq!(
            store
                .retain(b"12345678".to_vec())
                .expect("reservation released"),
            1
        );
    }

    #[test]
    fn concurrent_identical_retention_has_one_alias_and_one_charge() {
        let store = Arc::new(ResultStore::new(StoreLimits::default()).expect("store"));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                thread::spawn(move || store.retain(vec![7; 1024]).expect("retain"))
            })
            .collect();
        let aliases: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("join"))
            .collect();
        assert!(aliases.iter().all(|alias| *alias == aliases[0]));
        assert_eq!(store.usage().expect("usage").entries, 1);
        assert_eq!(store.limits(), StoreLimits::default());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_capture_reservations_never_exceed_session_quota() {
        let store = Arc::new(
            ResultStore::new(StoreLimits {
                max_bytes: 32,
                max_entries: 8,
            })
            .expect("store"),
        );
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let tasks = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    let mut capture = store.capture(0);
                    barrier.wait().await;
                    capture.append(b"12345678").await.ok().map(|()| capture)
                })
            })
            .collect::<Vec<_>>();
        let mut reservations = Vec::new();
        for task in tasks {
            if let Some(capture) = task.await.expect("join") {
                reservations.push(capture);
            }
        }

        assert_eq!(reservations.len(), 4);
        assert!(matches!(
            store.retain(b"x".to_vec()),
            Err(StoreError::ByteQuota {
                current: 32,
                incoming: 1,
                max: 32,
            })
        ));
        drop(reservations);
        assert_eq!(store.retain(vec![0; 32]).expect("quota released"), 1);
    }

    #[test]
    fn active_readers_prevent_release() {
        let store = ResultStore::new(StoreLimits::default()).expect("store");
        let alias = store.retain(b"leased".to_vec()).expect("retain");
        let reader = store.get(alias).expect("reader");
        assert_eq!(store.release(alias), Err(StoreError::InUse(alias)));
        drop(reader);
        store.release(alias).expect("release after reader");
    }

    #[tokio::test]
    async fn large_capture_spills_to_disk_and_remains_range_readable() {
        let store = Arc::new(
            ResultStore::new(StoreLimits {
                max_bytes: 64,
                max_entries: 2,
            })
            .expect("store"),
        );
        let mut capture = store.capture(8);
        capture.append(b"abcdefgh").await.expect("memory prefix");
        capture.append(b"ijklmnop").await.expect("spilled suffix");
        let captured = capture.finish().await.expect("finish capture");
        assert_eq!(captured.residency(), StoreResidency::Disk);

        let alias = store
            .retain_captures(vec![captured])
            .expect("commit capture")[0];
        let lease = store.get(alias).expect("lease");
        let spool_path = lease.spool_path().expect("disk path").to_owned();
        assert!(spool_path.is_file());
        assert_eq!(
            lease.read_range(6, 5, 5).await.expect("bounded range"),
            b"ghijk"
        );
        assert_eq!(
            lease.read_all(16).await.expect("complete value").as_ref(),
            b"abcdefghijklmnop"
        );
        assert_eq!(
            lease.read_all(15).await,
            Err(StoreError::ReadLimit {
                requested: 16,
                max: 15,
            })
        );
        assert_eq!(store.release(alias), Err(StoreError::InUse(alias)));

        drop(lease);
        store.release(alias).expect("release after lease");
        assert!(!spool_path.exists());
    }

    #[tokio::test]
    async fn disk_captures_deduplicate_and_session_drop_removes_the_spool_root() {
        let store = Arc::new(
            ResultStore::new(StoreLimits {
                max_bytes: 64,
                max_entries: 2,
            })
            .expect("store"),
        );
        let mut first = store.capture(8);
        first
            .append(b"same-disk-value")
            .await
            .expect("first capture");
        let first = first.finish().await.expect("finish first");
        let alias = store.retain_captures(vec![first]).expect("retain first")[0];

        let mut duplicate = store.capture(8);
        duplicate
            .append(b"same-disk-value")
            .await
            .expect("duplicate capture");
        let duplicate = duplicate.finish().await.expect("finish duplicate");
        assert_eq!(
            store
                .retain_captures(vec![duplicate])
                .expect("deduplicate disk capture"),
            vec![alias]
        );
        assert_eq!(
            store.usage().expect("deduplicated usage"),
            StoreUsage {
                bytes: 15,
                entries: 1,
            }
        );

        let lease = store.get(alias).expect("lease");
        let spool_path = lease.spool_path().expect("spool path").to_owned();
        let spool_root = spool_path.parent().expect("spool root").to_owned();
        drop(lease);
        drop(store);
        assert!(!spool_path.exists());
        assert!(!spool_root.exists());
    }

    #[tokio::test]
    async fn changed_spool_length_fails_closed_before_alias_allocation() {
        use std::io::Write as _;

        let store = Arc::new(ResultStore::new(StoreLimits::default()).expect("store"));
        let mut capture = store.capture(1);
        capture.append(b"immutable").await.expect("capture");
        let captured = capture.finish().await.expect("finish");
        let path = match &captured.pending.content {
            Some(Content {
                storage: Storage::Disk { path, .. },
                ..
            }) => path.to_path_buf(),
            _ => panic!("capture did not spill"),
        };
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open spool")
            .write_all(b"changed")
            .expect("change spool length");

        assert_eq!(store.retain_captures(vec![captured]), Err(StoreError::Io));
        assert_eq!(
            store.usage().expect("unchanged usage"),
            StoreUsage {
                bytes: 0,
                entries: 0,
            }
        );
        assert!(!path.exists());
        assert_eq!(store.retain(b"ok".to_vec()).expect("first alias"), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_spool_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let store = Arc::new(ResultStore::new(StoreLimits::default()).expect("store"));
        let mut capture = store.capture(1);
        capture.append(b"private").await.expect("capture");
        let alias = store
            .retain_captures(vec![capture.finish().await.expect("finish")])
            .expect("retain")[0];
        let lease = store.get(alias).expect("lease");
        let path = lease.spool_path().expect("spool path");
        let directory = path.parent().expect("spool directory");

        assert_eq!(
            std::fs::metadata(directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
