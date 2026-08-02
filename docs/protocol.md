# ASH/1 protocol and ASON

Status: canonical ASON, framing, capability negotiation, action-bound approval permits, typed exec/read/list/search/patch/fs/batch/snapshot/cancel schemas, direct retained-result formula operators, disk-backed process spooling, durable file transactions, bounded DAG execution, retained-result inspection, and runtime paths are implemented

ASH/1 is the typed session protocol of `ash`. ASON is its native LLM-facing serialization. Both are specified and implemented inside this project; ASON is not an adapter around another data format.

## 1. Protocol layers

ASH/1 separates three concerns:

1. **Semantic model** — typed requests, programs, events, results, errors, references, and capabilities.
2. **Framing** — message boundaries on a persistent byte stream.
3. **ASON encoding** — canonical text presented to the agent and carried by the initial stdio transport.

The same semantic request is used by persistent sessions and one-shot CLI calls. One-shot arguments are decoded directly into a request; stdout contains one bare ASON result document terminated by EOF.

## 2. ASON goals

ASON means **Agent Serialized Object Notation**. It is optimized for model accuracy per completed task, not for human readability.

The format is designed to:

- declare repeated field names once;
- use operation schemas instead of repeating type information;
- avoid object braces, closing tags, comments, and decorative whitespace;
- represent homogeneous records as compact tables;
- reuse session-local path and result identifiers;
- keep errors structural and stable;
- remain incrementally parseable with hard resource limits;
- produce exactly one canonical encoding for a typed value.

ASON is not intended to replace JSON in public APIs, configuration files, release manifests, or databases. It is the native Agent-to-`ash` notation.

## 3. Core syntax

ASON is UTF-8. `LF` is the only encoded line separator. A canonical document has no blank lines, comments, indentation, or trailing spaces.

Field and column keys begin with an ASCII letter or `_`; remaining bytes are ASCII letters, digits, `_`, `.`, or `-`. Keys are case-sensitive. Protocol-defined keys use lowercase ASCII. A parser rejects duplicate top-level fields and duplicate columns before schema validation.

### 3.1 Scalar field

```ason
key:value
```

The schema defines the value type. ASON does not guess dates, numbers, booleans, paths, or identifiers from a string.

### 3.2 Record

```ason
key{field_a,field_b,field_c}:
value_a,value_b,value_c
```

The header declares fields once. Exactly one row follows.

### 3.3 Homogeneous table

```ason
key[2]{field_a,field_b}:
a1,b1
a2,b2
```

The declared row count is mandatory. Exactly that number of rows follows, allowing a parser to find the next top-level field without indentation or closing syntax. Streaming operations emit several independently framed tables rather than an unknown-length table.

### 3.4 Vector

```ason
key:[a,b,c]
```

Vectors contain values of the element type declared by the schema. Nested unbounded vectors are prohibited.

### 3.5 Null and references

```ason
value:~
result:@7
```

`~` is explicit null. `@` followed by an unsigned decimal session identifier is a typed reference. References are meaningful only in the session that issued them.

### 3.6 Strings

A string may be bare only when every byte is valid UTF-8 and every character is in this set:

```text
A-Z a-z 0-9 _ . / @ + - # ? | >
```

All other strings use double quotes with `\\`, `\"`, `\n`, `\r`, `\t`, and `\u{...}` escapes. The encoder quotes a string whenever a bare representation could conflict with a reserved scalar in its schema position.

Large strings are not serialized inline. They are retained and represented by a result reference plus a bounded excerpt.

### 3.7 Numbers and booleans

- Signed and unsigned integers use canonical base-10 notation with no leading zeroes.
- Floating-point fields use the shortest finite decimal that round-trips to the declared type.
- Non-finite floats are rejected unless an operation defines a dedicated enum value.
- Booleans encode as `0` and `1` because the schema carries the boolean type.
- Enums use stable non-negative integer discriminants.

### 3.8 Binary values

Binary values are never Base64-encoded in the default Agent response. A small binary field may use lowercase hexadecimal only when its operation schema explicitly permits it. All other binary data is retained as an artifact reference.

## 4. Canonicalization

Canonical ASON is required for fixtures, digest binding, permits, and token benchmarks.

