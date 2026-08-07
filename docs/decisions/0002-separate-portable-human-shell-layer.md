# ADR 0002: Separate the portable human shell from ASH/1

- Status: accepted
- Date: 2026-08-03
- Accepted: 2026-08-07

## Context

`ash` currently provides a typed, agent-first execution boundary. Its process
operation launches an executable with an explicit argument vector, captures
bounded machine evidence, and deliberately excludes a human REPL, historical
shell syntax, interactive job control, and PTY emulation.

A human-facing shell should offer Linux-inspired commands that operate natively
on Windows, Linux, and macOS, while optionally executing real Linux binaries
through WSL on Windows. Adding free-form command strings directly to ASH/1
would mix parsing and presentation with the machine protocol, make dialect and
quoting ambiguous, and weaken the existing deterministic contract.

The existing batch DAG is also not a shell-pipeline implementation. A batch
dependency starts after its predecessor completes; a shell pipe requires both
processes to run concurrently over an operating-system stream with
backpressure.

## Decision

The human shell:

1. It will live in a separate `a3s-ash-shell` crate and be exposed as
   `ash shell`.
2. It will parse a documented `ash` dialect into a shell-specific typed plan.
3. It will call shared semantic services directly rather than round-tripping
   through ASON.
4. It will preserve ASH/1, `ash run`, and `ash rpc` behavior unchanged.
5. Portable commands will use native platform services. On Windows they will
   operate on Windows files without WSL.
6. Native external programs will continue to use direct argument-vector
   launch without an implicit host shell.
7. Actual Linux commands on Windows will use an explicit WSL backend. Native
   lookup failure will never trigger WSL silently.
8. Streaming pipelines and ordered redirections will use a dedicated process
   plan and OS pipes, not batch completion dependencies or retained-result
   materialization.
9. Foreground interactive programs will first use inherited terminal handles.
   Managed Unix PTY and Windows ConPTY support will be added only for use cases
   that require shell-owned terminal sessions.
10. Human interactive mode will inherit the user's OS authority. Optional
    workspace confinement will not be represented as a sandbox for arbitrary
    child processes.

The detailed accepted contract is defined in
[`../portable-human-shell.md`](../portable-human-shell.md).

The current source checkpoint has completed H0 and implemented H1. The
independent `a3s-ash-shell` crate now owns source-spanned parsing, persistent
state, deterministic command resolution, a cross-platform line editor, private
file history, and a sequential executor for `pwd`, `echo`, `cd`, expanded
`export`/`unset` environment updates, `exit`, portable `ls`, bounded raw-byte
`cat`, bounded text `grep`, named and last-status parameter expansion, and
native host executables. Parameter nodes retain exact quote and source-span
metadata; a separate native-string expansion stage performs the documented
fixed field splitting before resolution. Shared provider-neutral semantic
services live below the ASH/1 adapters in `a3s-ash-ops`; the portable commands
reuse their bounded list/read/search semantics through an ordinary-authority
native provider. Native commands reuse the `ash-platform` process-tree boundary
and launch an already-resolved native executable with the exact argument vector,
persistent cwd/environment, closed stdin, bounded captured stdout/stderr, and
propagated exit status; no host shell is inserted. The feature-gated `ash shell`
route opens a terminal REPL or accepts `-c SOURCE`, bounded stdin, or a bounded
native script path. Startup Profiles are explicit through `--profile FILE` or
`ASH_PROFILE`, with `--no-profile` as the deterministic recovery path. These
human features do not change the machine diagnostics of `ash run` or `ash rpc`.
H2 has begun below the language surface with explicit per-stream `Null`, `Piped`,
and `Inherit` modes in the shared platform process specification. ASH/1 and the
H1 shell map their existing stdin/capture behavior explicitly, while an 8 MiB
copy regression locks pipe backpressure and handle closure. User-visible
streaming stdio and pipelines, broader expansion, foreground interactive
programs and jobs, mutations, and WSL execution remain staged work.

## Rejected alternatives

### Add `shell: String` to ASH/1

Rejected because it makes semantics depend on an implicit dialect, host shell,
quoting implementation, and installed environment. Callers can already request
an explicit shell executable when non-portable behavior is intentional.

### Lower `producer | consumer` to a batch dependency

Rejected because completion dependencies serialize the commands and cannot
provide streaming, backpressure, broken-pipe behavior, or shared job control.

### Route every Windows command through WSL

Rejected because portable commands can operate on Windows directly, native
Windows programs must remain first-class, and WSL availability is not a valid
cross-platform assumption.

### Claim Bash compatibility

Rejected because matching familiar command names is not equivalent to matching
Bash grammar, expansion, scripting, process, and platform behavior. The `ash`
dialect will publish only the compatibility it tests.

## Consequences

- The machine and human interfaces can evolve without destabilizing each
  other's representation and latency contracts.
- Existing engine, operation, filesystem, cancellation, and platform work is
  reusable, but semantic services need an adapter boundary below
  `FinalResponse` construction.
- The platform process API needs explicit standard-I/O and terminal modes while
  keeping the current machine `ProcessSpec` stable.
- The project gains a second execution plan for streaming jobs; this is
  intentional because batch scheduling and pipelines have different lifecycle
  semantics.
- WSL becomes an optional Windows integration rather than a hidden dependency.
- Full Bash compatibility remains outside the product contract.
