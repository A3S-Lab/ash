# ash system architecture

Status: architecture baseline plus implementation checkpoint

This document defines the intended architecture of `ash` and is normative for component ownership and runtime boundaries. Statements explicitly labeled as the current source checkpoint describe implemented behavior; the remaining contracts are design targets rather than release claims.

The current source checkpoint implements the Rust workspace, ASON and framed ASH/1 session, capability negotiation, session/action-bound one-time approval permits, dual Tokio/Rayon runtime, hierarchical governor, direct process execution with quota-bound disk-backed lossless retained output and conservative crash-orphan cleanup, deterministic byte-saving repeated-line, repeated-block, and failure-diagnostic projection, bounded read/list/search, durable compare-and-swap patching and file-only filesystem transactions with restart recovery, algebraic retained-result slicing/search/projection/release and safe artifact materialization, workspace snapshot/delta, cancellation, bounded batch DAGs with stable retained child evidence, strict signed-release verification/download/activation/recovery/rollback, deterministic format evidence, a locked seven-task cross-platform ASH/native-shell tool-plan corpus, a strict provider-neutral paired Agent trace validator/replayer, a stateless OpenAI Responses capture adapter with canonical raw audit binding, a twenty-two-scenario host-local runtime harness with three dual-stream capture profiles and paired disk I/O under idle versus fully occupied compute, four twice-weekly ASON/frame/update-metadata fuzz targets with bounded evolving corpora and source-bound evidence artifacts, deterministic six-target packaging, and a fail-closed native release workflow.
The accepted post-ASH/1 human frontend has completed H0 with an independent `a3s-ash-shell` crate, source-spanned simple-command parsing, persistent state types, deterministic command classification, locked parser/resolution fixtures, and provider-neutral raw read/list/search semantic services reused by the ASH/1 adapters. H1 now includes a feature-gated, line-edited `ash shell` REPL with configurable prompt, safety-checked persistent history, opt-in Profile startup, prompt Ctrl+C/EOF handling, and `exit [STATUS]`, alongside inline plus bounded stdin/native-file sources. One persistent state executes expanded `export`/`unset`, sequential `pwd`, `echo`, `cd`, portable `ls`, bounded raw-byte `cat`, bounded text `grep`, source-spanned `$NAME`/`${NAME}`/`$?` expansion with quote-aware fixed field splitting and native-string preservation, and native host executables launched through `ash-platform` with exact argument vectors, persistent cwd/environment, owned process trees, bounded dual-stream capture, and native status propagation. H2 has begun at the platform process boundary with an explicit `ProcessStdio` mode for each child stream and an eight-megabyte backpressure regression. The current shell still selects null stdin plus piped capture, so user-visible streaming, pipelines, foreground interactive programs and job control, broader expansion and mutations, and WSL launch remain open.
Release-key and platform-signing credential provisioning, the first published release, enough accumulated fuzz duration to claim a soak gate, captured real multi-model Coding Agent runs, medium/large task families, and published hardware-labelled runtime measurements remain open.

The runtime evidence covers fifteen end-to-end paths: recursive listing, literal and regular-expression search, snapshot, disk spill/fetch, paired disk-spill I/O with the compute plane idle and fully occupied, fresh `ash run` startup, empty child spawn, steady, fragmented, and paced-bursty simultaneous disk-backed stdout/stderr capture, repeated cancellation of a parent plus pipe-inheriting descendant, warm framed RPC dispatch, and retained ASON table projection. The mixed-load pair alternates sample order, consumes identical bytes, times only `capture` through asynchronous flush, and checks that every Rayon worker remains inside a bounded integer workload at I/O completion. It releases that workload before BLAKE3 retention and exact tail validation, so compute queueing cannot contaminate the I/O interval. Capture metadata records exact producer chunk cycles, flush boundaries, pacing, and output seeds; the compiled helper must independently describe the same profiles before timing starts. List and search split the deterministic fixture into disjoint roots so traversal and scanning can use the bounded worker pool. Structured projection reads and parses the retained value, applies ordered column reduction across the configured Rayon pool, and emits the same canonical row order at every worker count. Three compute-plane reducer scenarios separately process 131,072 deterministic lines as 512-line runs, eight-line blocks repeated 64 times, and sparse failure diagnostics with fixed context windows; all require the same compact evidence at every worker count. Four isolated single-caller scenarios additionally measure 4,096 hot lookups in a 1,024-entry path dictionary and DAG validation/scheduling at 64, 256, and 1,024 nodes. They report no scaling curve because they bypass the configurable engine worker pools. Cold startup launches and reaps a real shell process for every observation. Warm dispatch uses the same embeddable gateway as `ash rpc`, excludes its handshake, and keeps one session alive per worker configuration. Cancellation is recorded only after the owned native process group or Job Object has emptied and the final response is canonicalized; it is not merely a signal-delivery timer.

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

