# ASH/1 operation reference

Use this reference when composing typed requests. The full protocol remains authoritative; these examples cover the stable pre-release surface needed by Coding Agents.

## Session lifecycle

`ash run` creates one temporary session, executes one bare request, writes one
response, and exits. Use it only when the immediate response is sufficient.
References are session-local: retained-result formulas, snapshot deltas,
inspection of batch-child responses, cancellation of active work, and permit
retries must use values produced in the same live `ash rpc` session. A harness
using RPC must implement the canonical handshake, framing, and shutdown
lifecycle; bare ASON documents are not RPC frames.

## Canonical envelope

Every request uses this exact field order:

```ason
t:1
i:17
o:g
a{q,p,f}:
TODO,[src],0
u{tok,rec,ms}:
256,64,30000
```

- `t` is the request message type and is `1`.
- `i` is a positive request identifier.
- `o` is one canonical operation identifier.
- `a` is the operation-specific argument value.
- `u` limits immediate tokens, records, and wall-clock milliseconds.
- `v` may follow `u` only for a permit-bound semantic retry issued by a trusted harness.

## Operation matrix

| ID             | Argument shape       | Meaning                                                     |
| -------------- | -------------------- | ----------------------------------------------------------- |
| `x`            | `a{x,v,c,e,in,f}:`   | executable, argv, cwd, environment delta, stdin, flags      |
| `r`            | `a{p,m,o,n}:`        | paths, range mode, offset, length                           |
| `l`            | `a{p,d,f}:`          | roots, maximum depth, flags                                 |
| `g`            | `a{q,p,f}:`          | query, roots, flags                                         |
| `p`            | `a{p,h,i,o,n,v,f}:`  | paths, preimage digests, aligned edits, flags               |
| `f`            | `a[N]{i,k,p,q,h,v}:` | ordered file actions                                        |
| `b`            | `a[N]{i,d,o,a}:`     | graph nodes, dependencies, leaf operation, nested arguments |
| `s`            | `a{p,d,m,r,f}:`      | roots, depth, capture/delta mode, baseline, flags           |
| `/ # ? - \| >` | `a:[...]`            | retained-result formula operands                            |
| `k`            | `a{i}:`              | target request identifier                                   |

## Search, read, and list

Search `src` for literal `TODO`:

```ason
t:1
i:17
o:g
a{q,p,f}:
TODO,[src],0
u{tok,rec,ms}:
256,64,30000
```

Read lines 1 through 80 from one file (`m:1` is one-based line mode):

```ason
t:1
i:19
o:r
a{p,m,o,n}:
[src/lib.rs],1,1,80
u{tok,rec,ms}:
512,64,30000
```

List files and directories through depth 2:

```ason
t:1
i:20
o:l
a{p,d,f}:
[.],2,0
u{tok,rec,ms}:
256,128,30000
```

List flags are bit 0 include hidden, bit 1 files only, and bit 2 directories only. Search flags are bit 0 regular expression, bit 1 case-insensitive, and bit 2 include hidden.

## Direct process execution

Run Cargo without an implicit host shell:

```ason
t:1
i:18
o:x
a{x,v,c,e,in,f}:
cargo,[test,--locked],.,["RUST_BACKTRACE=1",-SECRET],~,0
u{tok,rec,ms}:
512,64,120000
```

Environment entries use `NAME=value` to set and `-NAME` to remove. `in` accepts `~`, inline text, or an immutable retained reference. Exec flag bit 0 clears the inherited environment before applying the delta.

## Guarded mutation

Patch paths and their lowercase 64-digit BLAKE3 preimages must be sorted. Edit vectors align by index: file index, byte offset, removed byte length, and replacement.

```ason
t:1
i:23
o:p
a{p,h,i,o,n,v,f}:
[src/lib.rs],[aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa],[0],[4],[3],[pub],0
u{tok,rec,ms}:
512,64,30000
```

Replace the example digest with the current preimage. On conflict, reread and re-evaluate the edit.

Filesystem action kinds are `0` create, `1` copy, `2` move, and `3` remove:

```ason
t:1
i:84
o:f
a[2]{i,k,p,q,h,v}:
1,0,new.txt,~,~,"hello\n"
2,1,Cargo.toml,Cargo.copy.toml,aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,~
u{tok,rec,ms}:
64,8,30000
```

Actions are file-only, never overwrite, and commit or roll back as one journaled transaction.

## Batch graph

Nested `a` cells contain one canonical argument document. Dependencies are sorted node IDs:

```ason
t:1
i:80
o:b
a[2]{i,d,o,a}:
1,[],g,"a{q,p,f}:\nTODO,[src],0\n"
2,[1],r,"a{p,m,o,n}:\n[Cargo.toml],0,0,32\n"
u{tok,rec,ms}:
64,16,30000
```

Use an empty dependency vector for independent ready nodes. Failed descendants are skipped; already-running independent work drains before a stable task error is selected.

## Snapshot and delta

Capture a workspace snapshot:

```ason
t:1
i:24
o:s
a{p,d,m,r,f}:
[.],64,0,~,0
u{tok,rec,ms}:
512,64,30000
```

For a delta, use mode `1` and the returned snapshot reference in `r` within the
same live RPC session. Roots, depth, and flags must match the baseline scope.

## Retained-result formulas

| ID   | Operands                             | Meaning                                |
| ---- | ------------------------------------ | -------------------------------------- |
| `/`  | `[@r,offset,length]`                 | zero-based byte slice                  |
| `#`  | `[@r,offset,length]`                 | one-based line slice                   |
| `?`  | `[@r,offset,length,query,flags]`     | bounded search                         |
| `-`  | `[@r]`                               | release one reference                  |
| `\|` | `[@r,table,offset,length,column...]` | ordered table projection               |
| `>`  | `[@r,path]`                          | no-overwrite workspace materialization |

These formulas consume aliases created earlier in the same live RPC session.
Example projection:

```ason
t:1
i:44
o:|
a:[@7,d,0,64,p,l,t]
u{tok,rec,ms}:
256,64,30000
```

Example retained search:

```ason
t:1
i:43
o:?
a:[@7,0,1048576,TODO,0]
u{tok,rec,ms}:
256,64,30000
```

## Cancellation

Cancellation is meaningful only while its target is queued or active in the
same live RPC session. The cancellation request ID must differ from its target:

```ason
t:1
i:42
o:k
a{i}:
41
u{tok,rec,ms}:
64,1,30000
```

Cancellation is idempotent. Completion means ash has handled the target state; process termination additionally waits for owned descendants and inherited pipes.
