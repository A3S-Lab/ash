# Token-efficiency benchmark contract

Status: deterministic format corpus, two-tokenizer representation plus repeated-line/block/error-focus reduction evidence, retained-formula regression gate, a locked seven-task cross-platform ASH/native-shell tool-plan corpus, and a twenty-two-scenario host-local runtime harness; Coding Agent results and published hardware reports remain open

Token reduction is the primary performance objective of `ash`. This document defines how it is measured without trading away task correctness or hiding protocol overhead.

The checked-in [v0.1.0 format report](../benches/reports/v0.1.0/format.json) is generated from a versioned synthetic corpus and pinned `cl100k_base` and `o200k_base` vocabularies. It compares canonical ASON with semantically equivalent compact row-object and columnar JSON, and separately compares four equivalent retained-formula syntaxes. It is deliberately limited to representation cost: no agent success, native-shell task, latency, or multi-core claim is inferred from it.

The current corpus records 6,313 `cl100k_base` tokens and 6,312 `o200k_base` tokens for ASON, versus 10,192 and 10,198 for compact row-object JSON. That rounds up to 62% in both profiles. The checked gate is a 65% regression ceiling; the proposed 50% release target below remains unachieved and unchanged. Columnar JSON is also reported rather than hidden, and is closer at 6,807 and 6,909 tokens.

For retained formulas, the report measures the former ASCII wrapper (`o:h` plus an inner discriminator), direct Greek glyphs, direct ASCII letters, and the canonical keyboard-math operators `/ # ? - | >`. Across byte slice, line slice, search, release, projection, and materialization, the canonical form is 126 bytes and 80 tokens in both tokenizers. It matches the direct-letter token floor, improves on Greek's 132 bytes and 86/86 tokens, and uses 84% of the wrapper bytes plus 83%/82% of its tokens. The checked ceiling is 85% in every profile.

The same deterministic report exercises the production `×N` reducer over 8,192 diagnostic lines in 64 consecutive runs. The immediate projection contains 128 lines: one source line and one count marker per run. It is 4,864 bytes and 1,408 tokens in either pinned tokenizer, versus 573,440 bytes and 155,648 tokens for the retained source. All three rounded ratios are 1%, below the checked 5% ceiling; this measures projection cost only and never substitutes for the exact retained evidence.

Its independent `×N#K` gate exercises 32 six-line diagnostic blocks repeated 64 times. The 12,288-line retained source becomes a 224-line projection containing each block once plus its marker. The projection is 14,432 bytes and 4,384 tokens in either tokenizer, versus 909,312 bytes and 270,336 tokens for the source. All three rounded ratios are 2%, below the same checked 5% ceiling.

The failure-diagnostic `⋯N` gate places 32 error anchors into 8,192 otherwise unique lines. Fixed stream edges and `[-2, +6]` context windows retain 325 projection lines while 33 markers account for 7,900 omitted lines. The projection is 22,135 bytes, 6,913 `cl100k_base` tokens, and 6,880 `o200k_base` tokens, versus 622,304 bytes and 194,200/194,234 tokens for the source. Every rounded ratio is 4%, below the same 5% ceiling. The production `exec` path enables this pass only for unsuccessful native exits and retains the exact stream bytes.

Reproduce the byte-identical report with:

```sh
cargo run -p a3s-ash-bench --release --locked -- \
  --check benches/reports/v0.1.0/format.json
```

The same runner creates deterministic workspace, retained-value, native-process, and framed-transport fixtures. Build the real shell binary first; the runner selects the same-profile sibling by default, or accepts an explicit path after `--runtime`:

```sh
cargo build -p a3s-ash --release --locked
cargo run -p a3s-ash-bench --release --locked -- --runtime
# Cross-profile or external binary:
cargo run -p a3s-ash-bench --release --locked -- --runtime ./path/to/ash
```

