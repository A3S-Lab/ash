# Token-efficiency benchmark contract

Status: deterministic format corpus, two-tokenizer representation evidence, retained-formula regression gate, and a five-scenario host-local runtime harness covering search, snapshot, disk spill/fetch, dual-stream process capture, and process-tree cancellation; agent-task and published hardware reports remain open

Token reduction is the primary performance objective of `ash`. This document defines how it is measured without trading away task correctness or hiding protocol overhead.

The checked-in [v0.1.0 format report](../benches/reports/v0.1.0/format.json) is generated from a versioned synthetic corpus and pinned `cl100k_base` and `o200k_base` vocabularies. It compares canonical ASON with semantically equivalent compact row-object and columnar JSON, and separately compares four equivalent retained-formula syntaxes. It is deliberately limited to representation cost: no agent success, native-shell task, latency, or multi-core claim is inferred from it.

The current corpus records 6,313 `cl100k_base` tokens and 6,312 `o200k_base` tokens for ASON, versus 10,192 and 10,198 for compact row-object JSON. That rounds up to 62% in both profiles. The checked gate is a 65% regression ceiling; the proposed 50% release target below remains unachieved and unchanged. Columnar JSON is also reported rather than hidden, and is closer at 6,807 and 6,909 tokens.

For retained formulas, the report measures the former ASCII wrapper (`o:h` plus an inner discriminator), direct Greek glyphs, direct ASCII letters, and the canonical keyboard-math operators `/ # ? - | >`. Across byte slice, line slice, search, release, projection, and materialization, the canonical form is 126 bytes and 80 tokens in both tokenizers. It matches the direct-letter token floor, improves on Greek's 132 bytes and 86/86 tokens, and uses 84% of the wrapper bytes plus 83%/82% of its tokens. The checked ceiling is 85% in every profile.

Reproduce the byte-identical report with:

```sh
cargo run -p a3s-ash-bench --release --locked -- \
  --check benches/reports/v0.1.0/format.json
```

The same runner creates deterministic workspace, retained-value, and native-process fixtures. It executes public literal-search and BLAKE3 snapshot paths, forced disk spill/fetch, simultaneous stdout/stderr pressure, and process-tree cancellation at 1, 2, 4, 8, and host-available worker counts:

```sh
cargo run -p a3s-ash-bench --release --locked -- --runtime
```

The schema-4 report includes every observation, p50/p95/p99 nanoseconds, item and byte throughput, speedup and parallel efficiency in basis points, worker count, host OS/architecture/available CPU count, per-scenario input digest, and output digest. Every scenario uses the normal `Engine`, governor, and session lifecycle. Search and snapshot continue through `PortableOperations`, the workspace backend, reduction, retention, and ASON encoding. Store spill/fetch uses capture, Rayon digest, atomic retention, range read, release, and teardown. Process scenarios use direct argv spawn, native process-group or Job Object ownership, concurrent pipe capture, cancellation, process-tree wait, retained references, and canonical response encoding. Stable evidence bytes are compared across warm-up, samples, and worker counts; a difference fails before timing is printed. Host timings are not checked in or gated because shared-runner performance is not portable.

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

The initial corpus contains small, medium, and large fixtures in these families:

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

Every task is versioned and contains:

```text
task id
platform constraints
initial workspace archive and digest
agent objective
allowed capabilities
time and resource limits
expected final-state verifier
native-shell baseline definition
ash program opportunities
output-retention policy
```

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

The implemented host-local slice exercises five real runtime paths over deterministic inputs:

- `search-literal` walks the workspace, reads files on bounded workers, scans text, performs stable merge and path interning, and encodes the final ASON response;
- `snapshot-blake3` walks the same workspace, hashes files in the Rayon pool, builds and retains the canonical manifest, and encodes the final ASON response;
- `result-store-spill-fetch` captures 8 MiB in 16 KiB chunks with a 4 MiB memory ceiling, proves disk residency, hashes and atomically retains the value through the compute pool, fetches its final 64 KiB range, releases the alias, and tears down the session spool;
- `exec-capture-pressure` runs a fixture process whose two native threads emit 8 MiB to stdout and stderr simultaneously, proves both complete streams crossed the 4 MiB disk boundary, verifies their deterministic final 64 KiB ranges, and measures admission through canonical response encoding;
- `exec-cancel-tree-empty` runs a parent that spawns a pipe-inheriting descendant, waits for the descendant PID marker, then measures from `Session::cancel` until the native process group or Job Object is empty, inherited pipes reach EOF, the request unregisters, and the canonical cancelled response is encoded.

Every sample uses a fresh session so aliases, dictionaries, budgets, cancellation registrations, and spools start from the same state. Fixture compilation, engine construction, and session creation are outside the timed interval. Search, snapshot, and process capture time program admission through canonical response encoding. Store timing includes capture through range fetch, release, and spool teardown. Cancellation timing begins only after the descendant exists and ends after tree wait plus canonical response encoding. The benchmark has no fixed speedup or cancellation threshold: it proves stable evidence and bounded completion, then reports current-host measurements.

The remaining runtime corpus expands this implemented slice to measurements that isolate `ash` overhead from the executed tool:

- cold process startup;
- warm framed request dispatch;
- direct child spawn overhead;
- stream capture at additional output rates and chunk distributions;
- reducer throughput;
- path dictionary lookup;
- graph scheduling at several node counts;
- directory traversal, literal search, regular-expression search, hashing, and reduction at 1, 2, 4, 8, and host-default compute workers;
- mixed-load I/O latency while the compute pool is saturated.

The current schema reports p50, p95, p99, sample count, host OS/architecture/available CPU count, selected compute workers, work volume, input and output digests, output size, throughput, speedup, parallel efficiency, and every raw observation. Future scenarios must add configured I/O workers, peak resident memory, and CPU utilization where those values are material to the claim. Results from different hosts are not combined into one scaling curve.

Parallel and sequential runs consume the same input digest and must emit byte-identical evidence. Search and snapshot outputs are canonical ASON; store and process-capture outputs are verified ranges; cancellation output binds the typed final status and termination kind after tree cleanup. A faster run with reordered, missing, duplicated, or changed evidence is a correctness failure rather than a performance result. Future fixtures still need fewer large files, skewed directory trees, binary files, ignored paths, sparse and dense matches, varied output rates, and mixed compute/process pressure.

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
|-- reports/v0.1.0/
|   |-- format.json
|   `-- README.md
|-- tasks/<task-id>/
|   |-- task.json
|   |-- workspace.tar.zst
|   |-- verify/
|   `-- baselines/
|-- tokenizers/<profile>/
`-- schemas/
```

Future task and runtime reports add `summary.json`, `runs.jsonl`, and `environment.json` beside their versioned report README. Published summaries link to raw versioned evidence. README marketing copy may quote a benchmark only after the corresponding report is available and reproducible.
