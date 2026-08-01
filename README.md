<p align="center">
  <img src="./assets/readme/hero.svg" width="100%" alt="ash — AI Native Shell executing typed work across bounded I/O and CPU planes and returning compact ASON evidence">
</p>

<p align="center"><strong>AI Native Shell</strong> · typed parallel execution · compact model context</p>

> [!IMPORTANT]
> `ash` is pre-release. Source builds now execute typed `exec`, `read`, `list`, `search`, compare-and-swap `patch`, durable file-only `fs` transactions, workspace `snapshot/delta`, and bounded dependency-graph `batch` requests; they also negotiate least-privilege capabilities, enforce session/action-bound one-time approval permits, inspect retained results, cancel active work, and verify, activate, recover, or roll back signed releases. Cross-platform installers and a fail-closed six-target release workflow are implemented; release credentials are not provisioned and no supported signed binary release is published yet.

`ash` is a greenfield shell designed around coding agents rather than terminal users. It turns shell work into typed programs, executes independent work across bounded I/O and CPU planes, and returns only the evidence worth placing in an LLM context.

## Proof starts at the result

The default LLM-facing representation is **ASON**, the native structured format designed and implemented by `ash` for LLM exchange. Homogeneous records are emitted in columns, paths are interned into compact dictionaries, and large or nested values become references instead of repeated text.

Conceptual search result:

```ason
t:3
i:17
s:0
p[1]{i,v}:
1,src/lib.rs
d[2]{p,l,c,t}:
1,42,7,"TODO item"
1,87,3,"FIXME item"
z:0
r:~
```

The schema for these short fields is negotiated once. Full output remains available through a bounded session reference; truncation is explicit instead of silently destructive.

## Why ash is different

- **Agent-first semantics.** Programs are typed graphs, not human-oriented command strings.
- **Token cost is a runtime concern.** Filtering, projection, deduplication, path interning, and output budgets happen before serialization.
- **Multi-core by design.** Independent graph nodes and repository partitions use a work-stealing compute pool while process and RPC I/O stay responsive.
- **Portable by construction.** Linux, macOS, and Windows share one semantic contract; platform behavior is isolated behind native backends.
- **Deterministic at the boundary.** Parallel workers may finish in any order; stable merge produces byte-identical canonical ASON.
- **Loss is visible.** Every omitted byte is recoverable by reference, and every reduced result declares its status.

## One runtime, two execution planes

<p align="center">
  <img src="./assets/readme/architecture.svg" width="100%" alt="ash architecture from coding agent through typed program and hierarchical governor into separate Tokio I/O and Rayon CPU planes, native backends, stable merge, and canonical ASON">
</p>

A persistent stdio session is the primary integration. One-shot calls use the same engine. Tokio owns RPC, child processes, pipes, deadlines, and cancellation; a fixed Rayon work-stealing pool owns search, hashing, diffing, reduction, and other splittable CPU work. A shared governor prevents a wide graph from multiplying inner parallelism beyond host and request budgets.

The externally visible path remains deterministic:

1. **Semantic IR** — typed programs, nodes, edges, values, budgets, and capabilities.
2. **Parallel runtime** — dependency-aware nodes plus bounded operation partitions.
3. **Stable merge** — logical path and position keys erase worker completion order.
4. **LLM presentation** — deterministic reduction followed by canonical ASON.

Read the complete contracts:

- [System architecture](./docs/architecture.md)
- [ASH/1 protocol and ASON specification](./docs/protocol.md)
- [Cross-platform distribution and one-click installation](./docs/distribution.md)
- [Release operator contract](./docs/releasing.md)
- [Token-efficiency benchmark contract](./docs/benchmarks.md)
- [Rust and dual-plane runtime decision](./docs/decisions/0001-rust-and-dual-plane-runtime.md)

## Verify the current baseline

```sh
git clone https://github.com/A3S-Lab/ash.git
cd ash
cargo test --workspace --all-targets
cargo run -p a3s-ash-bench --release --locked -- \
  --check benches/reports/v0.1.0/format.json
```

Run one canonical request from the repository root:

```sh
cargo run -p a3s-ash -- run < spec/fixtures/ason/search-request.ason
```