The schema-14 report includes every observation, p50/p95/p99 nanoseconds, item and byte throughput, selected compute and I/O workers, host OS/architecture/available CPU count, per-scenario input digest, and output digest. It carries typed process-capture metadata: the two-stream byte count, fetched tail length, producer chunk cycle, flush policy, burst boundary, pause, profile seed, and a digest of the canonical profile descriptor. It also records the paired mixed-I/O boundary, CPU load, block size, all-worker-active proof, timeouts, and saturated-versus-idle p50 ratio for every worker configuration. The compiled fixture executable describes its own capture configuration before timing; any difference from the report metadata fails the run. Scenarios where worker count is a scaling variable report speedup and parallel efficiency in basis points. The mixed-load pair changes the amount of adversarial load with worker count, while fresh CLI startup and isolated primitives have one caller; those scenarios keep both scaling fields `null`. Recursive listing reports zero byte throughput because it reads metadata rather than file content. Stable evidence bytes are compared across warm-up, samples, and applicable worker counts; a difference fails before timing is printed. Host timings are not checked in or gated because shared-runner performance is not portable.

The versioned task seed is independently locked and executable on all three operating systems:

```sh
cargo run -p a3s-ash-bench --locked -- \
  --check-task-lock benches/tasks/v1/lock.json
cargo run -p a3s-ash-bench --locked -- --tasks
```

`manifest.json` contains seven tasks: source-marker search, compiler-diagnostic aggregation, an exact worker-limit patch, recursive source listing, ordered multi-file reading, a guarded copy/remove transaction, and an independent two-node search graph. It names objectives, least-privilege operation sets, output policy, hard limits, declarative expected output/files, a declarative ASH plan, and a native command for Linux, macOS, and Windows. The generated schema-2 lock binds the complete manifest, each initial visible tree, and each expected final visible tree.

The runner makes two isolated copies of the same fixture. One executes the current platform's native-shell baseline with bounded pipe drain and process cleanup. The other opens the production `ExecutionSession`, builds each typed request, canonicalizes it to ASON, decodes that document through the protocol validator, and executes it through the normal engine and portable operations. Read-derived BLAKE3 digests feed guarded `patch` and `fs` requests without benchmark-side file access. Both paths must match the declared semantic output, expected files, and locked final visible tree; ASH's reserved `.ash` state is excluded by the same visibility rule used by production listing and snapshots.

The native-shell total is objective + command + stdout + stderr. The ASH total is the same objective + every canonical request + every canonical response; each step and each length-delimited transcript is separately hashed. The embedded session handshake is not LLM-facing and is not tokenized here; a future Agent report must add amortized primer and format-instruction cost. ASH elapsed time does include engine/session construction, canonical encode/decode, execution, response encoding, and session close. Request budgets are fixed by the manifest, so repeated runs produce stable transcripts. The report labels itself `deterministic-tool-plan` and sets `agent_results` to false: a human-authored plan is useful for protocol accounting and correctness, but it does not execute a model, measure retries caused by a model, or establish an Agent-task Token reduction claim. These tiny tasks currently expose structured metadata and digest overhead; no comparative Token gate is applied. Host-local elapsed time is printed by `--tasks` but not committed.

## 1. Optimization target

The primary metric is **total agent interaction tokens per successfully completed task**.

It includes:

- amortized session and format instructions;
- every operation request;
- streaming events;
- final results and errors;
- follow-up reads from result references;
- retries caused by malformed requests or misunderstood output;
- recovery calls after conflicts, timeouts, or partial failure.

It excludes hidden model reasoning that cannot be measured consistently across providers. Wall time, tool calls, bytes, and runtime resources are secondary metrics.

## 2. Correctness comes first

A task is successful only when its machine-verifiable final state matches the fixture. Short output that causes an incorrect edit, misses an error, or requires an evaluator to infer intent is a failure.

Results are reported in this order:

1. task success rate;
2. total tokens per successful task;
3. retries and tool calls;
4. wall-clock latency;
5. runtime CPU, memory, and retained bytes.

Token comparisons are invalid when the compared systems have materially different task success.

