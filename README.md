<p align="center">
  <img src="./assets/readme/hero.svg" width="100%" alt="ash — AI Native Shell with a compact ASON result specimen">
</p>

<p align="center"><strong>AI Native Shell</strong></p>

> [!IMPORTANT]
> `ash` is currently an architecture-stage project. The protocol, runtime boundaries, cross-platform contract, and release plan are specified; no executable release is published yet.

`ash` is a greenfield shell designed around coding agents rather than terminal users. It expresses shell work as typed operations, executes it through a deterministic local runtime, and reduces results before they enter an LLM context.

## The interface starts with the result

The default LLM-facing representation is **ASON**, the native structured format designed and implemented by `ash` for LLM exchange. Homogeneous records are emitted in columns, paths are interned once per session, and large values become references instead of repeated text.

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

The schema for these short fields is negotiated once. Full output remains available through a bounded session reference; truncation is always explicit.

## Design contract

- **Agent-first semantics.** Programs are typed graphs, not human-oriented command strings.
- **Token cost is a runtime concern.** Filtering, projection, deduplication, path interning, and output budgets happen before serialization.
- **Portable by construction.** Linux, macOS, and Windows share one semantic contract; platform behavior is isolated behind native backends.
- **Deterministic execution.** `argv` is passed directly to processes. Shell expansion, quoting, and implicit host-shell behavior are not part of the default path.
- **Loss is visible.** Every omitted byte is recoverable by reference, and every reduced result declares its status.

## Architecture

<p align="center">
  <img src="./assets/readme/architecture.svg" width="100%" alt="ash architecture from coding agent through protocol, typed program scheduler, operation engine, platform backends, reducers, and ASON results">
</p>

A persistent stdio session is the primary integration. One-shot CLI calls remain available for harnesses that can only launch commands. Both routes compile into the same typed program IR, capability checks, scheduler, operation engine, and output pipeline.

The architecture deliberately separates three representations:

1. **Semantic IR** — typed programs, nodes, edges, values, budgets, and capabilities.
2. **Transport framing** — length-prefixed ASON over stdio.
3. **LLM presentation** — the same canonical ASON after deterministic reduction.

Read the complete design:

- [System architecture](./docs/architecture.md)
- [ASH/1 protocol and ASON specification](./docs/protocol.md)
- [Cross-platform distribution and one-click installation](./docs/distribution.md)
- [Token-efficiency benchmark contract](./docs/benchmarks.md)

## First release contract

The first usable release must ship as one native `ash` binary and include:

- persistent stdio RPC and one-shot invocation;
- direct process execution with cancellation, timeouts, and process-tree cleanup;
- bounded read, list, search, patch, filesystem mutation, snapshot, and result-reference operations;
- batch and dependency-graph execution;
- Linux and macOS `install.sh`, Windows `install.ps1`, verified release artifacts, self-update, and rollback;
- native builds for x86-64 and ARM64 on Linux, macOS, and Windows.

Installation commands will be published only when signed binaries and end-to-end installer tests exist. The required entrypoints are already fixed in the [distribution design](./docs/distribution.md).

## Boundaries

`ash` is not a Bash, Zsh, PowerShell, or CMD compatibility layer. Version one does not include a human REPL, prompt themes, completion, aliases, interactive job control, an embedded model, remote execution, or a universal security sandbox.

It is a local execution boundary for coding agents. Workspace capabilities, resource limits, atomic file mutation, explicit approval permits, and structured errors are in scope; pretending that arbitrary child-process network and syscall isolation is portable is not.

## Delivery order

1. Freeze ASH/1 fixtures and the cross-platform benchmark corpus.
2. Ship the smallest vertical slice: session, `exec`, `read`, `list`, `search`, reducer, and result store.
3. Release and test all six platform artifacts plus both one-click installers.
4. Add graph execution, patch transactions, workspace deltas, and approval permits.
5. Gate releases on correctness, token cost, latency, cancellation, installer, and upgrade evidence.

No benchmark number is claimed before a reproducible corpus has been run. The acceptance criteria and accounting rules are defined in [docs/benchmarks.md](./docs/benchmarks.md).

## Contributing and support

Read [CONTRIBUTING.md](./CONTRIBUTING.md) before proposing behavior or protocol changes. Focused design questions, feature requests, architecture proposals, and reproducible bugs each have a structured issue form. General expectations are documented in [SUPPORT.md](./SUPPORT.md) and the [Code of Conduct](./CODE_OF_CONDUCT.md).

Do not report suspected vulnerabilities in a public issue. Follow the private reporting instructions in [SECURITY.md](./SECURITY.md).

## License

`ash` is available under the [MIT License](./LICENSE).