A post-v1, opt-in human frontend is accepted in
[Portable Human Shell Architecture](./portable-human-shell.md). It remains
separate from ASH/1 and does not change the version-one non-goals above.

## 2. Architectural principles

### 2.1 Optimize completed tasks, not individual strings

A representation that is shorter but causes more agent retries is a regression. Benchmarks must include the session primer, operation calls, responses, error recovery, and any follow-up reference reads.

### 2.2 Separate semantics from representation

Operations and results are strongly typed inside the runtime. ASON, CLI arguments, and future adapters are representations over the same semantic model. No representation owns execution behavior.

### 2.3 Reduce before encoding

Compression after serialization does not reduce LLM tokens. Projection, filtering, repeated-line and repeated-block collapse, path interning, diagnostic extraction, and result referencing happen before the LLM-facing encoder.

### 2.4 Make implicit shell behavior explicit

The default process operation accepts executable, argument vector, working directory, environment delta, timeout, input, and output policy. It does not perform glob expansion, variable interpolation, quoting, command substitution, or host-shell selection.

### 2.5 Bound every resource

Programs have limits for elapsed time, parallelism, child processes, bytes read, bytes retained, records emitted, and session storage. Every stream uses bounded memory and can spill to a quota-controlled store.

### 2.6 Preserve evidence

Reduction may omit data from the immediate response, but it must never silently destroy the ability to inspect that data. Reduced or truncated content carries a session result reference unless retention was explicitly disabled.

### 2.7 Parallelize work, not observable order

Independent graph nodes and splittable operations use all available CPU cores when the workload is large enough to repay scheduling cost. I/O readiness and CPU work run on separate executors so repository scans cannot delay pipe draining, cancellation, or RPC progress. Parallel completion order is never protocol order: records pass through a stable merge before reduction and ASON encoding, and concurrent request finals are sequenced by input order.

### 2.8 Express data work as algebra

Machine operations use small typed formulas instead of prose-shaped options or sparse unions. Selection, projection, slicing, release, and materialization have explicit arity and composition rules. For retained data, the semantic forms are `σ(q,R)`, `π_C(R)`, range slicing, `drop(@r)`, and `μ(path,@r)`; ASH/1 maps them to the single-byte mathematical operator set `/ # ? - | >`. Each symbol is the request opcode, so the operand vector needs neither a generic reference wrapper nor a second discriminator. The checked tokenizer matrix shows the symbol form at the same token and byte floor as direct ASCII letters and below a direct Greek-glyph form.

The formula is the semantic IR, not a string to evaluate dynamically. Rust represents each operator as an enum variant, schema validation proves its operands before dispatch, and canonical ASON is only the compact serialization. No unused mode fields or nullable operands survive into a request.

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
- canonical approval challenges and opaque permit wire values;
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

The engine depends on traits provided by the operation, platform, and store boundaries. It cannot contain target-specific conditional behavior. In the current checkpoint it owns the generic validated DAG scheduler, program leases, child-budget isolation, cancellation propagation, and hierarchical permits; `ash-ops` supplies the leaf-operation futures.