## 3. Baselines

Each task defines equivalent native-shell procedures:

- Linux: a documented POSIX or Bash command sequence using common installed tools;
- macOS: the equivalent native command sequence and system tool behavior;
- Windows: PowerShell with native Windows commands;
- structured baseline: compact JSON produced by an adapter with the same semantic data, when applicable.

The baseline includes command text, stdout, stderr, formatting, retries, and any platform-specific discovery the agent must perform.

No baseline may receive less task context, a different repository state, or a more favorable output limit than `ash`.

## 4. Corpus

The implemented v1 corpus covers seven small deterministic contracts across workspace discovery/listing/reading, search, diagnostics, exact patching, file transactions, and batch graphs. The target corpus still expands those seeds to medium and large fixtures in these families:

### 4.1 Workspace discovery

- enumerate a shallow repository;
- inspect a deep repository with ignored build directories;
- locate manifests and entrypoints;
- identify files changed since a snapshot.

### 4.2 Search

- literal symbol search;
- regular-expression search;
- many duplicate matches;
- a result set exceeding the immediate token budget;
- binary and non-UTF-8 path encounters.

### 4.3 Reading

- read one exact range;
- read several ranges across files;
- recover context around search matches;
- fetch a second slice from a retained result;
- detect a content change between read and mutation.

### 4.4 Process execution

- silent success;
- short failure;
- verbose successful build;
- verbose compiler diagnostics;
- interleaved stdout and stderr;
- repeated progress lines;
- timeout and cancellation;
- descendant process cleanup.

### 4.5 Coding mutation

- apply one exact patch;
- patch several files;
- handle a compare-and-swap conflict;
- recover from one failed file in a mutation journal;
- verify the resulting workspace delta.

### 4.6 Tests and diagnostics

- one failing unit test in a large suite;
- several distinct failures;
- repeated stack frames;
- success with noisy framework output;
- test retry after a targeted edit.

### 4.7 Batch and graph execution

- parallel independent searches;
- build after generated-file validation;
- skip dependent nodes after failure;
- pipe process output without materializing it in the model context.

## 5. Task fixture

The implemented seed manifest contains:

```text
task id
task family
initial workspace fixture and locked digest
agent objective
allowed capabilities
time and resource limits
expected stdout, stderr, and exact file content
declarative ASH operation plan and semantic answer extractor
Linux, macOS, and Windows native-shell baseline definitions
output-retention policy
```

Its lock adds the manifest digest and the expected complete visible-tree digest. The schema-2 local report already pins the selected deterministic ASH request/response trace, per-step hashes, both tokenizer totals, and the native denominator. Later Agent-task reports must additionally pin model/prompt configuration, model-selected traces, retries, verifier version, and raw normalized evidence. A compressed workspace archive becomes necessary when fixtures are published outside this source tree; it is not simulated for the current small checked-in directories.

Fixtures must not depend on an external network, current package registry state, wall-clock date, or unpinned tool version unless the task explicitly measures those conditions.

## 6. Model and tokenizer matrix

The benchmark configuration names:

- model or deterministic agent policy;
- exact model/version identifier when available;
- tokenizer implementation and digest;
- context and output limits;
- temperature and sampling controls;
- system instructions and tool schemas;
- number of repetitions and random seeds.

ASON microbenchmarks run across multiple tokenizer families because a short byte sequence is not necessarily a short token sequence. Runtime acceptance uses the active supported tokenizer profiles; model-independent reports also include UTF-8 bytes and Unicode scalar counts.

## 7. Protocol accounting

### 7.1 Session primer

The ASH/1 and ASON primer is counted once per session and amortized across the tasks executed in that session. Reports include both cold single-task cost and warm multi-task cost.

### 7.2 Path dictionary

The first introduction of a path is counted. Later numeric references are counted normally. A benchmark cannot preload a dictionary with fixture-specific paths.

### 7.3 Retained results

