#![forbid(unsafe_code)]

//! Bounded, session-local retained result storage for ash.

mod path_dictionary;
mod result_store;

pub use path_dictionary::{InternedPaths, PathDictionary, PathDictionaryError, PathEntry};
pub use result_store::{
    CapturedContent, CapturedView, ContentId, DEFAULT_CAPTURE_MEMORY_BYTES, ResultLease,
    ResultStore, StoreCapture, StoreError, StoreLimits, StoreResidency, StoreUsage,
};