- Top-level fields follow the order defined by the message schema.
- Record and table columns follow schema order.
- Optional absent fields are omitted; explicit null remains `~`.
- Maps whose keys are not statically known are sorted by UTF-8 byte order.
- Strings use the shortest valid escaped representation.
- No insignificant whitespace is emitted.
- A document ends with exactly one `LF` in text mode.
- Duplicate fields, duplicate dynamic-map keys, and extra table cells are errors.

Canonicalization is performed from typed values. Text received from a caller is parsed and then re-encoded before it is used in a permit digest.

The syntax decoder preserves non-reserved atoms as text and does not infer a number, boolean, enum, path, or identifier. Operation-schema decoding performs those conversions and enforces their canonical numeric form. This keeps lexical parsing deterministic without guessing a field's type.

## 5. Framing

Persistent `ash rpc` uses:

```text
4-byte unsigned big-endian payload length
N bytes canonical ASON payload
```

The length prefix is transport metadata and is never shown to the LLM. The initial hard payload ceiling is 8 MiB, with a lower negotiated session default. Large content must use streaming events or result references.

Before handshake negotiation, the implementation accepts at most 1 MiB per frame. Negotiation may lower or raise that limit within `256..=8388608`; the 256-byte floor guarantees that every peer can carry a structural control result. The selected immediate-output ceiling is also capped by the selected frame size.

A receiver must reject a frame before allocation when its declared length exceeds the negotiated ceiling. Zero-length frames are invalid. EOF inside a frame terminates the session with a framing error and cancels owned programs.

## 6. Handshake

The client opens a session with a handshake containing:

- ASH protocol major and supported minor range;
- ASON format major and supported minor range;
- maximum frame and desired output budgets;
- supported operation and reducer capabilities;
- workspace capability request;
- client session nonce.

The ASH/1.0 client request is encoded with this exact core schema:

```ason
t:0
i:7
a{ap,al,ah,zp,zl,zh,frm,out,ops,cap,root,n}:
1,0,0,1,0,0,1048576,65536,1023,15,.,nonce-7
```

`ap` and `zp` are the ASH and ASON major versions; `al..ah` and `zl..zh` are supported minor ranges. `frm` and `out` request frame and immediate-output byte ceilings. `ops` and `cap` are unsigned capability masks. `root` is the requested logical workspace root, and `n` is a client nonce of at most 128 bytes.

The server returns the selected versions, effective limits, operation capability bitmap, platform identifiers, and session identifier. The ASH/1.0 compact field dictionary is fixed by this specification and retained by the adapter rather than repeated in every handshake.

```ason
t:0
i:7
s:0
d{ap,av,zp,zv,frm,out,ops,cap,os,arch,sid,n}:
1,0,1,0,1048576,65536,1023,15,linux,x86_64,1,nonce-7
```

The response echoes the nonce and request identifier. Limits and masks are intersections, never expansions, of client requests and server capabilities. The current source checkpoint advertises operation mask `0x3ff`, exactly all ten ASH/1 operation-family bits: `exec`, `read`, `list`, `search`, `patch`, `fs`, `batch`, retained formulas, `snapshot`, and `cancel`. The six retained-formula symbols share bit 7. Its capability mask is `0x0f`: bit 0 workspace read, bit 1 workspace write, bit 2 direct host-process execution, and bit 3 retained-result access. Unknown response capability bits are invalid. A client that requests no capability receives a structurally valid session in which only capability-free control operations can run.

The handshake is retained by the adapter and is not repeated in each model-visible result.

## 7. Message envelope

Every message begins with a type, request identifier, and type-specific fields. Core field names are deliberately short and stable:

| Field | Meaning |
| --- | --- |
| `t` | message type |
| `i` | request or program identifier |
| `o` | operation identifier |
| `a` | typed operation arguments |
| `u` | resource and output budget |
| `v` | optional opaque approval permit |
| `s` | final status |
| `p` | path dictionary delta |
| `d` | result data |
| `e` | structured error |
| `z` | result flags |
| `r` | retained result reference |

Message types are numeric enums:

| Value | Type |
| ---: | --- |
| `0` | handshake |
| `1` | request |
| `2` | event |
| `3` | final result |
| `4` | cancellation |

## 8. Example request and result

Search request:

```ason
t:1
i:17
o:g
a{q,p,f}:
TODO,[src],0
u{tok,rec,ms}:
256,64,30000
```

Search result:

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