### 4.3 `ash-ops`

Owns portable operation semantics:

- `exec` — direct child-process execution;
- `read` — bounded byte and line slices across one or more files;
- `list` — directory enumeration, globbing, stat, and compact trees;
- `search` — literal and regular-expression search;
- `patch` — compare-and-swap file edits and live multi-file rollback;
- `batch` — bounded dependency graphs over heterogeneous leaf operations;
- `fs` — create, copy, move, and remove mutations;
- `snapshot` — workspace state and deltas;
- `/ # ? - | >` — algebraic byte slice, line slice, search, release, projection, and safe artifact materialization over stored results;
- `cancel` — explicit cancellation of a program or node.
- capability policy, approval challenge retention, permit verification, and replay rejection.

Operation implementations may use platform traits but cannot inspect the host shell or emit presentation text.

### 4.4 `ash-platform`

Owns native operating-system adapters:

- executable resolution and process creation;
- pipes, process groups, job objects, signals, and termination;
- filesystem metadata and path conversion;
- atomic replacement primitives;
- durable workspace transaction journals, cross-process mutation locking, and restart recovery;
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
- an embeddable framed RPC service over caller-owned async streams;
- installer receipt discovery;
- self-update, rollback, and uninstall coordination;
- machine-only bootstrap diagnostics.

The crate library owns argument routing, build identity, exit mapping, and the production RPC gateway. The binary target is only the Tokio process wrapper. Benchmarks and trusted embedders call the same gateway over caller-owned pipes, sockets, or in-memory transports; there is no benchmark-only dispatcher. The CLI translates arguments into protocol requests and renders protocol results. It does not duplicate operation logic.

### 4.7 `ash-update`

Owns the release trust boundary:

- canonical six-target release manifests and detached Ed25519 signatures;
- compile-time trust roots and key-set fingerprints;
- monotonically sequenced update and signed-rollback policy;
- SHA-256 archive and binary identity checks;
- exact-shape, size-bounded `.tar.gz` and `.zip` extraction;
- installation journals, candidate activation, health checks, and rollback.

The current checkpoint implements all of these boundaries. It rejects weak or unknown keys, noncanonical metadata, signature alteration, sequence rollback or equivocation, incomplete target matrices, archive traversal, links, duplicate or surplus entries, decompression beyond declared ceilings, mismatched embedded release metadata, unowned launchers, concurrent activation, and inconsistent recovery journals. Unix activation atomically replaces the active version link; Windows delegates running-binary replacement to the verified candidate with bounded retries. Health failure restores the prior executable and state before reporting failure.

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

The complete model admits typed edges:

- `control` — target becomes eligible after source reaches the required status;
- `stream` — source bytes flow into the target input with backpressure;
- `value` — target consumes a typed value or result reference;
- `artifact` — target receives a stored file or blob reference.

The graph must be acyclic. Cycles fail validation before any mutation or process creation.

The current `batch` schema implements control dependencies only. It validates the whole graph before dispatch, runs every ready node concurrently, skips only transitive descendants of a non-success result, and continues independent branches. Stream, value, and artifact bindings remain protocol extensions; they are not inferred from control edges.

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

Process stdout and stderr are captured independently as bytes and both pipes are drained concurrently through 16 KiB chunks. A stream remains in memory up to 4 MiB, then transitions to a session-private temporary file while retaining only a fixed head/tail ring for immediate projection. At that transition the complete prefix is charged, and every later chunk is charged before it is written, against the session result-store quota. Streams that require references are committed together only after both pipes close, so a quota, I/O, digest, or entry failure publishes neither a partial alias nor a misleading truncation result.

