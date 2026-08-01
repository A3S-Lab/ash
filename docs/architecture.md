# ash system architecture

Status: architecture baseline

This document defines the intended architecture of `ash`. It is normative for component ownership and runtime boundaries. It does not claim that the described implementation already exists.

## 1. Product definition

`ash` is an **AI Native Shell**.

Its primary caller is a coding agent or an agent harness. The shell therefore optimizes for task completion, total LLM token cost, deterministic execution, and stable machine semantics. Human-readable command syntax, terminal ergonomics, and compatibility with historical shells are not design goals.

`ash` does not interpret natural language and does not embed a model. Planning remains the responsibility of the calling agent. `ash` receives a typed program, executes it locally, and returns a bounded structured result.

### 1.1 Goals

- Minimize total agent interaction tokens, including protocol instructions, requests, results, and retries.
- Provide identical operation semantics on Linux, macOS, and Windows.
- Execute external programs without an implicit host shell.
- Make output limits, truncation, cancellation, conflicts, and partial failure explicit.
- Support persistent low-latency sessions and one-shot fallback calls.
- Ship as one native binary with one-click installation and self-update.
- Remain useful without a network service, account, model provider, or background daemon.

### 1.2 Non-goals for version one

- POSIX shell, Bash, Zsh, Fish, PowerShell, or CMD compatibility.
- A human REPL, prompt, history UI, completion, aliases, or themes.
- Interactive job control or PTY emulation.
- Natural-language command generation.
- Remote execution, fleet orchestration, or a hosted control plane.
- A plugin marketplace or dynamically loaded in-process plugins.
- A universal cross-platform syscall or network sandbox.
- Reimplementation of Git, compilers, package managers, or other domain tools.

## 2. Architectural principles

### 2.1 Optimize completed tasks, not individual strings

A representation that is shorter but causes more agent retries is a regression. Benchmarks must include the session primer, operation calls, responses, error recovery, and any follow-up reference reads.

### 2.2 Separate semantics from representation

Operations and results are strongly typed inside the runtime. ASON, CLI arguments, and future adapters are representations over the same semantic model. No representation owns execution behavior.

### 2.3 Reduce before encoding

Compression after serialization does not reduce LLM tokens. Projection, filtering, repeated-line collapse, path interning, diagnostic extraction, and result referencing happen before the LLM-facing encoder.

### 2.4 Make implicit shell behavior explicit

The default process operation accepts executable, argument vector, working directory, environment delta, timeout, input, and output policy. It does not perform glob expansion, variable interpolation, quoting, command substitution, or host-shell selection.

### 2.5 Bound every resource

Programs have limits for elapsed time, parallelism, child processes, bytes read, bytes retained, records emitted, and session storage. Every stream uses bounded memory and can spill to a quota-controlled store.

### 2.6 Preserve evidence

Reduction may omit data from the immediate response, but it must never silently destroy the ability to inspect that data. Reduced or truncated content carries a session result reference unless retention was explicitly disabled.

### 2.7 Parallelize work, not observable order

Independent graph nodes and splittable operations use all available CPU cores when the workload is large enough to repay scheduling cost. I/O readiness and CPU work run on separate executors so repository scans cannot delay pipe draining, cancellation, or RPC progress. Parallel completion order is never protocol order: records pass through a stable merge before reduction and ASON encoding, and concurrent request finals are sequenced by input order.

## 3. System context

```text
Agent or harness
    |
    | persistent stdio / one-shot process
    v
Protocol gateway
    |
    v
Typed Program IR
    |
    +--> validation and capability policy
    +--> scheduler, governor, and cancellation tree
    |        |--> async I/O plane
    |        `--> work-stealing compute plane
    +--> operation engine
             |
             +--> process backend
             +--> filesystem backend
             +--> search and patch operations
             +--> session result store
    |
    v