The path `src/lib.rs` is introduced once with identifier `1`. Later messages in the same session emit only `1`. If the search is reduced, `z` contains the reduction flag and `r` points to the retained full result.

## 9. Core operation identifiers

The first protocol level reserves single-character presentation identifiers:

| Identifier | Mask bit | Operation | Required result behavior |
| --- | ---: | --- | --- |
| `x` | 0 | direct process execution | normalized termination plus bounded stream projections |
| `r` | 1 | file read | explicit ranges, digests, and path identifiers |
| `l` | 2 | list, glob, or stat | compact paths and typed metadata |
| `g` | 3 | literal or regex search | homogeneous match records |
| `p` | 4 | compare-and-swap patch | per-file commit or conflict records |
| `f` | 5 | filesystem mutation | journaled mutation outcomes |
| `b` | 6 | batch or dependency graph | node-status table and selected outputs |
| `/` | 7 | retained byte slice | bounded zero-based byte range |
| `#` | 7 | retained line slice | bounded one-based line range |
| `?` | 7 | retained search | bounded literal or regex matches |
| `-` | 7 | retained release | drop one session reference |
| `\|` | 7 | retained projection | row window plus ordered columns |
| `>` | 7 | retained materialization | write immutable bytes without overwrite |
| `s` | 8 | workspace snapshot or delta | versioned file-change records |
| `k` | 9 | cancellation | acknowledged target and final cancellation state |

These are the only canonical identifiers in the current pre-release source. No legacy formula identifier or alias is accepted. Internal Rust enum ordering is not part of the protocol.

### 9.1 Core request schemas

Every core request uses the exact envelope order `t,i,o,a,u`, or `t,i,o,a,u,v` when retrying with an approval permit. No other optional or reordered envelope shape is valid. The budget record is always:

```ason
u{tok,rec,ms}:
256,64,30000
```

`tok` is the maximum immediate model-token projection requested by the adapter, `rec` is the emitted-record ceiling, and `ms` is the wall-clock deadline in milliseconds. The engine additionally applies the lower handshake, session, and system ceilings.

Operation argument records are positional only after their declared columns, so a model emits field names once while the schema decoder still rejects reordered or surplus values:

| Operation | Argument header | Semantics |
| --- | --- | --- |
| `x` | `a{x,v,c,e,in,f}:` | executable, argv vector, logical cwd, environment deltas, stdin source, flags |
| `r` | `a{p,m,o,n}:` | path vector, range mode, offset, length |
| `l` | `a{p,d,f}:` | root path vector, maximum depth, flags |
| `g` | `a{q,p,f}:` | query, root path vector, flags |
| `p` | `a{p,h,i,o,n,v,f}:` | sorted paths, expected digests, edit file indexes, byte offsets, delete lengths, replacements, flags |
| `f` | `a[N]{i,k,p,q,h,v}:` | action ID, action kind, source or create path, optional destination, optional preimage digest, optional content |
| `b` | `a[N]{i,d,o,a}:` | node ID, dependency-ID vector, leaf operation ID, canonical nested argument document |
| `s` | `a{p,d,m,r,f}:` | sorted roots, maximum depth, capture mode, optional baseline reference, flags |
| `/ # ? - \| >` | `a:[...]` | one retained-result formula; the operation symbol fixes the exact operand schema |
| `k` | `a{i}:` | active target request identifier |

`exec` invokes `x` directly. Environment entries use `NAME=value` to set and `-NAME` to remove; duplicate names are invalid. `in` is `~`, inline text, or a retained `@reference`. Exec flag bit 0 clears the inherited environment before applying deltas.

Read mode `0` is a zero-based byte range and mode `1` is a one-based line range. A zero length is invalid. List flag bits are 0 include hidden, 1 files only, and 2 directories only. Search flag bits are 0 regular expression, 1 case-insensitive, and 2 include hidden. Unknown flag bits fail schema validation rather than being silently ignored.

Patch paths are unique and sorted by UTF-8 bytes. The `h` vector contains one lowercase 64-digit BLAKE3 digest per path. The `i`, `o`, `n`, and `v` vectors are aligned edits: zero-based path index, zero-based byte offset in the original file, deleted byte length, and replacement. A replacement is inline text or an immutable `@reference`, so binary or previously retained content does not need to be repeated. Edits are sorted by file index and offset and cannot overlap. Every path has at least one edit, all paths must already be regular files, and the current flag value is `0`.