The spool never becomes the LLM-facing value. Rayon computes the full BLAKE3 identity through a bounded 4 MiB scratch buffer, content-addressed entries deduplicate before alias allocation, and byte-range formulas seek directly into disk-backed values. Consumers that require a complete value apply their own independent ceiling: 8 MiB for structured ASON, 64 MiB for process stdin, and 128 MiB for file materialization or patch content. A lease prevents early unlink while a consumer is active; explicit release or session drop removes the temporary file once the final lease ends. Unix spool directories and files use modes `0700` and `0600`; other platforms inherit their native per-user temporary-directory protections. If quota cannot cover every captured byte, execution returns typed storage-budget error `601` instead of discarding a tail.

Crash recovery is conservative and daemon-free. The first result-store construction in a process scans the operating-system temporary directory once. It removes a spool root only when the root has the current `ash` prefix, its exact versioned owner marker is at least one hour old, its regular lock file can be locked exclusively, and every entry is a recognized regular non-symlink marker, lock, or stream file. Removal is file-by-file and never recursive. Active, recent, malformed, symlinked, or foreign-content roots remain untouched; a scan failure never prevents creation of a new session spool. Consequently, a crash orphan becomes eligible on the first store construction of a later process after the grace period, without risking a live session.

### 7.2 Classification

Captured data is classified as structured records, valid UTF-8 text, or opaque bytes. Raw bytes remain unchanged in storage. Text normalization is applied only to the LLM projection and records whether line endings changed.

### 7.3 Reduction

Reducers are explicit and deterministic. Complete valid-UTF-8 process captures first normalize CRLF to LF, then replace a byte-saving consecutive run `line^N` with the first `line` followed by `×N`. Here `N` is the total run length, including the retained line. A run is eligible only when every line is LF-terminated and the marker is shorter than the omitted copies; short runs and a final unterminated line remain verbatim. Large inputs are partitioned on Rayon and merged in source order, so worker count cannot change the projection.

After line reduction, the block pass considers `B^N` for `2 <= K <= 32`, where `B` contains `K` LF-terminated projection lines. A winning run becomes one retained `B` plus `×N#K`. At each source position the candidate with greatest byte saving wins; ties choose smaller `K`, then larger `N`. Fixed-size candidate batches and exact repetition checks run on Rayon, results remain in source order, and hash fingerprints can accelerate discovery but never authorize omission. The original capture remains authoritative through a retained reference whenever normalization or either repetition reduction occurs.

For unsuccessful native exits, including a nonzero code or signal, a third pass classifies complete valid-UTF-8 stdout and stderr lines on Rayon. It keeps both two-line stream edges plus two lines before and six after every stable diagnostic anchor. Each remaining maximal gap becomes `⋯N` only if the marker is shorter than its `N` source lines. Window union and encoding are source-ordered, so worker count cannot change the result. Successful exits, timeout/cancellation evidence, opaque bytes, and disk-backed samples do not enter this pass. Any applied diagnostic reduction retains the complete original stream reference.

Current and planned reducer families are:

- status only;
- head, tail, or selected ranges;
- repeated-line, repeated-block, and failure-diagnostic window reduction (implemented);
- literal or regular-expression filtering;
- grouped search matches;
- compact path trees;
- changed-file summaries and patch hunks;
- projection and sorting of structured records.

Reducers cannot call a model. Process projection runs on the fixed compute pool after pipe capture; normal requests use the hierarchical compute permit, while timeout and cancellation finalization may enter that same bounded pool directly so typed termination evidence cannot be rejected by its already-cancelled permit. Reducer parameters and versions are retained with result metadata so a response can be reproduced.

### 7.4 Budget enforcement

The engine enforces byte and record ceilings before encoding. A negotiated tokenizer profile may add a stricter token ceiling at the presentation boundary. If no profile is available, the adapter supplies a conservative byte budget and performs final token accounting.

The program budget is allocated across nodes by explicit priority and deterministic defaults. A node cannot consume another node's reserved error budget. The current batch implementation divides token, record, and immediate-output capacity deterministically across nodes, inherits the parent deadline and cancellation token, and gives each child isolated counters plus a node-local path dictionary.

### 7.5 References

If output is reduced or truncated, the response carries a numeric session reference. Follow-up `ref` operations can fetch a slice, search within it, apply another reducer, or materialize it as an artifact. Binary data is never embedded as Base64 in the default LLM response.