Returning `@7` is cheap, but later reads of `@7` are part of the task cost. Reports include retained bytes and the number of references never used so that aggressive deferral cannot hide unusable results.

### 7.4 Errors and retries

Malformed ASON, invalid operation arguments, misunderstood numeric codes, and unnecessary reference fetches are charged to the system that produced them.

## 8. ASON format benchmarks

Format-only microbenchmarks compare canonical ASON with compact JSON and operation-specific positional JSON over the same typed values.

Datasets include:

- flat status records;
- homogeneous search matches;
- compiler diagnostics;
- file metadata tables;
- path-heavy directory trees;
- irregular errors;
- nested batch results;
- highly escaped source excerpts.

Measured values:

- encoded bytes;
- tokens by tokenizer profile;
- parse and encode throughput;
- peak parser allocation;
- malformed-input rejection time;
- model reconstruction accuracy for the typed value;
- model request-generation accuracy.

Canonical ASON rules may change before ASH/1 freezes if a shorter representation increases retries or reconstruction errors.

The runner gates all six retained-result formulas twice: the earlier direct ASCII formula beats the sparse-union shape, and the current direct mathematical operator form beats the former wrapper while matching the direct-ASCII token floor. The corpus contains byte slice, line slice, search, release, projection, and materialization requests. Reproduce the focused gates with:

```sh
cargo test -p a3s-ash-bench reference_formulas_beat_the_sparse_union
cargo test -p a3s-ash-bench direct_math_symbols_beat_wrappers_and_match_the_ascii_token_floor
```

## 9. Runtime benchmarks

The implemented host-local slice exercises fifteen end-to-end runtime paths over deterministic inputs:

- `list-recursive` walks 16 disjoint fixture roots on bounded workers, selects files, performs stable merge and path interning, validates one output record per fixture file, and encodes the final ASON response without claiming content-byte throughput;
- `search-literal` walks the same roots, reads files on bounded workers, finds one fixed literal per file, validates the exact match count, performs stable merge and path interning, and encodes the final ASON response;
- `search-regex` compiles an anchored regular expression, walks and scans the same roots, validates one match per file and the same canonical evidence as literal search, and includes compilation in the timed operation;
- `snapshot-blake3` walks the same roots, hashes files in the Rayon pool, builds and retains the canonical manifest, and encodes the final ASON response;
- `result-store-spill-fetch` captures 8 MiB in 16 KiB chunks with a 4 MiB memory ceiling, proves disk residency, hashes and atomically retains the value through the compute pool, fetches its final 64 KiB range, releases the alias, and tears down the session spool;
- `io-spill-idle-compute` uses a zero-byte memory ceiling and times only 8 MiB of 16 KiB asynchronous appends plus the final disk flush while the Rayon pool is idle. Hashing, retention, final 64 KiB range fetch, release, and teardown remain outside this paired timer;
- `io-spill-saturated-compute` repeats the exact input and timed boundary while every configured Rayon worker executes a bounded-block xorshift64 workload. Every lane completes at least one block before timing, all lanes must remain active when the flush completes, and the load is stopped and joined before the same retained-tail validation. Samples alternate idle/saturated order; the report compares their p50 values directly and applies no cross-host threshold;
- `cli-cold-startup` starts the selected real `ash run` executable for every observation, sends one canonical request, and measures from immediately before OS spawn until process exit and complete stdout/stderr drain; its input digest binds the executable bytes, arguments, and request;
- `exec-spawn-empty` launches a silent success fixture through normal engine admission, the hierarchical governor, and the native process owner, then validates the typed exit evidence;
- `exec-capture-pressure` runs two unpaced native producer threads with a 16 KiB write cycle, each emitting 8 MiB to stdout or stderr; it proves both complete streams crossed the 4 MiB disk boundary, verifies their deterministic final 64 KiB ranges, and measures admission through canonical response encoding;
- `exec-capture-fragmented` repeats the producer-write cycle `[1, 7, 31, 257, 4093, 16384, 65521]`, flushing every write. Its profile-specific bytes prove that this path—not the steady fixture—ran while exercising highly skewed pipe fragments through the same exact retained-tail checks;
- `exec-capture-bursty` repeats `[512, 4096, 16384, 65536]`, clips writes at each 256 KiB burst boundary, flushes, and pauses for 2,000 microseconds before the next burst. This adds a paced output-rate shape without converting a host-local observation into a fixed throughput claim;
- `exec-cancel-tree-empty` runs a parent that spawns a pipe-inheriting descendant, waits for the descendant PID marker, then measures from `Session::cancel` until the native process group or Job Object is empty, inherited pipes reach EOF, the request unregisters, and the canonical cancelled response is encoded;
- `rpc-warm-dispatch` starts the production RPC gateway over an in-memory duplex transport, completes the real ASH/1 handshake outside the timed interval, then measures full framed request/response round trips through decode, admission, stable response encoding, flush, and client decode on the same warm session;
- `ref-project-structured` retains a canonical 16,384-row, eight-column ASON table before timing, then measures retained-value read, bounded decode, ordered six-column projection on the configured Rayon pool, and canonical encoding; exact full-row validation follows the timer, and every worker count must match.