Patch preparation reads, hashes, and constructs independent files on the compute plane under aggregate byte ceilings. No file is changed if preflight finds a stale digest. Commit is serialized across threads and processes per workspace and revalidates every preimage before mutation. Original files and staged replacements each have a 128 MiB aggregate ceiling and are persisted with exact sizes and BLAKE3 identities in the checksummed workspace transaction journal. Same-directory compare-and-swap replacement preserves permissions where supported; a later conflict or filesystem failure rolls visible replacements back in reverse order. After interruption, the next mutation rolls an uncommitted patch back or finalizes cleanup after its durable commit marker. Ambiguous or externally modified content remains untouched and returns error `502`.

Filesystem action kinds are `0` create, `1` copy, `2` move, and `3` remove. Create requires inline text or an immutable `@reference` in `v`; the other actions omit content. Copy and move require destination `q` and a lowercase 64-digit BLAKE3 source digest in `h`; remove requires the digest and omits `q`. Action IDs are positive and increasing, all source and destination paths are distinct, and the table contains at most 256 actions. Files are bounded to 128 MiB and the transaction's combined staged or hashed input is bounded to 128 MiB. Only regular files are accepted. Create, copy, and move never overwrite, and recursive or directory mutation has no ASH/1.0 encoding.

The complete response budget is reserved before mutation. The implementation serializes a workspace across threads and processes, stages content, persists a checksummed journal, publishes actions using reversible same-filesystem links or renames, and writes a durable commit marker before cleanup. A clean conflict reverses earlier actions and returns status `8`; cancellation and deadlines are checked during lock waits, request hashing, and staging. Before the next mutation transaction, recovery rolls an uncommitted journal back or finalizes committed cleanup. If exact size, digest, or journal state cannot prove a safe action, the journal and workspace are left untouched and error `502` requires inspection or approval.

A batch table contains 1 through 256 nodes and at most 4096 dependency edges. Node rows are sorted by increasing `i`; every `d` vector is sorted, unique, and names nodes in the same table. The entire graph is checked for unknown dependencies, self-edges, duplicate IDs, and cycles before any node can run. The nested `a` cell is a quoted canonical one-field ASON document, for example `"a{q,p,f}:\nTODO,[src],0\n"`. This keeps heterogeneous arguments typed without paying for a sparse union table. Filesystem transactions are valid leaf nodes; batch and cancellation cannot be nested.

Ready nodes execute concurrently under the global/session node governor. Each node receives deterministic token, record, and output shares, the parent deadline and cancellation token, and a node-local path dictionary so sibling completion order cannot change child bytes. The current batch edge is a control dependency: a node starts only after every dependency succeeds. Runtime output binding and stream edges are reserved for a later compatible extension.

Snapshot mode `0` captures current state and requires `r:~`; mode `1` emits a delta from the session-local snapshot `@reference`. The roots, depth, and flags must match the baseline scope exactly. Snapshot flag bit 0 includes hidden entries. Files are streamed through bounded BLAKE3 buffers on the compute plane, symlink targets are hashed without following them, and the canonical manifest is always retained as the response reference so deltas can be chained without resending the tree.

Reference work is encoded as prefix data formulas, not as a sparse union with a numeric mode and unused nullable slots. The operator names the type and its single vector contains only that operator's operands:

| Algebra | Opcode and canonical operand vector | Meaning |
| --- | --- | --- |
| `β(@r,o,n)` | `o:/` and `a:[@r,o,n]` | zero-based byte slice |
| `λ(@r,o,n)` | `o:#` and `a:[@r,o,n]` | one-based line slice |
| `σ(q,@r[o:o+n],f)` | `o:?` and `a:[@r,o,n,q,f]` | bounded literal or regex search |
| `drop(@r)` | `o:-` and `a:[@r]` | release the retained value |
| `π_C(T[o:o+n])` | `o:|` and `a:[@r,T,o,n,C...]` | project ordered columns `C` from table field `T` |
| `μ(path,@r)` | `o:>` and `a:[@r,path]` | materialize the bytes as a workspace file |

