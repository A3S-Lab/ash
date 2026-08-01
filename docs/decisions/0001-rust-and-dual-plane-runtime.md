# ADR 0001: Rust and a dual-plane runtime

- Status: accepted
- Date: 2026-08-02

## Context

`ash` must minimize agent tokens while delivering a high-performance execution boundary on Linux, macOS, and Windows. Its workload combines asynchronous child-process and pipe I/O with CPU-heavy repository search, hashing, diffing, reduction, and graph scheduling. Treating all of that work as asynchronous tasks would not guarantee CPU parallelism; placing blocking work on the I/O runtime would harm latency under load.

The `ash` repository is independently releasable and is registered in the A3S monorepo as the `crates/ash` Git submodule. The A3S root remains an umbrella package rather than a Cargo workspace.

## Decision

The implementation language is stable Rust.

The runtime has two cooperating execution planes:

1. A Tokio multi-thread runtime owns RPC, timers, cancellation, child processes, pipes, and bounded asynchronous channels.
2. A fixed Rayon work-stealing pool owns CPU-intensive, splittable work such as repository search, hashing, diff computation, structured reduction, and large deterministic merges.

A shared governor limits runnable graph nodes, child processes, filesystem operations, retained bytes, and CPU tasks. Defaults derive from `std::thread::available_parallelism()` and remain overridable by an explicit runtime configuration or request budget. `ash` never creates one operating-system thread per file, result, or graph node.

Parallel operations partition input into deterministic units. Workers may finish in any order, but externally visible records are merged by stable operation-specific keys before ASON encoding. Parallel execution therefore cannot change canonical output.

Protocol, engine, operation, store, and CLI crates forbid unsafe Rust. Target-specific unsafe code is permitted only in narrowly scoped `ash-platform` modules when a native API has no safe binding; each block must document its invariants and have a platform contract test.

## Consequences

- I/O stalls do not consume compute workers, and CPU-heavy scans do not block pipe draining or cancellation.
- Independent graph nodes and intra-operation partitions can use multiple cores without oversubscribing the host.
- Benchmarks must report scaling at fixed 1, 2, 4, and 8-worker configurations as well as the detected host default.
- Deterministic merge and bounded queues add some coordination cost; that cost is accepted because reproducible ASON is a protocol requirement.
- Zig may be evaluated for a future bootstrap helper only if measured installer constraints justify another language. It is not part of the core runtime.