deterministic reducers -> ASON encoder -> agent context
```

There is no required global daemon. A harness that needs warm state starts `ash rpc` once and retains the stdio child for the life of its agent session. One-shot invocations run the same engine with an ephemeral session.

## 4. Component model

### 4.1 `ash-protocol`

Owns:

- ASH/1 message and value types;
- operation identifiers and typed arguments;
- stable status and error codes;
- program, node, edge, budget, capability, and result schemas;
- the canonical ASON codec and stdio framing;
- framing, handshake, capability negotiation, and compatibility fixtures.

It contains no operating-system I/O and does not dispatch operations.

### 4.2 `ash-engine`

Owns:

- program normalization and validation;
- immutable execution-context capture;
- dependency graph construction;
- concurrency scheduling and failure propagation;
- global, session, program, and node concurrency governance;
- I/O-plane and compute-plane dispatch;
- cancellation trees and deadlines;
- resource-budget allocation;
- operation dispatch;
- result assembly and event sequencing.

The engine depends on traits provided by the operation, platform, and store boundaries. It cannot contain target-specific conditional behavior.

### 4.3 `ash-ops`

Owns portable operation semantics:

- `exec` — direct child-process execution;
- `read` — bounded byte and line slices across one or more files;
- `list` — directory enumeration, globbing, stat, and compact trees;
- `search` — literal and regular-expression search;
- `patch` — compare-and-swap file edits and multi-file journals;
- `fs` — create, copy, move, and remove mutations;
- `snapshot` — workspace state and deltas;
- `ref` — slice, filter, search, and project stored results;
- `cancel` — explicit cancellation of a program or node.

Operation implementations may use platform traits but cannot inspect the host shell or emit presentation text.

### 4.4 `ash-platform`

Owns native operating-system adapters:

- executable resolution and process creation;
- pipes, process groups, job objects, signals, and termination;
- filesystem metadata and path conversion;
- atomic replacement primitives;
- clocks, temporary locations, terminal detection, and environment access;
- optional filesystem observation used by workspace snapshots.

The crate exposes a platform-neutral interface and target-specific modules for Linux, macOS, and Windows.

### 4.5 `ash-store`

Owns session-local retained data:

- content-addressed blobs;
- small numeric aliases used in LLM-facing responses;
- path interning tables;
- stream spooling and bounded ring buffers;
- quotas, expiry, cleanup, and secure file permissions;
- workspace snapshot indexes;
- result metadata and reducer provenance.

Content identity uses a full digest internally. Short aliases are valid only inside the session and are never used as security identifiers.

### 4.6 `ash-cli`

Owns the `ash` executable:

- persistent `rpc` mode;
- one-shot operation invocation;
- installer receipt discovery;
- self-update, rollback, and uninstall coordination;
- machine-only bootstrap diagnostics.

The CLI translates arguments into protocol requests and renders protocol results. It does not duplicate operation logic.

## 5. Semantic execution model

### 5.1 Program

A program is the unit accepted by the engine:

```text
Program
  id
  context
  nodes[]
  edges[]
  budget
  failure_policy
  capability_set
```

A one-operation call is normalized into a one-node program. Batch calls and pipelines use the same type rather than a second execution path.

### 5.2 Node

Each node contains:

```text
Node
  id
  operation
  arguments
  input_bindings
  output_policy
  deadline
  resource_limits
  required_capabilities