The keyboard symbols are the mathematical syntax, not display aliases. The report compares the former ASCII wrapper, direct Greek glyphs, direct ASCII letters, and this canonical set under pinned `cl100k_base` and `o200k_base`: `/ # ? - | >` matches the direct-letter floor at 80/80 tokens and 126 bytes, while Greek costs 86/86 tokens and 132 bytes and the wrapper costs 97/98 tokens and 150 bytes. Formula arity is exact, so surplus operands fail before dispatch. Projection preserves source row order, accepts at most 128 unique ASON column keys, and applies the response record/output budget after the requested row window. Byte and search offsets are zero-based; line offsets are one-based. Search flag bits are 0 regular expression and 1 case-insensitive. Slice projections return UTF-8 when valid and lowercase hexadecimal otherwise; the full selected range remains identified by its digest and source reference.

`cancel` is a control-plane request. Its own request identifier must differ from the target. State `1` means cancellation was signaled to queued or running work; state `0` means the target was no longer active. Both are successful, idempotent outcomes.

### 9.2 Capability and approval binding

Required capabilities are derived from typed arguments rather than supplied by the caller: `exec` requires host process; `read`, `list`, `search`, and `snapshot` require workspace read; `patch` and `fs` require workspace write; reference formulas require retained-result access, and `>` additionally requires workspace write; a batch requires the union of its leaves. Cancellation deliberately requires no capability so gated or queued work can always be stopped. Workspace write authorizes the reads necessary to validate a guarded mutation. Host-process execution is a stronger boundary: the child inherits the operating-system access of `ash`, so it is not constrained by the workspace read/write bits.

A session policy splits the negotiated mask into direct grants and capabilities that require per-action approval. A missing permit returns status `3`, error `301`, retry class `3`, authorization stage `3`, and a retained evidence reference. Invalid, expired, mismatched, or replayed permits return error `302` and a fresh challenge. A capability outside the policy returns error `300` with retry class `0` and no side effect.

The retained challenge is canonical ASON with exact field order `v,s,c,e,b,p,h,n`: challenge version, session ID, required capability mask, Unix expiry in milliseconds, 16-byte session binding, 16-byte policy digest, 32-byte normalized-action digest, and 16-byte nonce. Binary fields are lowercase hexadecimal. The normalized action is independently re-encoded as the exact document `v,o,a,c`; it excludes request ID, budget, and permit so a semantic retry may change transport identity or budget without changing the approved action.

The `v` request field is a fixed 48-byte opaque token encoded as 96 lowercase hexadecimal characters: the 16-byte nonce plus a 32-byte domain-separated keyed BLAKE3 authenticator. The authority keeps the corresponding 105-byte challenge signing value in a bounded, expiring pending map, avoiding repetition of that value in the LLM-facing retry. Verification binds the permit to the action, required capabilities, session ID, fresh session binding, policy identity, expiry, and nonce. A successful verification removes the pending challenge and consumes the nonce in a bounded replay cache before execution, so a permit is one-shot even when the operation later fails.

The trusted embedding harness supplies the 32-byte authority key and a fresh 16-byte session binding; neither crosses ASH/1. The harness retrieves the challenge, obtains external approval, and calls the issuer only after approval. A policy that expects challenge retrieval through ASH must directly grant retained-result access. The standalone CLI has no approval UI or secret-key input and directly grants only the capability intersection selected by its handshake; embedders opt into approval policy through `ash-ops`.

## 10. Process result

A process result contains:

- normalized termination kind;
- exit code or platform signal code when applicable;
- elapsed monotonic time;
- stdout and stderr projections;
- stdout and stderr retained references when present;
- truncation and redaction flags;
- observed workspace delta when requested.

The current `exec` path drains stdout and stderr concurrently. Each stream keeps at most 4 MiB in memory; after crossing that boundary its complete bytes move to a session-private disk spool while only a bounded head/tail sample remains resident for projection. The full prefix and every later chunk are charged incrementally against the session retained-byte ceiling before disk write. Streams needing references are BLAKE3-identified on the Rayon compute plane and committed atomically in stdout/stderr order. A successful truncated projection therefore points to every original byte, including bytes beyond the memory ceiling. Byte slicing reads only the requested spool range; full-value consumers enforce their own 8, 64, or 128 MiB work limits. Insufficient quota returns storage-budget error `601` with no partial reference, and reference release or session shutdown removes its spool files after active leases end.

Each spool root carries an exact versioned owner marker and an exclusive lifetime lock. On the first result-store construction of a later process, a root older than one hour is reclaimed only if the lock proves it inactive and every entry is a recognized regular non-symlink spool file. Cleanup is non-recursive and fail-closed: active, recent, malformed, or foreign roots are not modified.