Three compute-plane reducers and four isolated primitive scenarios complete the schema-14 report:

- `reduce-repeated-lines` constructs 131,072 diagnostic lines in consecutive 512-line runs, then times the production ordered Rayon reducer at every configured compute-worker count; exact compact text, 256 collapsed runs, 130,816 omitted lines, and encoded evidence are validated outside the timer;
- `reduce-repeated-blocks` constructs 256 distinct eight-line blocks, repeats each 64 times for 131,072 source lines, and times candidate hashing, ordered parallel search, byte-exact verification, and projection; validation requires 256 collapsed blocks, 16,128 omitted repetitions, 129,024 omitted lines, and byte-identical evidence at every worker count;
- `reduce-error-focused` places 256 diagnostic anchors into a separate 131,072-line failure log, then times parallel anchor classification plus source-ordered window union and `⋯N` encoding; validation requires 256 anchors, 257 omitted spans, 128,764 omitted lines, and byte-identical evidence at every worker count;

- `path-dictionary-hot` first populates 1,024 canonical paths, then times batches of 4,096 existing-path lookups; every sample must return the same identifiers without introducing another mapping or changing dictionary size;
- `dag-schedule-64`, `dag-schedule-256`, and `dag-schedule-1024` form eight independent dependency chains, then time complete graph validation and ready-node scheduling with successful no-op jobs; graph construction and evidence encoding remain outside the timed interval, while every node must complete in stable input order.

List, search, snapshot, store, mixed-I/O, direct-process, and reference-projection matrix samples use fresh execution sessions so aliases, dictionaries, budgets, cancellation registrations, and spools start from the same state. Fixture and request creation, engine construction, session creation, structured-source retention, reducer-input construction, and reduction evidence encoding are outside their timed intervals; regular-expression compilation, structured ASON parsing, and each complete reduction remain timed. The compute-only reducers use the configured Rayon worker count; their reported I/O worker count is configuration metadata and no I/O work enters the timed interval. The path-dictionary and DAG primitives run on the benchmark's current thread rather than an ASH engine pool, so their `compute_workers` and `io_workers` fields are both one and their scaling fields are `null`. Cold CLI observations use a fresh OS process and include startup, request I/O, shutdown, and pipe drain; a 10-second bound kills and reaps a stuck child. Warm RPC keeps one production service session per worker configuration, excludes only service creation and handshake, and closes it within the same bound after sampling. List, search, snapshot, child spawn, process capture, and structured projection time admission through canonical response encoding. General store timing includes capture through range fetch, release, and spool teardown. The mixed-I/O pair instead times only zero-memory-ceiling capture through asynchronous flush; saturation setup, release, hashing, validation, and cleanup are outside the interval. Cancellation timing begins only after the descendant exists and ends after tree wait plus canonical response encoding. The benchmark has no fixed speedup, mixed-load ratio, startup, dispatch, spawn, cancellation, projection, reduction, dictionary, or scheduler threshold: it proves stable evidence and bounded completion, then reports current-host measurements.

