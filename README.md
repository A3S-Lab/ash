<p align="center">
  <img src="./assets/readme/hero.svg" width="100%" alt="ash — AI Native Shell executing typed work across bounded I/O and CPU planes and returning compact ASON evidence">
</p>

<p align="center"><strong>AI Native Shell</strong> · typed execution · guarded mutation · compact, retrievable evidence</p>

<p align="center">
  <a href="https://a3s-lab.github.io/ash/">中文网站</a> ·
  <a href="https://a3s-lab.github.io/ash/en/">English docs</a> ·
  <a href="https://a3s-lab.github.io/ash/guide/capabilities.html">Capabilities</a> ·
  <a href="https://a3s-lab.github.io/ash/guide/coding-agents.html">Coding Agent Skill</a> ·
  <a href="https://a3s-lab.github.io/ash/guide/install.html">Install</a>
</p>

> [!IMPORTANT]
> `ash` is pre-release. The source implementation, cross-platform installers,
> and fail-closed six-target release workflow are available, but release
> credentials are not provisioned and no supported signed binary has been
> published. Build from source for development validation.

`ash` is a greenfield shell designed around Coding Agents rather than terminal
users. It accepts typed ASH/1 programs, executes independent work under explicit
budgets, and returns canonical ASON with references to complete retained
evidence. No hidden shell string, silent truncation, or completion-order output
becomes part of the contract.

## What ash covers

| Surface              | Operations                            | What is implemented                                                                                                                                                    |
| -------------------- | ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Repository discovery | `read r`, `list l`, `search g`        | Workspace-confined byte/line reads, stable traversal, literal and regular-expression search                                                                            |
| Processes            | `exec x`, `cancel k`                  | Direct executable + argv launch, environment/stdin control, deadlines, concurrent stdout/stderr capture, and owned process-tree cleanup                                |
| Guarded mutation     | `patch p`, `fs f`                     | BLAKE3 compare-and-swap edits plus journaled, no-overwrite file create/copy/move/remove with rollback and restart recovery                                             |
| Parallel programs    | `batch b`                             | Validated acyclic graphs, ready-node concurrency, failed-descendant skip, independent drain, and stable lowest-index errors                                            |
| Workspace state      | `snapshot s`                          | Deterministic scoped manifests and matching before/after deltas                                                                                                        |
| Retained evidence    | `/ # ? - \| >`                        | Byte and line slices, search, release, ordered table projection, and capability-gated materialization                                                                  |
| Model context        | ASON, `×N`, `×N#K`, `⋯N`              | Columnar records, path dictionaries, explicit reductions, stable merge, and references back to the full source                                                         |
| Trust and delivery   | capabilities, permits, signed updates | Least-privilege negotiation, session/action/policy/expiry-bound one-time permits, replay rejection, transactional activation, recovery, rollback, SBOM, and provenance |

