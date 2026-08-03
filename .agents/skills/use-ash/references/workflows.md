# Coding workflows with ash

Select the smallest workflow that proves the requested outcome. Combine operations only when each step contributes necessary evidence.

Any workflow that follows a retained alias, compares a snapshot baseline,
inspects a batch child, cancels active work, or retries with a permit requires
one persistent RPC session. Separate `ash run` processes do not share those
session-local values.

## Explore an unfamiliar repository

1. Use `l` with a shallow depth to identify manifests and source roots.
2. Use `g` for exact symbols, diagnostics, or markers.
3. Use `r` for the smallest useful line ranges around matches.
4. In the same live RPC session, follow a retained reference with `#`, `?`, or `|` instead of repeating a broad search.
5. Report paths and relevant ranges, not the full tree.

## Make a guarded code edit

1. Read the target and obtain its current digest from typed evidence.
2. Build sorted, non-overlapping edits against that exact preimage.
3. Submit `p` with the current BLAKE3 digest.
4. If ash returns a conflict, reread and recompute; never weaken the guard.
5. Run focused verification through `x`.
6. Read the changed range or compare workspace snapshots before declaring success.

Use `f` instead when the task creates, copies, moves, or removes regular files. Keep related lifecycle actions in one transaction so a later failure reverses earlier actions.

## Diagnose a noisy test failure

1. Execute the test binary directly with `x` and a bounded immediate output budget.
2. Inspect the failure-focused projection first. `×N`, `×N#K`, and `⋯N` are explicit reductions, not source bytes.
3. In the same live RPC session, use `?` to search the complete retained stream for another diagnostic.
4. Use `#` or `/` in that session to retrieve only the necessary context.
5. Release unused aliases with `-` when the session remains active.

## Run independent work concurrently

1. Put independent searches, reads, or process calls in ready `b` nodes with `d:[]`.
2. Add dependencies only for control flow; runtime value piping is not part of ASH/1.0.
3. Keep nested argument documents canonical and within the parent budget.
4. Read retained child evidence in the same live RPC session only for nodes that affect the answer.
5. Expect results and task errors in stable input order, not completion order.

## Prove a workspace change

1. In a persistent RPC session, capture `s` mode `0` before a multi-file task and retain its manifest reference.
2. Perform guarded patches or file transactions.
3. Request `s` mode `1` with the same roots, depth, and flags plus the baseline reference.
4. Verify the typed delta and the project-specific tests.
5. Keep the baseline reference only while another delta is needed.

## Decide when not to use ash

Use native tools when ash is absent and installation is not authorized, or when the task requires an unsupported semantic such as recursive directory mutation, overwrite, an interactive terminal, shell language evaluation, remote execution, or a portable network/syscall sandbox. State the boundary instead of approximating it silently.