```

Operations are versioned semantic identifiers. Single-character mnemonics are presentation aliases, not internal enum stability guarantees.

### 5.3 Edges

Edges are typed:

- `control` — target becomes eligible after source reaches the required status;
- `stream` — source bytes flow into the target input with backpressure;
- `value` — target consumes a typed value or result reference;
- `artifact` — target receives a stored file or blob reference.

The graph must be acyclic. Cycles fail validation before any mutation or process creation.

### 5.4 Context

An execution context includes workspace root, logical current directory, environment base plus delta, platform capabilities, policy identity, and session dictionaries. The context is captured when a program is accepted. Concurrent nodes cannot observe later context mutations.

There is no implicit `cd` operation inside a graph. A session may update its default context between programs; every submitted program receives an immutable snapshot.

### 5.5 Multi-core execution model

`ash` uses two bounded execution planes because asynchronous I/O and CPU parallelism solve different problems.

| Work class | Executor | Typical work | Ordering boundary |
| --- | --- | --- | --- |
| I/O and control | Tokio multi-thread runtime | stdio RPC, timers, process pipes, cancellation, bounded channels | request and event sequence numbers |
| CPU partitions | Fixed Rayon work-stealing pool | search, hashing, diffing, structured reduction, large merges | stable operation-specific keys |
| Child processes | Native platform backend, observed asynchronously | compilers, tests, tools, direct executable calls | typed node dependencies |

The compute pool defaults to `std::thread::available_parallelism()` workers, so a CPU-bound operation can occupy every CPU made available to the process by the operating system or container. The I/O runtime uses a smaller independently bounded worker set; CPU work is never executed inline on an I/O worker. Explicit configuration can lower both limits for shared hosts and repeatable benchmarks.

Parallel work follows these rules:

- The DAG scheduler starts independent ready nodes concurrently while respecting program priority, dependency, mutation, and process limits.
- `list`, `search`, snapshot hashing, multi-file patch preparation, and result reduction partition by directory, file, or bounded byte range. Small inputs remain sequential when parallel setup would cost more than the work.
- Large-file partitions overlap only enough bytes to preserve line and pattern boundaries, then discard duplicate boundary matches during merge.
- Workers reuse bounded scratch buffers. Immutable inputs use shared ownership rather than one copy per worker, and oversized content spills to the session store.
- Cooperative cancellation is checked between partitions and during long scans. No operation can fill an unbounded queue while a downstream consumer is slow.
- Stable merge keys include the logical path byte order and operation-specific position, such as line, column, node identifier, or input index. Equal inputs therefore produce byte-identical canonical ASON regardless of worker completion order.

A hierarchical governor owns weighted permits for runnable nodes, child processes, filesystem descriptors, compute partitions, captured bytes, and retained bytes. System limits bound all sessions; a session limit bounds its programs; a program budget is reserved before execution; node limits are the final boundary. This prevents a wide graph from multiplying intra-operation parallelism until the host is oversubscribed.

## 6. Request lifecycle

1. The protocol gateway validates framing and message size.
2. The codec parses into typed protocol values with depth and collection limits.
3. The engine resolves the requested protocol version and operation schemas.
4. Paths and result aliases are resolved within the session namespace.
5. Capability and workspace-boundary checks run before side effects.
6. The graph is validated and normalized.
7. Budgets are reserved for nodes and retained output.
8. The scheduler acquires hierarchical permits and starts eligible nodes on the appropriate I/O or compute plane.
9. Events enter bounded internal streams and the session store.
10. Deterministic reducers produce the immediate result projection.
11. The ASON encoder emits a canonical response and any reference metadata.
12. Session quotas and cleanup are reconciled even after cancellation or failure.

## 7. Output pipeline

Output handling is part of the runtime contract, not cosmetic formatting.

### 7.1 Capture

Process stdout and stderr are captured independently as bytes. In-memory buffers are bounded. Overflow is streamed to the result store, subject to the program and session quotas. Backpressure must not deadlock a child process whose other stream is still active.

### 7.2 Classification

Captured data is classified as structured records, valid UTF-8 text, or opaque bytes. Raw bytes remain unchanged in storage. Text normalization is applied only to the LLM projection and records whether line endings changed.

### 7.3 Reduction

Reducers are explicit and deterministic. Initial reducer families are:

- status only;
- head, tail, or selected ranges;
- repeated-line and repeated-block collapse;
- literal or regular-expression filtering;
- error-focused process output;
- grouped search matches;
- compact path trees;
- changed-file summaries and patch hunks;
- projection and sorting of structured records.

Reducers cannot call a model. Their parameters and version are retained with the result metadata so a response can be reproduced.

### 7.4 Budget enforcement

The engine enforces byte and record ceilings before encoding. A negotiated tokenizer profile may add a stricter token ceiling at the presentation boundary. If no profile is available, the adapter supplies a conservative byte budget and performs final token accounting.

The program budget is allocated across nodes by explicit priority and deterministic defaults. A node cannot consume another node's reserved error budget.

### 7.5 References

If output is reduced or truncated, the response carries a numeric session reference. Follow-up `ref` operations can fetch a slice, search within it, apply another reducer, or materialize it as an artifact. Binary data is never embedded as Base64 in the default LLM response.

Reference readers hold short-lived leases. Early release is atomic and returns a typed conflict while any operation still owns a lease, preventing a concurrent inspection from emitting an alias that has already been retired.

## 8. Process execution

`exec` starts an executable directly with an argument vector. It never silently inserts `sh -c`, `cmd /c`, or `pwsh -Command`.

Inputs include:

- executable and `argv`;
- logical working directory;
- environment additions, replacements, and removals;
- stdin source or stream edge;
- timeout and cancellation policy;
- output reducer and retention policy;
- process and byte limits.

An explicit non-portable shell operation may be added later, but it must declare the requested dialect and cannot participate in portable conformance claims.

### 8.1 Lifecycle guarantees

- Cancellation propagates from session to program to node to process tree.
- Dropping the client closes the session and initiates bounded cleanup.
- Timeout and cancellation are different statuses.
- Exit code, signal, forced termination, spawn failure, and lost-process state are distinct.
- Output is drained during termination up to the remaining budget.
- No production path waits indefinitely for a child, pipe, or cleanup task.
- Concurrent child processes are bounded separately from compute partitions, preventing graph width from starving pipe readers or the host.

## 9. Filesystem semantics

Protocol paths use UTF-8 with `/` separators and are resolved relative to a workspace capability. The platform backend converts them to native paths.

Unix names that cannot be represented as UTF-8 are returned as opaque path references. They may be inspected or passed back to `ash`, but are never lossy-decoded into model text.

### 9.1 Reads

Reads support explicit byte or line ranges, expected content digests, multiple files per request, and a combined budget. Metadata is returned once per file, not once per slice.

### 9.2 Mutations

Mutations use compare-and-swap semantics. A caller supplies the digest or version observed during its read. A changed target returns a structured conflict instead of overwriting newer content.

Single-file replacement uses a same-directory temporary file, permission preservation where supported, flush, and atomic replacement. Multi-file operations use a journal with preimages or reversible moves. Because no operating system provides a general atomic multi-file transaction, the protocol reports `committed`, `rolled_back`, or `recovery_required` explicitly.

### 9.3 Path safety

Resolution checks lexical traversal, symlink or reparse-point traversal, workspace escape, and mutation target type. The backend must revalidate at the point of use to reduce time-of-check/time-of-use races.

## 10. Safety and approval

`ash` is a policy enforcement point for its own operations, not a claim of complete child-process isolation.

Capabilities are explicit and scoped, for example:

- workspace read;
- workspace write;
- create process;
- access outside workspace;
- recursive delete;
- environment secret access;
- network-capable process approval;
- non-portable host-shell execution.

An operation that requires external approval returns a compact `permit_required` error with a digest of the normalized action. The harness obtains approval and resubmits an opaque permit bound to that digest, session, expiry, and policy identity. `ash` itself does not display a human approval UI.

Secrets configured for redaction are removed from LLM projections before encoding. Retention of unredacted raw output is an independent policy choice and defaults to the narrowest local access permissions.

Telemetry is off by default. Local timing and token-accounting events may be retained for benchmarks only when explicitly enabled.

## 11. Cross-platform contract

### 11.1 Common semantics

- forward-slash logical paths;
- direct argument vectors;
- explicit environment deltas;
- normalized file kinds and timestamps;
- normalized process termination categories;
- consistent timeout, cancellation, truncation, and conflict codes;
- identical protocol fixtures.

### 11.2 Linux

The backend uses native process groups and signals, nonblocking pipes, and same-filesystem atomic replacement. Release binaries target musl for x86-64 and ARM64 and must not require system OpenSSL or another dynamically installed runtime.

### 11.3 macOS

The backend uses process groups and Darwin-native filesystem behavior. Separate Apple Silicon and Intel binaries are released. Packages are code-signed and notarized before promotion.

### 11.4 Windows

The backend uses `CreateProcessW`, explicit command-line encoding from the argument vector, Job Objects for process-tree ownership, native overlapped pipes, and Windows replacement semantics. Reparse points and `PATHEXT` are handled in the platform layer. Release binaries use the MSVC target for x86-64 and ARM64 and are Authenticode-signed.

### 11.5 Deferred PTY support

Version one supports non-interactive pipes only. Unix PTY and Windows ConPTY require a separate capability and protocol extension because their resizing, control sequences, signal behavior, and cancellation semantics differ from bounded machine execution.

## 12. Failure model

Errors contain a stable numeric code, operation-specific typed payload, retry classification, and optional retained evidence reference. They do not require prose to be actionable.

Error families include:

- malformed frame or unsupported version;
- invalid program or cycle;
- invalid argument or unsupported operation;
- path not found, wrong type, or workspace escape;
- capability denied or permit required;
- compare-and-swap conflict;
- executable not found or spawn failure;
- timeout, cancellation, or forced termination;
- output, storage, or session quota exceeded;
- partial filesystem commit or recovery required;
- internal invariant failure.

Partial graph results identify every node as pending, running, succeeded, failed, skipped, or cancelled. A program-level status never hides node-level evidence.

## 13. Versioning and compatibility

- The executable follows Semantic Versioning.
- The protocol uses an independent integer major and minor level.
- A major protocol change may alter message meaning or remove fields.
- A minor level may add operations, optional fields, error codes, or capabilities.
- Handshake negotiation selects the highest mutually supported level.
- Unknown optional fields are ignored; unknown required capabilities reject the request.
- ASON has its own format version because presentation rules can evolve without changing execution semantics.
- Canonical fixtures lock field order, quoting, column selection, and error encoding.

## 14. Repository layout

```text
ash/
|-- Cargo.toml
|-- rust-toolchain.toml
|-- crates/
|   |-- ash-protocol/
|   |-- ash-engine/
|   |-- ash-ops/
|   |-- ash-platform/
|   |-- ash-store/
|   `-- ash-cli/
|-- spec/
|   |-- ash-1.md
|   `-- fixtures/
|-- benches/
|   |-- tasks/
|   |-- baselines/
|   `-- tokenizers/
|-- fuzz/
|-- tests/
|   `-- platform-contract/
|-- assets/readme/
|-- docs/
|   `-- decisions/
|-- install.sh
|-- install.ps1
|-- xtask/
`-- .github/workflows/
```

The root is a Cargo workspace rather than an executable package. Public types cross crate boundaries only when ownership requires it. The CLI binary is assembled in `ash-cli` and published as the `ash` executable.

The repository remains independently buildable and releasable at `A3S-Lab/ash`. The A3S umbrella repository registers the same Git history as the `crates/ash` Git submodule; it does not copy the sources or absorb this workspace into its root package. A3S integration pins a tested ash commit, while standalone users receive the same release artifacts.

## 15. Verification strategy

### 15.1 Protocol

- canonical round-trip fixtures;
- malformed and oversized frame rejection;
- property tests for encode/decode stability;
- one-shot and framed ASON semantic equality;
- fuzzing for parser depth, quoting, columns, and references.

### 15.2 Engine

- deterministic graph scheduling with a fake clock;
- cancellation at every lifecycle boundary;
- budget reservation and exhaustion;
- partial failure and skip propagation;
- session disconnect cleanup.
- deterministic results across 1, 2, 4, 8, and host-default compute workers;
- nested graph and intra-operation parallelism without oversubscription;
- I/O progress and cancellation while every compute worker is occupied;
- bounded queue, descriptor, memory, and retained-byte pressure.

### 15.3 Platform

- the same process and filesystem contract suite on Linux, macOS, and Windows;
- paths containing spaces, Unicode, reserved names, symlinks, and reparse points;
- parent and grandchild process termination;
- concurrent stdout/stderr pressure;
- atomic replacement and conflict races.

### 15.4 Distribution

- clean-host installation without administrator privileges;
- PATH changes and idempotent reinstall;
- pinned and latest versions;
- checksum failure and interrupted download;
- upgrade, rollback, uninstall, and Windows running-binary replacement;
- signed artifact and installer receipt verification.

### 15.5 Agent efficiency

The benchmark contract in [benchmarks.md](./benchmarks.md) measures correctness, total tokens, retries, tool calls, latency, memory, retained bytes, CPU utilization, and multi-core scaling against native-shell baselines.

## 16. Delivery milestones

### M0: contracts and evidence harness

- freeze ASH/1 core types and ASON canonical rules;
- build protocol fixtures and parser fuzz targets;
- define the multi-platform task corpus and baseline recorder;
- establish release artifact names and installer contracts.

### M1: smallest end-to-end shell

- persistent session and one-shot paths;
- `exec`, `read`, `list`, and `search`;
- dual-plane runtime, hierarchical concurrency governor, and deterministic parallel merge;
- cancellation, deadlines, output budgets, reducers, and result references;
- x86-64 and ARM64 builds for all three operating systems;
- one-click installers tested on clean hosts.

### M2: coding mutation workflow

- compare-and-swap patching;
- filesystem mutation journals;
- snapshots and changed-file deltas;
- batch programs and dependency edges;
- permit binding and structured conflict recovery.

### M3: release hardening

- signed update manifests and rollback;
- soak, fault-injection, leak, and parser fuzz gates;
- benchmark publication with reproducible raw evidence;
- compatibility policy enforced in CI.

## 17. Fixed decisions

| Decision | Choice |
| --- | --- |
| Product position | AI Native Shell |
| Primary caller | Coding agent or agent harness |
| Core language | Rust |
| Runtime concurrency | Tokio I/O plane plus Rayon compute plane |
| Compute workers | Available host parallelism, explicitly bounded and configurable |
| Parallel output | Stable merge before canonical ASON encoding |
| Default integration | Persistent framed stdio |
| Fallback integration | One-shot `ash` process |
| Default execution | Direct executable plus argument vector |
| Internal model | Typed program DAG |
| LLM output | ASON canonical format |
| Session transport | Length-prefixed ASON |
| Retained output | Session-local content-addressed store |
| Platform set | Linux, macOS, Windows; x86-64 and ARM64 |
| Installation | `install.sh` and `install.ps1`, no admin by default |
| Global daemon | None required |
| Human shell compatibility | Out of scope |
| Embedded LLM | Out of scope |
| PTY in version one | Out of scope |

These decisions may be changed only through an explicit architecture decision that updates the protocol, benchmarks, and affected compatibility fixtures together.