The [complete capability map](https://a3s-lab.github.io/ash/guide/capabilities.html)
documents guarantees, evidence, and deliberate non-goals for the full surface.

## Coding Agent Skill

The repository includes a project-native Agent Skill:

```text
.agents/skills/use-ash/
├── SKILL.md
├── agents/openai.yaml
└── references/
    ├── operations.md
    └── workflows.md
```

It teaches an Agent to select an operation, emit the exact `t,i,o,a,u`
envelope, use digest-guarded mutation, build bounded DAGs, retrieve retained
evidence, and verify the result. Invoke it in a compatible Coding Agent with:

```text
Use $use-ash to inspect this repository, make the requested change, and verify it.
```

Read the [Coding Agent integration guide](https://a3s-lab.github.io/ash/guide/coding-agents.html)
or inspect the [Skill source](./.agents/skills/use-ash/SKILL.md).

## First typed request

This canonical request searches `src` for literal `TODO` with explicit token,
record, and wall-clock budgets:

```ason
t:1
i:17
o:g
a{q,p,f}:
TODO,[src],0
u{tok,rec,ms}:
256,64,30000
```

Run the checked fixture from a source crate whose workspace contains `src`:

```sh
cd crates/ash-cli
cargo run -p a3s-ash -- run < ../../spec/fixtures/ason/search-request.ason
```

An installed binary uses `ash run < request.ason`. Windows PowerShell must keep
the canonical UTF-8/LF bytes intact; use
`Start-Process ash -ArgumentList run -NoNewWindow -Wait -RedirectStandardInput request.ason`
instead of piping a decoded string.

For long-lived integration, `ash rpc` adds a framed handshake, concurrent
requests, cancellation, capabilities, permits, and retained-reference lifecycle.
Use `ash ason` to validate and canonicalize a request while authoring it.
References are session-local: a later `ash run` process cannot consume an alias,
snapshot baseline, batch-child response, cancellation target, or permit
challenge returned by an earlier process. Workflows that need those values must
keep one framed `ash rpc` session alive.

## One runtime, two execution planes

<p align="center">
  <img src="./assets/readme/architecture.svg" width="100%" alt="ash architecture from Coding Agent through typed program and hierarchical governor into Tokio I/O and Rayon CPU planes, stable merge, and canonical ASON">
</p>

Tokio owns RPC, child processes, pipes, deadlines, and cancellation. A fixed
Rayon work-stealing pool owns search, hashing, diffs, reduction, store commits,
and other splittable CPU work. One hierarchical governor bounds host, session,
request, and operation concurrency. Stable merge erases worker completion order
before canonical ASON is emitted.

Large process streams remain lossless without flooding the model context. A
fixed head/tail projection is returned immediately; evidence beyond the 4 MiB
session memory ceiling spills to private immutable files. Bounded range fetch,
aliases, deduplication, leases, release, and proven crash-orphan cleanup complete
the store lifecycle.

## Evidence, not aspiration

The current `main` baseline includes:

- **200 Rust workspace tests** across protocol schemas, RPC, every operation,
  transactions, recovery, the retained store, cancellation, and signed updates.
- **22 schema-14 runtime scenarios** across worker matrices, including an 8 MiB
  retained capture crossing the 4 MiB memory ceiling and fetching only its final
  64 KiB.
- **7 locked Coding Agent tasks** comparing native-shell and ash traces under
  one task, result, and transcript schema.
- **1,024 exhaustive four-node DAG/success-mask cases**, with forced completion
  order changes and stable error selection.
- **30 forward transaction cutpoints plus 12 recovery cutpoints**, including
  hard-link identity crash windows.
- Source-bound format/token reports, twice-weekly fuzzing, AddressSanitizer
  artifacts, six-target installer smoke tests, and third-party license gates.

The checked format corpus reports canonical ASON at **62% of compact row-object
JSON tokens** for both pinned tokenizers. Explicit reductions and reference
formulas are measured separately; the full source remains retrievable.

Reproduce the gates:

```sh
cargo test --workspace --all-targets
cargo run -p a3s-ash-bench --release --locked -- \
  --check benches/reports/v0.1.0/format.json
cargo run -p a3s-ash-bench --release --locked -- \
  --check-task-lock benches/tasks/v1/lock.json
cargo run -p a3s-ash-bench --release --locked -- --tasks
cargo build -p a3s-ash --release --locked
cargo run -p a3s-ash-bench --release --locked -- --runtime
npm --prefix website ci
npm --prefix website run check
```

Read the [benchmark contract](https://a3s-lab.github.io/ash/guide/benchmarks.html)
before interpreting a number.

## Installation

> [!WARNING]
> The release installers intentionally fail closed until a supported signed
> binary exists. The Cargo command below builds current source; it is not a
> signed release.

Linux and macOS, x86-64 and ARM64:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh
```

Windows PowerShell, x86-64 and ARM64:

```powershell
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
irm https://raw.githubusercontent.com/A3S-Lab/ash/main/install.ps1 | iex
```

Build current source with Cargo:

```sh
cargo install --git https://github.com/A3S-Lab/ash --locked a3s-ash
```

See [installation](https://a3s-lab.github.io/ash/guide/install.html) for pinned
versions, custom prefixes, offline archives, verification, transactional
activation, upgrade, rollback, recovery, and uninstall.

## Deliberate boundaries

ASH/1 is not a human REPL, POSIX compatibility layer, embedded model, remote
executor, or universal child-process sandbox. It does not provide interactive
terminal semantics, shell-language evaluation, overwrite, recursive directory
mutation, or runtime value piping between batch nodes. Child programs inherit
the operating-system authority of the ash process.

These are contract boundaries, not undocumented gaps. See
[security](https://a3s-lab.github.io/ash/guide/security.html) for the precise
capability, permit, path, transaction, and signed-update model.

## Documentation map

- [Get started](https://a3s-lab.github.io/ash/guide/) — reader paths and first request
- [Complete capabilities](https://a3s-lab.github.io/ash/guide/capabilities.html) — every operation, guarantee, and non-goal
- [Coding Agent integration](https://a3s-lab.github.io/ash/guide/coding-agents.html) — Skill and harness workflow
- [Architecture](./docs/architecture.md) — semantic IR, scheduler, governor, and platform boundary
- [Portable human-shell architecture](./docs/portable-human-shell.md) and [separation decision](./docs/decisions/0002-separate-portable-human-shell-layer.md)
- [ASH/1 and ASON](./docs/protocol.md) — authoritative wire and data contract
- [Distribution](./docs/distribution.md) and [release operations](./docs/releasing.md)
- [Benchmark methodology](./docs/benchmarks.md)

Contributions follow [CONTRIBUTING.md](./CONTRIBUTING.md), the project
[Code of Conduct](./CODE_OF_CONDUCT.md), and [SECURITY.md](./SECURITY.md).
The Rust workspace is MIT licensed.
