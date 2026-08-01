use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use thiserror::Error;

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

/// Thread-safe immutable content store. Numeric aliases are never reused.
pub struct ResultStore {
    limits: StoreLimits,
    state: Mutex<State>,
}

struct State {
    next_alias: u64,
    bytes: u64,
    by_alias: BTreeMap<u64, Entry>,
    by_content: HashMap<ContentId, u64>,
}

struct Entry {
    content_id: ContentId,
    bytes: Arc<[u8]>,
}

impl ResultStore {
    pub fn new(limits: StoreLimits) -> Result<Self, StoreError> {
        if limits.max_bytes == 0 || limits.max_entries == 0 {
            return Err(StoreError::InvalidLimits);
        }
        Ok(Self {
            limits,
            state: Mutex::new(State {
                next_alias: 1,
                bytes: 0,
                by_alias: BTreeMap::new(),
                by_content: HashMap::new(),
            }),
        })
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
    pub fn retain_many(&self, mut contents: Vec<Vec<u8>>) -> Result<Vec<u64>, StoreError> {
        if contents.is_empty() {
            return Ok(Vec::new());
        }
        let content_ids = contents
            .iter()
            .map(|bytes| ContentId(*blake3::hash(bytes).as_bytes()))
            .collect::<Vec<_>>();
        let mut state = self.lock()?;

        enum Placement {
            Existing(u64),
            New(usize),
        }

        let mut placements = Vec::with_capacity(contents.len());
        let mut first_new = HashMap::new();
        let mut incoming = 0_u64;
        for (index, (content_id, bytes)) in content_ids.iter().zip(&contents).enumerate() {
            if let Some(alias) = state.by_content.get(content_id).copied() {
                let existing = state.by_alias.get(&alias).ok_or(StoreError::Invariant)?;
                if existing.bytes.as_ref() != bytes.as_slice() {
                    return Err(StoreError::DigestCollision);
                }
                placements.push(Placement::Existing(alias));
            } else if let Some(first) = first_new.get(content_id).copied() {
                if contents[first] != *bytes {
                    return Err(StoreError::DigestCollision);
                }
                placements.push(Placement::New(first));
            } else {
                let bytes = u64::try_from(bytes.len()).map_err(|_| StoreError::ContentTooLarge)?;
                incoming = incoming
                    .checked_add(bytes)
                    .ok_or(StoreError::ContentTooLarge)?;
                first_new.insert(*content_id, index);
                placements.push(Placement::New(index));
            }
        }
        let total = state
            .bytes
            .checked_add(incoming)
            .ok_or(StoreError::ContentTooLarge)?;
        if total > self.limits.max_bytes {
            return Err(StoreError::ByteQuota {
                current: state.bytes,
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

        let mut aliases = vec![0_u64; contents.len()];
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
                    let bytes = std::mem::take(&mut contents[index]);
                    aliases[index] = alias;
                    allocated.insert(index, alias);
                    state.by_content.insert(content_id, alias);
                    state.by_alias.insert(
                        alias,
                        Entry {
                            content_id,
                            bytes: Arc::from(bytes),
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
        state.bytes = total;
        Ok(aliases)
    }

    #[must_use = "a missing or poisoned reference must be handled"]
    pub fn get(&self, alias: u64) -> Result<Arc<[u8]>, StoreError> {
        self.lock()?
            .by_alias
            .get(&alias)
            .map(|entry| Arc::clone(&entry.bytes))
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
        if Arc::strong_count(&entry.bytes) != 1 {
            return Err(StoreError::InUse(alias));
        }
        let entry = state.by_alias.remove(&alias).ok_or(StoreError::Invariant)?;
        state.by_content.remove(&entry.content_id);
        state.bytes = state
            .bytes
            .checked_sub(entry.bytes.len() as u64)
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

impl Default for ResultStore {
    fn default() -> Self {
        let limits = StoreLimits::default();
        Self {
            limits,
            state: Mutex::new(State {
                next_alias: 1,
                bytes: 0,
                by_alias: BTreeMap::new(),
                by_content: HashMap::new(),
            }),
        }
    }
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
    #[error("a BLAKE3 digest collision was detected")]
    DigestCollision,
    #[error("result-store lock was poisoned")]
    Poisoned,
    #[error("result-store invariant failed")]
    Invariant,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::{ResultStore, StoreError, StoreLimits, StoreUsage};

    #[test]
    fn immutable_content_is_deduplicated_and_aliases_are_not_reused() {
        let store = ResultStore::new(StoreLimits {
            max_bytes: 16,
            max_entries: 2,
        })
        .expect("store");
        let first = store.retain(b"same".to_vec()).expect("retain");
        assert_eq!(store.retain(b"same".to_vec()).expect("deduplicate"), first);
        assert_eq!(store.get(first).expect("get").as_ref(), b"same");
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

    #[test]
    fn grouped_retention_allocates_stable_aliases_and_reuses_duplicates() {
        let store = ResultStore::default();
        let aliases = store
            .retain_many(vec![b"beta".to_vec(), b"beta".to_vec(), b"alpha".to_vec()])
            .expect("retain group");

        assert_eq!(aliases, vec![1, 1, 2]);
        assert_eq!(store.get(1).expect("first").as_ref(), b"beta");
        assert_eq!(store.get(2).expect("second").as_ref(), b"alpha");
        assert_eq!(
            store.usage().expect("usage"),
            StoreUsage {
                bytes: 9,
                entries: 2,
            }
        );
    }

    #[test]
    fn grouped_quota_failure_is_atomic() {
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
        assert_eq!(store.get(existing).expect("existing").as_ref(), b"ok");
        assert_eq!(
            store.retain(b"new".to_vec()).expect("first unused alias"),
            2
        );
    }

    #[test]
    fn concurrent_identical_retention_has_one_alias_and_one_charge() {
        let store = Arc::new(ResultStore::default());
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

    #[test]
    fn active_readers_prevent_release() {
        let store = ResultStore::default();
        let alias = store.retain(b"leased".to_vec()).expect("retain");
        let reader = store.get(alias).expect("reader");
        assert_eq!(store.release(alias), Err(StoreError::InUse(alias)));
        drop(reader);
        store.release(alias).expect("release after reader");
    }
}