Core result data uses these schemas; reference projection intentionally substitutes its validated requested column vector for `C...`:

| Operation | Result header | Row or record semantics |
| --- | --- | --- |
| `x` | `d{k,c,ms,o,e,ro,re}:` | termination kind, code, elapsed ms, stdout/stderr projections, stdout/stderr references |
| `r` | `d[N]{p,o,n,h,t,r}:` | path ID, actual offset and length, BLAKE3 digest, text projection, retained reference |
| `l` | `d[N]{p,k,z,m}:` | path ID, file kind, byte size, optional modified Unix milliseconds |
| `g` | `d[N]{p,l,c,t}:` | path ID, one-based line and column, matching line projection |
| `p` | `d[N]{p,s,h}:` | path ID, mutation state, resulting or observed BLAKE3 digest when known |
| `f` | `d[N]{i,k,p,q,s,h}:` | action ID, action kind, path ID, optional destination path ID, action state, resulting or observed digest |
| `b` | `d[N]{i,o,s,c,r}:` | node ID, operation, node state, child final status, retained child-response reference |
| `s` | `d[N]{p,c,k,z,h}:` | path ID, change kind, file kind, byte size, optional BLAKE3 digest |
| `/ # ? - \| >` | `d{o,n,p,h,t,b}:`, `d[N]{o,l,c,t}:`, `d[N]{C...}:`, `d{p,s,z,h}:`, or `d{r,z}:` | byte/line slice, search, release, projection, or materialization selected directly by the request symbol |
| `k` | `d{i,z}:` | target request identifier and cancellation state |

Null (`~`) omits an unavailable projection, code, timestamp, or reference. Result flag bits are 0 truncated, 1 reduced, 2 normalized text, 3 retained evidence, 4 partial completion, and 5 redacted. Unknown bits are invalid. Any truncated result must retain inspectable evidence, and the retained flag must agree with the references actually present.

Patch and filesystem action state values are `0` committed, `1` conflict, `2` rolled back, `3` recovery required, and `4` skipped. A clean stale-preimage or no-overwrite conflict uses status `8` and error `501`. If an atomic outcome is indeterminate, a durable journal is ambiguous, or rollback cannot restore a preimage, error `502` and result flag bit 4 make the partial state explicit; retry class `3` requires external inspection or approval rather than an automatic retry.

Batch node states are `0` succeeded, `1` failed, `2` skipped, and `3` cancelled. Field `c` is the child's final status; a skipped node has `c:~` and `r:~` because it never ran. Every executed child response is retained atomically in node-ID order, so `r` remains stable even when workers finish in a different order. An all-success graph returns final status `0` with retained flag bit 3. Any failed or cancelled node produces program status `5`, error `800`, and both retained and partial flag bits; only its transitive descendants are skipped, while independent branches finish normally. The compact table is the immediate model context, and the formula operators retrieve any full child response on demand.

Snapshot change values are `0` present in a full capture, `1` added, `2` modified, and `3` removed. File kinds reuse the list schema. Every snapshot response sets the retained flag and returns the new manifest reference, even when a delta is empty; truncation therefore never loses the full state transition.

Success with `status-only` output does not repeat empty stdout or stderr fields. Failure reserves a diagnostic budget even if normal output has exhausted its allocation.

## 11. Events and streaming

Long-running operations may emit event frames. Events are typed as lifecycle, progress counters, projected output, diagnostic records, or artifact availability. Human prose progress messages are not a core event type.

Each event has a monotonically increasing sequence number per request. A final result reports the last sequence number so the adapter can detect loss. Events are advisory unless their schema marks them as retained evidence.

Output events respect the same total budget as the final projection. A caller cannot evade a token ceiling by requesting many small stream frames.

The RPC transport registers each data-plane request before reading the next frame, executes independent requests concurrently under the session governor, and processes cancellation frames without waiting for earlier work to finish. Final response frames are emitted in request-input order, so scheduling completion order cannot change the byte stream. Both executing requests and the not-yet-emitted response window are bounded; once the latter is full, framed input receives transport backpressure until its leading request completes.

## 12. Budgets and truncation

Core budgets include:

