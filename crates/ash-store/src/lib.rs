#![forbid(unsafe_code)]

//! Bounded, session-local retained result storage for ash.

mod path_dictionary;
mod result_store;

pub use path_dictionary::{InternedPaths, PathDictionary, PathDictionaryError, PathEntry};
pub use result_store::{
    ContentId, ResultStore, StoreError, StoreLimits, StoreReservation, StoreUsage,
};
