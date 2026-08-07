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

The optional human frontend has reached an interactive H1 checkpoint with a
feature-gated `ash shell` route. A terminal invocation opens a line-edited REPL
with a configurable prompt, private persistent history, opt-in startup Profile,
and `exit [STATUS]`. The same persistent state executes sequential `pwd`,
`echo`, `cd`, `export`, `unset`, `set` pipefail control, portable `ls`, `cat`,
and `grep`, plus native host executables with direct argument vectors. Inline
source, native script files, and bounded stdin remain available without changing
the machine contracts of `ash run` and `ash rpc`. H2 now provides explicit
process-stdio modes, validated native OS pipe graphs, same-line native pipelines
of two to 32 stages, configurable `pipefail`, and source-ordered native
redirections. Every stage is preflighted before spawn and connected through
direct OS handles. Redirections may replace internal pipeline endpoints: ash
closes the unused parent end so downstream readers receive EOF or upstream
writers receive the native broken-pipe behavior. Unredirected final stdout and
stderr share the remaining bounded capture allowance.

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
| Human shell          | `ash shell`                           | H1 REPL lifecycle and commands plus H2 native pipelines, configurable `pipefail`, ordered native redirections, and internal endpoint replacement                       |

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

## First human commands

Run `ash shell` on a terminal for the default `ash> ` prompt, or select a
non-interactive source explicitly:

```sh
ash shell
ash shell -c "grep -in 'semantic' crates/ash-ops/src/semantic.rs"
ash shell ./script.ash
printf 'echo from-stdin\n' | ash shell --no-profile
ash shell --profile ./profile.ash
```

Every script, stdin source, and Profile is limited to 1 MiB of valid UTF-8. Use
`ash shell -- ./-script.ash` when a file operand begins with `-`.
Profiles are opt-in through `--profile FILE` or non-empty `ASH_PROFILE`;
`--no-profile` provides deterministic recovery. A Profile is parsed completely
before any of it executes. A parse failure stops non-interactive startup, while
an interactive shell reports the source span and still opens with no partial
Profile effects. `exit [STATUS]` accepts 0 through 255, uses the previous status
when omitted, and stops the remaining submitted source.

`ASH_PROMPT` replaces the prompt and must be valid UTF-8. `ASH_HISTORY` selects
a history file relative to the initial cwd; an empty value disables persistent
history. Otherwise the default is `$XDG_STATE_HOME/ash/history`,
`$HOME/.local/state/ash/history`, or `%LOCALAPPDATA%\ash\history`, depending on
the host. Lines beginning with a space or tab are not recorded. History files
are rejected when they are symbolic links or non-regular targets, use mode
`0600` on Unix, and degrade to an in-memory session with a warning if persistence
is unsafe or unavailable.

Portable `ls` lists one path (default `.`), emits one stable native name per
line, and supports `-a`/`--all`, `-d`/`--directory`, `-1`, combined short
options, and `--`. Unsupported GNU options fail clearly.
Portable `cat` requires one file path, writes its bytes without conversion or
an added newline, accepts `--`, and shares the 128 MiB semantic read/capture
ceiling. Options, multiple files, and the stdin operand `-` remain explicit
errors until streaming stdio lands.
Portable `grep` requires one valid-UTF-8 regular file and uses Rust regular
expressions by default. It supports `-E`/`--extended-regexp`,
`-F`/`--fixed-strings`, `-i`/`--ignore-case`, `-n`/`--line-number`, combined
short options, and `--`. A search is limited to 64 MiB; no matches return status
1 without a diagnostic. Directories, multiple files, stdin `-`, and unsupported
options fail explicitly.

`export NAME=VALUE` and `unset NAME` update both shell-variable and exported
environment state for later commands. Each accepts one expanded assignment or
name plus `--`; names are ASCII shell identifiers, empty values are preserved,
and unsetting a missing name succeeds. Quote values that may contain field
separators, for example `export COPY="$SOURCE"`. Listing and multiple names
remain explicit non-features in this checkpoint.

`set -o pipefail` enables rightmost-failure pipeline status for the persistent
shell state, while `set +o pipefail` restores the default final-stage policy.
Other `set` forms remain explicit errors. A Profile can select the policy before
the main source runs; using `set` as a pipeline stage is rejected before spawn
and cannot mutate parent state.

`$NAME`, `${NAME}`, and `$?` expand immediately before each command resolves.
Single quotes and escaped dollars remain literal; double quotes preserve one
field, while unquoted values split on fixed ASCII space, tab, and LF separators.
Variables precede host-aware exported-environment lookup, undefined values are
empty, and native argument units are preserved through direct argv launch.

Native commands resolve through the shell state's `PATH` or an explicit
`native:` prefix, then launch the resolved executable directly with the parsed
argument vector, current directory, and exported environment. No `sh -c`,
`cmd /c`, or PowerShell command string is inserted. An unredirected standalone
child's stdin remains null and output remains synchronously captured, so native
programs that themselves require a foreground terminal are still deferred to
the H4 job-control work. Stdout and stderr share the remaining 128 MiB capture
allowance, and the native exit status is returned.

Native commands accept `<`, `>`, `>>`, `2>`, `2>>`, `2>&1`, and `1>&2`.
Redirections are applied from left to right, so `command >out 2>&1` merges both
streams into `out`, while `command 2>&1 >out` leaves stderr on the original
stdout capture. File targets expand to exactly one native field, resolve from
the persistent cwd, and connect directly to child OS handles without buffering
file output in shell memory. Superseded targets are still opened in source
order. Missing, ambiguous, or unopenable targets return status 1 with a
redirection diagnostic. Redirections on stateful, portable, or WSL commands are
rejected before side effects.

A same-line `|` forms a foreground pipeline of two to 32 native host commands.
Every stage is expanded, UTF-8 checked, resolved, and confirmed native before
any child starts. Intermediate stdout travels directly through OS pipes;
unredirected final stdout is returned, while unredirected stderr is captured
concurrently and appended in stage order. Pipeline status defaults to the final
stage. With
`set -o pipefail`, it becomes the rightmost unsuccessful stage, including
conventional `128 + signal` mapping; an all-success pipeline still uses its
final stage. Stateful and portable commands, aliases, functions, and WSL stages
fail explicitly before spawn because their streaming adapters do not exist yet.
Any native stage may redirect stdin, stdout, or stderr or duplicate descriptors
in source order. If a producer no longer writes an internal pipe, the downstream
reader receives EOF. If a consumer replaces its pipe stdin, the upstream writer
receives the platform's broken-pipe behavior; the usual final-stage or
`pipefail` policy selects the visible pipeline status. A descriptor duplicated
from the pipe remains connected even if the original descriptor is later
redirected.

User-visible terminal streaming, unified pipeline job supervision,
builtin/portable/WSL redirection and pipeline adapters, foreground interactive
programs and job control, broader expansion, mutations, and WSL execution are
not implemented yet. A minimal machine-only binary can be built with
`--no-default-features`.

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

- **288 Rust workspace tests** across protocol schemas, RPC, every operation,
  transactions, recovery, the retained store, cancellation, signed updates, and
  the human shell.
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