Reference readers hold short-lived leases. Early release is atomic and returns a typed conflict while any operation still owns a lease, preventing a concurrent inspection from emitting an alias that has already been retired.

The current request IR is a closed data-formula enum rather than a mode record with optional fields. Structured projection computes `π_C(T[o:o+n])` over a bounded ASON table and then applies the negotiated record and output budgets. Materialization computes `μ(path,@r)` through the durable file transaction backend: it is workspace-confined, refuses overwrite, keeps the reference leased through commit, and requires both retained-result and workspace-write capabilities.

## 8. Process execution

`exec` starts an executable directly with an argument vector. It never silently inserts `sh -c`, `cmd /c`, or `pwsh -Command`.

Every platform process specification now selects `Null`, `Piped`, or `Inherit`
independently for stdin, stdout, and stderr. Only `Piped` exposes a corresponding
handle on `ProcessHandle`. The ASH/1 adapter preserves its contract by selecting
piped stdin only when a bounded input exists and always piping both output
streams; the current human-shell adapter selects null stdin and piped output.
Inherited handles are available to later shell plans but are not selected by a
machine request or claimed as foreground terminal support yet.

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

`ProcessHandle::terminate` is a completion boundary, not a fire-and-forget kill. Unix sends `SIGKILL` to the owned process group and waits through its reap path; Windows terminates the owned Job Object and waits for the job-empty completion event. `exec` then completes stdin and both pipe tasks before constructing its final response. A cancellation response therefore cannot be published while an owned descendant still holds an inherited stdout or stderr pipe.

## 9. Filesystem semantics

Protocol paths use UTF-8 with `/` separators and are resolved relative to a workspace capability. The platform backend converts them to native paths.

Unix names that cannot be represented as UTF-8 are returned as opaque path references. They may be inspected or passed back to `ash`, but are never lossy-decoded into model text.

### 9.1 Reads

Reads support explicit byte or line ranges, expected content digests, multiple files per request, and a combined budget. Metadata is returned once per file, not once per slice.

### 9.2 Mutations

Mutations use compare-and-swap semantics. A caller supplies the digest or version observed during its read. A changed target returns a structured conflict instead of overwriting newer content.

Single-file replacement uses a same-directory temporary file, permission preservation where supported, flush, and atomic replacement. Multi-file operations use a journal with preimages or reversible moves. Because no operating system provides a general atomic multi-file transaction, the protocol reports `committed`, `rolled_back`, or `recovery_required` explicitly.

The current patch vertical slice accepts sorted, non-overlapping byte splices over multiple existing regular files. It prepares and hashes replacement bytes in parallel, then revalidates every preimage under the workspace mutation lock. Before any replacement becomes visible, it persists the original files, staged replacements, and their exact sizes and BLAKE3 identities in the checksummed workspace journal. A later failure rolls visible replacements back in reverse order. After interruption, an uncommitted journal restores proven preimages while a durable commit marker finalizes cleanup; ambiguous or externally modified content is preserved as `recovery_required`.

The implemented `fs` operation is a separate durable, file-only transaction boundary. It supports create, copy, move, and remove over regular files; create and destination paths never overwrite, while copy, move, and remove require the caller's full BLAKE3 preimage digest. Requests contain 1 through 256 stable-ID actions, every source and destination is unique, and input hashing and staging are bounded per file and in aggregate. Directory creation, directory moves, overwrite, and recursive deletion are intentionally absent because they require additional capabilities and recovery rules.

