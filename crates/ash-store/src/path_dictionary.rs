use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use thiserror::Error;

/// One new logical-path mapping emitted to the session peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathEntry {
    pub id: u64,
    pub value: String,
}

/// Identifiers corresponding to input order plus mappings introduced by a batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InternedPaths {
    pub ids: Vec<u64>,
    pub introduced: Vec<PathEntry>,
}

/// Session-local logical path interner.
///
/// Callers pass paths in their deterministic result order. One batch is
/// assigned atomically, preventing duplicate aliases when requests overlap.
pub struct PathDictionary {
    max_entries: usize,
    state: Mutex<State>,
}

struct State {
    next_id: u64,
    by_path: HashMap<String, u64>,
}

impl PathDictionary {
    pub fn new(max_entries: usize) -> Result<Self, PathDictionaryError> {
        if max_entries == 0 {
            return Err(PathDictionaryError::InvalidLimit);
        }
        Ok(Self {
            max_entries,
            state: Mutex::new(State {
                next_id: 1,
                by_path: HashMap::new(),
            }),
        })
    }

    pub fn intern(&self, paths: &[String]) -> Result<InternedPaths, PathDictionaryError> {
        let mut state = self.lock()?;
        for path in paths {
            validate_path(path)?;
        }
        let new_count = paths
            .iter()
            .filter(|path| !state.by_path.contains_key(path.as_str()))
            .collect::<std::collections::HashSet<_>>()
            .len();
        if state.by_path.len().saturating_add(new_count) > self.max_entries {
            return Err(PathDictionaryError::Quota {
                max: self.max_entries,
            });
        }
        let new_count = u64::try_from(new_count).map_err(|_| PathDictionaryError::IdExhausted)?;
        state
            .next_id
            .checked_add(new_count)
            .ok_or(PathDictionaryError::IdExhausted)?;

        let mut ids = Vec::with_capacity(paths.len());
        let mut introduced = Vec::with_capacity(new_count as usize);
        for path in paths {
            let id = if let Some(id) = state.by_path.get(path).copied() {
                id
            } else {
                let id = state.next_id;
                state.next_id += 1;
                state.by_path.insert(path.clone(), id);
                introduced.push(PathEntry {
                    id,
                    value: path.clone(),
                });
                id
            };
            ids.push(id);
        }
        Ok(InternedPaths { ids, introduced })
    }

    pub fn len(&self) -> Result<usize, PathDictionaryError> {
        Ok(self.lock()?.by_path.len())
    }

    pub fn is_empty(&self) -> Result<bool, PathDictionaryError> {
        Ok(self.lock()?.by_path.is_empty())
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, PathDictionaryError> {
        self.state.lock().map_err(|_| PathDictionaryError::Poisoned)
    }
}

impl Default for PathDictionary {
    fn default() -> Self {
        Self {
            max_entries: 65_536,
            state: Mutex::new(State {
                next_id: 1,
                by_path: HashMap::new(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PathDictionaryError {
    #[error("path dictionary limit must be non-zero")]
    InvalidLimit,
    #[error("path dictionary quota of {max} is exhausted")]
    Quota { max: usize },
    #[error("logical path is empty, too long, or contains NUL")]
    InvalidPath,
    #[error("path identifier space is exhausted")]
    IdExhausted,
    #[error("path dictionary lock was poisoned")]
    Poisoned,
}

fn validate_path(path: &str) -> Result<(), PathDictionaryError> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        Err(PathDictionaryError::InvalidPath)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PathDictionary, PathDictionaryError, PathEntry};

    #[test]
    fn input_order_drives_ids_and_only_new_mappings_are_returned() {
        let dictionary = PathDictionary::new(4).expect("dictionary");
        let first = dictionary
            .intern(&["b".to_owned(), "a".to_owned(), "b".to_owned()])
            .expect("intern");
        assert_eq!(first.ids, [1, 2, 1]);
        assert_eq!(
            first.introduced,
            [
                PathEntry {
                    id: 1,
                    value: "b".to_owned()
                },
                PathEntry {
                    id: 2,
                    value: "a".to_owned()
                }
            ]
        );

        let second = dictionary
            .intern(&["a".to_owned(), "c".to_owned()])
            .expect("intern");
        assert_eq!(second.ids, [2, 3]);
        assert_eq!(second.introduced.len(), 1);
        assert_eq!(dictionary.len().expect("len"), 3);
    }

    #[test]
    fn quota_failure_is_atomic_even_with_duplicate_input() {
        let dictionary = PathDictionary::new(1).expect("dictionary");
        let error = dictionary
            .intern(&["a".to_owned(), "b".to_owned(), "b".to_owned()])
            .expect_err("quota");
        assert_eq!(error, PathDictionaryError::Quota { max: 1 });
        assert_eq!(dictionary.len().expect("len"), 0);
    }
}