Schema 14 reports p50, p95, p99, sample count, host OS/architecture/available CPU count, selected or actual single-caller workers, work volume, input and output digests, output size, throughput, and every raw observation. Speedup and parallel efficiency are present only when worker count represents more execution capacity for the same work. Paired load comparisons report their direct ratio instead. Future scenarios must add peak resident memory and CPU utilization where those values are material to the claim. Results from different hosts are not combined into one scaling curve.

Parallel and sequential runs consume the same input digest and must emit byte-identical evidence. List, literal and regular-expression search, snapshot, cold CLI, warm RPC, and structured projection outputs are canonical ASON; store, mixed-I/O, and process-capture outputs are verified ranges; child spawn and cancellation outputs bind typed termination evidence after cleanup. The idle and saturated I/O pair shares one input digest and one output digest, while runtime assertions prove every requested compute lane was active through saturated flush completion and stopped before retention. Each process-capture profile has distinct deterministic source bytes, and its input digest binds the compiled helper source plus the selected profile. After projection timing ends, the harness checks the exact selected columns and every row against independently constructed evidence. Repetition validation checks exact marker text, collapsed run/block counts, omitted repetition and line counts; diagnostic-focus validation checks exact windows, anchor/span/line counts, and `⋯N` markers. Other primitive outputs bind the exact path identifiers or ordered completed-node identifiers outside the timed interval. Literal and regular-expression search additionally must emit the same evidence for their intentionally equivalent queries. A faster run with reordered, missing, duplicated, skipped, or changed evidence is a correctness failure rather than a performance result. Future fixtures still need fewer large files, skewed directory trees, binary files, ignored paths, sparse and dense matches, and combined process fan-out plus reducer pressure.

## 10. Proposed release gates

These are engineering targets, not achieved claims:

- Task success is no more than one percentage point below the strongest comparable baseline.
- Median total tokens per successful task are at most 50% of the native-shell baseline across the full corpus.
- Search, repository tree, verbose build, test failure, and diff families target at most 30% of baseline tokens.
- Warm protocol dispatch p95 adds less than 1 ms, excluding the operation itself.
- Saturating the compute pool does not violate the RPC, pipe-drain, or cancellation latency gates.
- Multi-core search and hashing show positive scaling on the published release hosts; exact speedup becomes a release gate only after stable infrastructure evidence exists.
- Cancellation leaves no owned process after the platform-specific cleanup deadline.
- No immediate result exceeds its declared record, byte, or token budget.
- Every truncated result has a usable reference unless retention was explicitly disabled.
- All three operating systems pass the same semantic contract suite.

A gate is promoted from target to requirement only after the benchmark runner proves it is stable on release infrastructure.

## 11. Statistical rules

- Run enough repetitions to report confidence intervals for variable agent tasks.
- Publish failures and timeouts; do not calculate token averages only from favorable runs.
- Use paired initial workspaces and task order randomization.
- Separate format improvements from model, prompt, and tool-version changes.
- Mark regressions by task family, not only aggregate score.
- Retain raw ASON frames and normalized semantic traces with secrets removed.

## 12. Evidence layout

```text
benches/
|-- corpus/v1.json
|-- runner/
|-- tasks/v1/
|   |-- manifest.json
|   |-- lock.json
|   `-- workspaces/
|-- reports/v0.1.0/
|   |-- format.json
|   `-- README.md
|-- tokenizers/<profile>/
`-- schemas/
```

Future model-task and published runtime reports add `summary.json`, `runs.jsonl`, and `environment.json` beside their versioned report README. Larger task versions may package immutable workspace archives and standalone verifiers under their version directory. Published summaries link to raw versioned evidence. README marketing copy may quote a benchmark only after the corresponding report is available and reproducible.
