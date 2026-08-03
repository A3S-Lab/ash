---
name: use-ash
description: Execute coding-repository work through ash, the AI Native Shell, using typed ASON requests, bounded parallelism, guarded workspace mutations, snapshots, and retrievable evidence. Use when ash is available and a coding task needs repository search or reading, direct process execution, tests, compare-and-swap patches, file transactions, dependency-graph batches, cancellation, or compact result retrieval.
---

# Use ash

Use `ash` as a typed execution surface for coding work. Prefer it when structured budgets, deterministic output, safe mutation, parallel operations, or retained evidence improve the task. Do not route work through `ash` merely to replace a short, already-safe native command.

## Preflight

1. Identify the intended workspace root. File operations are confined to the directory where `ash` starts.
2. Run `ash --build-info`. If `ash` is unavailable, do not install it or change the environment unless the user authorized that action; use the available native tools and report the fallback.
3. Use `ash run` only for a self-contained request whose immediate response is
   sufficient. Its one-shot session ends when the process exits, so a later
   invocation cannot consume its aliases, snapshot baseline, batch-child
   references, cancellation target, or permit challenge. Use `ash rpc` only
   through a harness that implements the framed handshake and persistent-session
   lifecycle, and use that same live session whenever a workflow needs those
   session-local values.
4. Build exact canonical ASON. Validate uncertain documents with `ash ason` before execution.

## Select the operation

| Need                                                                                   | Operation        |
| -------------------------------------------------------------------------------------- | ---------------- |
| Launch an executable with argv, environment, stdin, timeout, and complete pipe cleanup | `x` (`exec`)     |
| Read explicit byte or line ranges                                                      | `r` (`read`)     |
| Walk stable workspace paths and metadata                                               | `l` (`list`)     |
| Search text with bounded literal or regular-expression matches                         | `g` (`search`)   |
| Apply guarded byte edits to existing files                                             | `p` (`patch`)    |
| Create, copy, move, or remove regular files transactionally                            | `f` (`fs`)       |
| Run an acyclic graph with ready-node concurrency                                       | `b` (`batch`)    |
| Capture or compare workspace state                                                     | `s` (`snapshot`) |
| Slice, search, release, project, or materialize retained evidence                      | `/ # ? - \| >`   |
| Cancel queued or active work and descendants                                           | `k` (`cancel`)   |

Read [references/operations.md](references/operations.md) before composing an unfamiliar operation or reference formula. Never guess a column name, field order, flag bit, or nullable value.

## Compose a coding workflow

1. Discover narrowly with `g` or `l`; do not request the whole repository when a path or query can bound the result.
2. Read only the ranges required to understand the change. In a live RPC session,
   follow a retained reference when the immediate projection is insufficient.
3. Use `p` for edits to existing files and `f` for file lifecycle changes. Supply current BLAKE3 preimages and treat conflicts as a signal to reread, not to bypass the guard.
4. Use `b` only for independent work or explicit control dependencies. Keep observable output stable even when nodes execute concurrently.
5. Run the smallest relevant verification first, then broader project gates when the task warrants them.
6. In a live RPC session, use `s` before and after a multi-file task when an
   exact workspace delta is useful evidence.
7. Return the verified outcome and compact evidence. Preserve or release references deliberately.

Read [references/workflows.md](references/workflows.md) for exploration, mutation, diagnostics, parallel work, and snapshot patterns.

## Enforce request discipline

- Emit the exact envelope order `t,i,o,a,u`, adding `v` only for an externally approved permit retry.
- Use a positive numeric request ID and the smallest realistic token, record, and wall-clock budgets.
- Launch programs as executable plus argv. Never smuggle a Bash, PowerShell, or CMD command string into `exec` unless the user explicitly needs that shell as the executable.
- Keep paths canonical and workspace-relative. Do not attempt lexical escape, symlink traversal, directory mutation, overwrite, or recursive removal; ASH/1 does not authorize those shortcuts.
- Treat omitted output as recoverable only when the response supplies a retained
  reference and the issuing session remains live. A one-shot reference proves
  that its projection is incomplete, but a later `ash run` process cannot follow
  it. Do not infer missing bytes from a projection.
- Do not claim that ash sandboxes arbitrary child-process network access or system calls. Host-process execution inherits the operating-system authority of the ash process.

## Run and verify

On Unix-like shells:

```bash
ash run < request.ason
```

On PowerShell, preserve the request bytes instead of piping a decoded string:

```powershell
$ash = Start-Process ash -ArgumentList run -NoNewWindow -Wait -PassThru `
  -RedirectStandardInput request.ason
if ($ash.ExitCode) { throw "ash exited with code $($ash.ExitCode)" }
```

When a Coding Agent process tool accepts stdin directly, prefer executable
`ash`, argv `run`, and the exact UTF-8 request bytes. Check the typed status and
operation-specific result rather than matching prose. After a mutation, verify
the resulting files or a same-session snapshot delta; after `exec`, check
termination plus the projection, and follow a retained range only in the live
RPC session that issued it.