- wall-clock milliseconds;
- maximum parallel nodes and child processes;
- bytes captured per stream;
- bytes retained per program and session;
- records emitted;
- estimated or negotiated model tokens;
- maximum path dictionary growth.

Result flag bits identify truncation, reduction, redaction, normalized text, partial graph completion, and retained full evidence. A truncation flag without either a retained reference or an explicit `retain=0` request is an internal error.

## 13. Path dictionary

Paths are normalized once and assigned increasing unsigned session identifiers. A response carries only new entries. Identifiers are never reused during a session, even after the referenced path disappears. Retained child responses inside a batch use independent node-local dictionaries starting at `1`; each child is therefore self-contained and byte-stable under concurrent sibling execution.

The dictionary maps a logical workspace path, not an unchecked native path. Opaque non-UTF-8 Unix paths receive identifiers but no lossy text value.

## 14. Result references

Result references identify immutable retained content or structured record sets. The store tracks full content digests internally; the short ASON identifier is only an alias.

The `/ # ? - | >` operation family is designed to:

- fetch byte, line, or record ranges;
- search within a retained value;
- apply a different deterministic reducer;
- project selected table columns;
- materialize binary content as a workspace artifact;
- release retained content early.

The current source implements all six formulas. `|` decodes bounded UTF-8 ASON, selects one top-level table, slices its rows, and emits only the ordered requested columns; malformed structured content, a non-table field, or an unknown column fails explicitly. `>` holds a source lease, reserves its complete response before mutation, and routes the immutable bytes through the same workspace-confined, journaled, no-overwrite transaction used by `fs create`. Its result record is path ID, transaction state, source byte size, and resulting or observed digest. Slice field `p` is the number of projected source bytes, while `n` is the full selected byte length. A busy reference cannot be released until active readers drop their leases.

References are immutable and session-local. The current store enforces byte and entry quotas, never reuses a retired alias, and supports explicit release; TTL metadata is reserved for a later negotiated extension. Unknown, retired, or foreign-session identifiers return a stable reference error.

## 15. Errors

Errors are typed records, not prose. The core error record includes numeric code, retry class, operation stage, compact arguments, and optional evidence reference.

```ason
t:3
i:21
s:5
e{c,q,p,x,a}:
31,0,4,@8,@9
z:0
r:@10
```

The schema negotiated for error code `31` defines the meanings and types of its payload slots. Agent instructions need describe each stable code only once.

Error code families reserve ranges for protocol, validation, capability, path, process, filesystem, budget, reference, and internal failures. Capability codes `300`, `301`, and `302` identify policy denial, a required approval permit, and an invalid or consumed permit. Filesystem codes `501` and `502` identify a compare-and-swap content conflict and a mutation requiring recovery. Budget codes `600`, `601`, and `602` identify immediate output, retained storage, and in-flight concurrency ceilings respectively. Reference codes `700` and `701` identify unknown and currently leased aliases. New minor protocol levels may add codes but cannot change an existing code's meaning.

Before a request identifier exists, CLI bootstrap failures are written to stderr as bare ASON rather than prose:

```ason
s:1
e{c}:
4
```

Bootstrap codes are `1` usage, `2` input ceiling, `3` invalid UTF-8, `4` ASON, `5` framing, `6` handshake or uncorrelatable message schema, `7` missing handshake, `8` unavailable message type, `9` I/O, and `10` internal model construction. Once a valid request ID exists, failures use a normal framed ASH result instead of stderr. These are CLI diagnostics, not ASH request error codes.

## 16. Parser security

The parser enforces limits before or during allocation:

- frame bytes;
- document lines;
- fields per record;
- table rows and columns;
- vector length and nesting depth;
- decoded string bytes;
- escape length;
- path dictionary entries;
- total typed values per message.

Malformed input never produces a partially executable program. Decoding, schema validation, canonicalization, and capability validation all complete before side effects begin.

## 17. Compatibility

No supported ASH release has been published. Until the first release tag, `main` is the protocol design space: a better typed or lower-token formula may deliberately replace an earlier fixture without a compatibility shim. The canonical fixtures describe the current source, not a promise to unreleased builds.

ASH protocol and ASON format versions are negotiated independently. An ASH minor release may add optional fields or operations. An ASON minor release may add canonical scalar forms or table features without changing operation semantics.

Major changes require new canonical fixtures. Implementations must keep fixtures for every supported major version and verify that one-shot and persistent encodings produce the same typed result.
