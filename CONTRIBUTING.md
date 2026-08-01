# Contributing to ash

Thank you for improving `ash`.

The project is currently architecture-first. Contributions should preserve the
product position **AI Native Shell** and make Coding Agent task completion,
token efficiency, deterministic behavior, and cross-platform semantics more
concrete.

## Before opening a change

1. Search existing issues and pull requests.
2. Use the appropriate issue form for bugs, features, or architecture changes.
3. Open an architecture proposal before changing ASH/1, ASON, capability
   semantics, cross-platform behavior, or release trust boundaries.
4. Keep changes focused. Do not combine protocol, runtime, and visual cleanup
   unless they are inseparable.
5. Never include credentials, private repositories, customer data, or
   unredacted agent transcripts in fixtures.

## Design invariants

- `ash` is for Coding Agents, not terminal-oriented human interaction.
- ASON is specified and implemented by this project; do not describe it as a
  compatibility layer for another serialization format.
- Token claims require reproducible evidence under
  [`docs/benchmarks.md`](./docs/benchmarks.md).
- Linux, macOS, and Windows are one semantic contract, not independent ports.
- External programs receive an executable and argument vector by default; an
  implicit host shell is not allowed.
- Reduction and truncation must be explicit, bounded, and recoverable by
  reference unless retention was deliberately disabled.
- Security boundaries must distinguish enforced behavior from planned or
  best-effort behavior.

## Documentation changes

Documentation and repository files are written in English. Update every
affected source of truth in the same pull request:

- `README.md` for project-facing behavior;
- `docs/architecture.md` for component ownership and runtime boundaries;
- `docs/protocol.md` for ASH/1 and ASON;
- `docs/distribution.md` for installation and update behavior; and
- `docs/benchmarks.md` for measurement rules and claims.

Do not present planned behavior as implemented. Render changed README SVGs at
both desktop and mobile widths, verify local links, and keep essential commands
and explanations in Markdown rather than images.

## Code changes

The implementation workspace has not been scaffolded yet. Once Rust crates are
present, code changes are expected to run at least:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
```

Protocol parsers, path handling, process lifecycle, and installer changes also
require focused integration, malformed-input, and platform contract coverage.
Follow repository-local commands when they become stricter than this baseline.

## Pull requests

A pull request should describe:

- the problem and affected agent workflow;
- the chosen behavior and alternatives considered;
- ASH/1 or ASON compatibility impact;
- Linux, macOS, and Windows impact;
- safety and resource-boundary impact;
- expected token impact without unsupported numbers; and
- the validation that was actually run.

Small reviewable commits are preferred. Generated artifacts belong in commits
only when the repository defines them as source-controlled release evidence.

By contributing, you agree that your contribution is licensed under the
repository's [MIT License](./LICENSE) and that project participation follows
the [Code of Conduct](./CODE_OF_CONDUCT.md).