The pinned Rust workspace currently verifies typed ASH/1 schemas, canonical framed handshakes, negotiated capability masks, action/session/policy/expiry-bound approval permits, replay rejection, concurrent `exec/read/list/search/patch/fs/snapshot`, acyclic batch graphs, dependency-failure skipping, stable child-response references, retained-result slicing/search/release, chained workspace deltas, preemptive cancellation, atomic budgets, hierarchical permits, workspace-confined paths, direct argv launch with timeout-safe process-tree cleanup, deterministic multi-core preparation, and multi-file rollback. File-only create, copy, move, and remove transactions add digest guards, no-overwrite semantics, a checksummed on-disk journal, cross-process serialization, reverse rollback, and restart recovery. Signed-release tests cover strict Ed25519 verification, sequence rollback/equivocation, the complete six-target manifest, exact archive shape, extraction ceilings, embedded binary identity, transactional activation, health-gated recovery, and reversible rollback. `ash self status|check|update|rollback|recover` uses canonical ASON, and network update input is HTTPS-only and byte-bounded. `ash rpc` executes independent requests concurrently while emitting final frames in stable input order. Offline installer smoke tests cover installation, integrity rejection, idempotent and forced reinstall, lock ownership, PATH ownership, rollback, and uninstall. This is still a development checkpoint, not a supported installation path.

## Release contract

The first usable release must ship as one native `ash` binary and include:

- persistent stdio RPC and one-shot invocation;
- direct process execution with cancellation, timeouts, and process-tree cleanup;
- bounded read, list, search, patch, filesystem mutation, snapshot, and result-reference operations;
- deterministic multi-core batch and dependency-graph execution with compact, retrievable node evidence;
- Linux and macOS `install.sh`, Windows `install.ps1`, verified release artifacts, self-update, and rollback;
- native builds for x86-64 and ARM64 on Linux, macOS, and Windows.

Installation commands will be published only when signed binaries and clean-host end-to-end release tests exist. The implemented entrypoints and remaining trust boundary are documented in the [distribution design](./docs/distribution.md).

## Repository ownership

`A3S-Lab/ash` is an independently buildable Rust workspace and release unit. The A3S umbrella repository consumes the same history as its `crates/ash` Git submodule, matching the existing A3S component model without turning the a3s root package into a Cargo workspace.

## Deliberate boundaries

`ash` is not a Bash, Zsh, PowerShell, or CMD compatibility layer. Version one does not include a human REPL, prompt themes, completion, aliases, interactive job control, an embedded model, remote execution, or a universal security sandbox.

It is a local execution boundary for coding agents. Workspace capabilities, resource limits, atomic file mutation, explicit approval permits, and structured errors are in scope; pretending that arbitrary child-process network and syscall isolation is portable is not.

## Delivery order

1. Stabilize the implemented vertical slice and its ASH/1 fixtures across Linux, macOS, and Windows.
2. Integrate the implemented capability-scoped permit API with trusted harness policy providers and freeze compatibility fixtures.
3. Harden parser, recovery, and scheduling paths with fuzzing and fault injection.
4. Provision protected release credentials and execute the implemented six-target signing, notarization, attestation, clean-host upgrade/rollback, and installer gates.
5. Gate release promotion on correctness, token cost, latency, cancellation, installer, upgrade, and benchmark evidence, then pin the proven commit in the A3S submodule.

The first checked-in evidence is intentionally format-only: on its deterministic corpus, canonical ASON uses 62% of compact row-object JSON tokens under both pinned tokenizer profiles, while the closer columnar JSON baseline is reported alongside it. This is a regression datum, not an agent-task or runtime-performance claim. The report, reproduction command, remaining acceptance criteria, and accounting rules are defined in [docs/benchmarks.md](./docs/benchmarks.md).

## Contributing and support

Read [CONTRIBUTING.md](./CONTRIBUTING.md) before proposing behavior or protocol changes. Focused design questions, feature requests, architecture proposals, and reproducible bugs each have a structured issue form. General expectations are documented in [SUPPORT.md](./SUPPORT.md) and the [Code of Conduct](./CODE_OF_CONDUCT.md).

Do not report suspected vulnerabilities in a public issue. Follow the private reporting instructions in [SECURITY.md](./SECURITY.md).

## License

`ash` is available under the [MIT License](./LICENSE).