Before the first mutation, the platform writes staged content and a versioned, checksummed binary manifest under the reserved workspace `.ash` state directory, flushes it, and atomically publishes the transaction journal. Actions then use reversible same-filesystem links, renames, or compare-and-swap replacements. A durable commit marker separates rollback recovery from committed cleanup. A later action failure rolls prior actions back in reverse order; after interruption, the next transaction infers each applied step from the manifest, exact file size, digest, native file identity, and journal layout. The create/copy and move windows in which both hard-link names exist are recoverable only when both names identify the same underlying file; independent equal-content files remain ambiguous. Every completed inverse mutation is itself a recognizable pre-transaction state, so a second crash can re-enter recovery safely. Ambiguous or externally modified state is preserved and reported as `recovery_required` instead of being guessed. A process-local mutex and an operating-system file lock serialize workspace mutations across cloned sessions and processes. Valid internal state is excluded from listings and snapshots and cannot be addressed through normal workspace operations.

### 9.3 Snapshots

A snapshot is a canonical, versioned ASON manifest bound to sorted roots, maximum depth, and visibility flags. Regular files carry streaming BLAKE3 identities, symlinks carry a digest of the target representation without traversal, and structural entries carry normalized kinds. Manifests are immutable session references; a delta validates the same capture scope and performs a stable ordered merge to emit only added, modified, and removed paths. Hash work partitions by file on the compute plane while fixed scratch buffers keep memory proportional to worker count rather than repository size.

### 9.4 Path safety

Resolution checks lexical traversal, symlink or reparse-point traversal, workspace escape, and mutation target type. The backend must revalidate at the point of use to reduce time-of-check/time-of-use races.

## 10. Safety and approval

`ash` is a policy enforcement point for its own operations, not a claim of complete child-process isolation.

ASH/1.0 has four explicit capability bits: workspace read, workspace write, direct host-process execution, and retained-result access. The handshake intersects the caller request with the server mask, then the session policy splits that result into direct grants and approval-required grants. Required bits are derived from the typed operation, and a batch is authorized once against the union of all leaf requirements before any node starts. Cancellation remains capability-free.

An operation that requires external approval returns a compact structured denial with a retained canonical challenge. The normalized action digest covers the operation, typed arguments, and required capability mask while excluding request ID, budget, and permit. The trusted harness obtains approval and resubmits a fixed-size opaque permit bound by keyed BLAKE3 to that action, session ID, fresh per-session binding, expiry, policy identity, capabilities, and nonce. Successful verification consumes the nonce before dispatch; expiry, alteration, cross-session use, cross-action use, and replay fail closed without invoking the operation.

The authority secret and fresh session binding are injected by the embedding harness and never cross ASH/1. `ash` does not display a human approval UI. The standalone CLI directly grants only negotiated capabilities; embedders select approval-required capabilities through the public `ash-ops` policy API. Policies that retrieve challenges through the protocol also grant retained-result access.

These capabilities constrain native ASH operations, not arbitrary syscalls made by a child. In particular, host-process capability gives the child the operating-system authority inherited from `ash`; callers that need stronger isolation must deny or externally approve it and place `ash` inside an OS sandbox or container.

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

Final graph results identify every node as succeeded, failed, skipped, or cancelled. Pending and running are lifecycle event states reserved for streaming progress. A program-level status never hides terminal node evidence: every executed child has a retained full response, while a skipped child has no fabricated result.

## 13. Versioning and compatibility

No supported release exists. Before the first supported tag, `main` may replace protocol fields, formulas, fixtures, and report schemas directly when the replacement is safer or cheaper. CI enforces one current canonical contract; it does not carry migration code for unreleased revisions. The rules below become a compatibility commitment only when the first release is published.

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
|   |-- ash-update/
|   `-- ash-cli/
|-- spec/
|   |-- ash-1.md
|   `-- fixtures/
|-- benches/
|   |-- corpus/
|   |-- runner/
|   |-- reports/
|   `-- tasks/v1/
|       |-- manifest.json
|       |-- lock.json
|       `-- workspaces/
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

The root is a Cargo workspace rather than an executable package. Public types cross crate boundaries only when ownership requires it. The CLI library and thin binary target are assembled in `ash-cli`; the binary is published as the `ash` executable.

