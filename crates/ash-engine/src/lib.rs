#![forbid(unsafe_code)]

//! Deterministic scheduling primitives for the ash execution engine.

mod parallel;

pub use parallel::{ComputePool, Parallelism, ParallelismError};