The repository remains independently buildable and releasable at `A3S-Lab/ash`. The A3S umbrella repository registers the same Git history as the `crates/ash` Git submodule; it does not copy the sources or absorb this workspace into its root package. A3S integration pins a tested ash commit, while standalone users receive the same release artifacts.

## 15. Verification strategy

### 15.1 Protocol

- canonical round-trip fixtures;
- malformed and oversized frame rejection;
- property tests for encode/decode stability;
- one-shot and framed ASON semantic equality;
- fuzzing for frame length/truncation/canonicality plus parser depth, quoting, columns, and references;
- arbitrary-signature update rejection plus validly signed version, rollback, sequence, target, and artifact-state fuzzing;
- twice-weekly AddressSanitizer runs with bounded evolving corpora, per-file and log hashes, final statistics, retained findings, and a workflow artifact digest.

### 15.2 Engine

- deterministic graph scheduling under controlled completion permutations;
- exhaustive dependency/failure propagation over every four-node forward DAG and success mask;
- lowest-input-index task-error selection after already-running independent work drains;
- cancellation at every lifecycle boundary;
- budget reservation and exhaustion;
- partial failure and skip propagation;
- session disconnect cleanup.
- deterministic results across 1, 2, 4, 8, and host-default compute workers;
- nested graph and intra-operation parallelism without oversubscription;
- I/O progress and cancellation while every compute worker is occupied;
- bounded queue, descriptor, memory, and retained-byte pressure.
- active/recent/foreign spool rejection plus proven crash-orphan reclamation.

### 15.3 Platform

- the same process and filesystem contract suite on Linux, macOS, and Windows;
- paths containing spaces, Unicode, reserved names, symlinks, and reparse points;
- parent and grandchild process termination;
- concurrent stdout/stderr pressure;
- repeated cancellation-to-tree-empty completion with inherited pipes;
- atomic replacement and conflict races.
- actual durable transaction execution at 30 preparation, publication, action, and commit cutpoints;
- recovery re-entry at 12 inverse-mutation and journal-cleanup cutpoints;
- corrupt journals, external interference, cancellation, and bounded recovery reads.

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

- define the current ASH/1 core types and ASON canonical rules; freeze them only with the first supported release;
- build protocol fixtures and parser fuzz targets;
- maintain the implemented locked cross-platform ASH/native-shell task runner and paired model-trace replay contract, then expand its seven small contracts to the complete medium/large task-family matrix and capture real model-selected traces;
- establish release artifact names and installer contracts.

### M1: smallest end-to-end shell

- persistent session and one-shot paths;
- `exec`, `read`, `list`, and `search`;
- dual-plane runtime, hierarchical concurrency governor, and deterministic parallel merge;
- cancellation, deadlines, output budgets, reducers, and result references;
- x86-64 and ARM64 builds for all three operating systems;
- one-click installers tested on clean hosts.

### M2: coding mutation workflow

- compare-and-swap patching (implemented in the source checkpoint);
- filesystem mutation journals (implemented in the source checkpoint);
- snapshots and changed-file deltas (implemented in the source checkpoint);
- batch programs and control dependency edges (implemented in the source checkpoint);
- permit binding (implemented in the source checkpoint) and structured conflict recovery.

### M3: release hardening

- signed update manifests and rollback;
- soak, fault-injection, leak, and parser fuzz gates;
- benchmark publication with reproducible raw evidence;
- one canonical pre-release fixture set in CI, followed by an explicit compatibility policy at the first supported tag.

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
| Retained output | Session-local content-addressed memory/disk store with bounded range reads |
| Platform set | Linux, macOS, Windows; x86-64 and ARM64 |
| Installation | `install.sh` and `install.ps1`, no admin by default |
| Global daemon | None required |
| Human shell compatibility | Out of scope for ASH/1 v1; accepted as a separate post-v1 frontend |
| Embedded LLM | Out of scope |
| PTY in version one | Out of scope |

Before the first supported release, these decisions may be changed through an explicit architecture decision that updates the protocol, benchmarks, and current fixtures together. A published release adds the compatibility obligations defined in section 13.
