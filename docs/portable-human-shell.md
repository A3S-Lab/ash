# Portable Human Shell Architecture

- Status: accepted; H1 implemented
- Target: post-ASH/1 version one
- Last updated: 2026-08-07

This document defines the accepted architecture for an optional human-facing
shell for `ash`. Contracts not explicitly labeled as the current source
checkpoint remain design targets, and this architecture does not change the
current ASH/1 machine protocol contract. The current product remains an
agent-first, typed execution boundary. The human shell is a separate frontend
that reuses the portable execution and filesystem foundations without
weakening their machine-facing semantics.

The current source checkpoint has completed H0 with the independent
`a3s-ash-shell` crate, a source-spanned simple-command parser, AST, diagnostics,
persistent state types, deterministic command classification, locked parser and
resolution fixtures, and provider-neutral raw read/list/search semantic
services. Existing ASH/1 adapters reuse those services while retaining permit,
deadline, budget, projection, retention, and ASON ownership. H1 now provides a
feature-gated, line-edited `ash shell` REPL with configurable prompt, private
persistent history, opt-in Profile startup, and `exit`, plus inline, bounded
stdin, and native script-file sources. One persistent state executes sequential
`pwd`, `echo`, `cd`, `export`/`unset`, portable `ls`, bounded raw-byte `cat` and
text `grep`, source-spanned named and last-status expansion, and direct-argv
native host commands. Pipelines, foreground interactive programs and jobs,
streaming stdio, broader expansion, mutations, and WSL launch remain
unimplemented.

## 1. Executive decision

Add a new `a3s-ash-shell` crate and expose it through `ash shell`. The shell
will provide a Linux-inspired command language on Linux, macOS, and Windows,
while keeping three kinds of execution explicit:

1. **Portable commands** such as `ls`, `cat`, `grep`, `cp`, `mv`, and `rm` are
   implemented by `ash` and operate through native platform APIs. On Windows,
   these commands manipulate Windows files directly; WSL is not involved.
2. **Native external commands** are resolved through the host operating
   system and launched directly with an argument vector.
3. **Linux external commands on Windows** use an explicit WSL backend. The
   shell never silently reroutes an unresolved native command to WSL.

The shell language is an `ash` dialect with a documented POSIX-inspired
subset. It is not advertised as Bash compatible. Bash scripts and Linux ELF
binaries require an explicit Bash or WSL execution path.

Real shell pipelines are represented by a dedicated streaming process plan.
They are not lowered to the existing batch DAG because a batch dependency
means that one node completes before another becomes runnable, while a pipe
requires producer and consumer processes to run concurrently with operating
system backpressure.

## 2. Compatibility vocabulary

The project must use precise compatibility terms.

| Term | Meaning |
| --- | --- |
| Portable command semantics | An `ash` implementation with the same documented behavior on supported hosts. |
| Native external command | A host executable launched without an implicit shell. |
| Shell-language compatibility | Compatibility with parsing, expansion, redirection, pipelines, status, and scripting rules. |
| Linux binary compatibility | The ability to execute Linux ELF binaries and Linux system calls. |
| WSL backend | An explicit Windows adapter that delegates a command to a selected WSL distribution. |

Portable command semantics do not imply GNU option-for-option compatibility.
Shell-language compatibility does not imply Linux binary compatibility.
Windows cannot execute an unmodified Linux ELF binary through a new parser or
command resolver; that case requires WSL, a container, or a virtual machine.

## 3. Goals

- Provide a human REPL with persistent current directory, environment,
  variables, history, aliases, functions, and last status.
- Provide a small, versioned, Linux-inspired command language on Linux, macOS,
  and Windows.
- Make common file and text commands operate natively and consistently on all
  three platforms.
- Preserve direct executable-plus-argv launch. Never insert `sh -c`, `cmd /c`,
  or `pwsh -Command` implicitly.
- Support streaming pipelines, ordered file-descriptor redirections, and
  foreground and background jobs.
- Allow foreground interactive programs to inherit the terminal.
- Add managed Unix PTY and Windows ConPTY support only where shell-owned
  terminal sessions are required.
- Keep native, portable, and WSL command resolution visible and testable.
- Reuse the engine governor, cancellation model, process-tree ownership,
  filesystem services, and platform contracts where their semantics fit.
- Keep all existing ASH/1 canonical fixtures and machine behavior unchanged.

## 4. Non-goals

- Full Bash, Zsh, Fish, PowerShell, or CMD compatibility.
- Transparent execution of Linux ELF binaries on the Windows kernel.
- Emulation of Linux-only resources such as `/proc`, `/sys`, devices, kernel
  modules, namespaces, or Linux permission behavior on NTFS.
- Silent fallback from native execution to WSL.
- Parsing a command line and forwarding it to a host shell as the normal
  execution path.
- Replacing the existing ASH/1 request IR with shell strings.
- Treating a workspace capability as a complete sandbox for arbitrary child
  processes.
- Reimplementing package managers, compilers, Git, or every GNU utility.

## 5. System shape

```text
                         +---------------------------+
Coding agent / harness ->| ASH/1 machine frontend    |
                         | canonical ASON, bounded   |
                         +-------------+-------------+
                                       |
                                       v
                         +-------------+-------------+
Human terminal --------->| Portable human frontend   |
                         | parser, state, jobs, UX    |
                         +-------------+-------------+
                                       |
                          typed calls  |  process plans
                                       v
                 +---------------------+---------------------+
                 | Shared semantic services and governor     |
                 | filesystem, search, mutation, cancellation|
                 +---------------------+---------------------+
                                       |
                 +---------------------+---------------------+
                 | Platform execution boundary               |
                 | native process, pipes, terminal, PTY, WSL |
                 +-------------+-----------------------------+
                               |
             +-----------------+------------------+
             |                 |                  |
          Windows            Linux              macOS
          native APIs        native APIs        native APIs
             |
          explicit WSL backend for Linux binaries
```

The two frontends share semantic services, not presentation formats. The human
shell must not encode an ASON request and decode its own response merely to call
a local operation. The ASH/1 adapter remains responsible for capability
checks, reduction, retention, canonical encoding, and machine error mapping.
The human adapter renders terminal output and maintains shell state.

## 6. Crate ownership

### 6.1 New `a3s-ash-shell` crate

The crate owns:

- line editing, prompt integration, history, and completion;
- lexer, parser, shell AST, expansion, and lowering;
- mutable `ShellState`;
- stateful and portable builtins;
- command resolution and explicit backend selection;
- pipeline supervision, foreground jobs, background jobs, and status mapping;
- terminal-oriented rendering and diagnostics;
- script-file execution for the documented `ash` dialect.

The crate must be usable as a library. `ash-cli` adds only the `ash shell`
route and lifecycle wrapper.

Suggested internal layout:

```text
crates/ash-shell/src/
|-- lib.rs
|-- repl.rs
|-- syntax.rs
|-- parser.rs
|-- expand.rs
|-- state.rs
|-- builtin.rs
|-- resolver.rs
|-- plan.rs
|-- execute.rs
|-- jobs.rs
|-- terminal.rs
`-- diagnostic.rs
```

### 6.2 `ash-ops`

Refactor operation implementations into two layers without changing their
external behavior:

1. semantic services returning typed raw results and typed errors;
2. the existing ASH/1 adapter that applies budgets, projection, retention, and
   `FinalResponse` construction.

The human shell calls semantic services directly. This avoids duplicating
portable listing, reading, searching, hashing, and mutation behavior while
also avoiding ASON as an internal API.

Interactive filesystem mutations may derive required preimage identities
immediately before the operation, but a detected conflict must be reported to
the user rather than silently retried. Existing transactional and no-overwrite
guarantees remain authoritative.

### 6.3 `ash-engine`

Keep the current typed program DAG and bounded governor. Add only reusable
permits and cancellation contexts needed by shell jobs. Do not represent a
streaming pipeline as ordinary DAG completion dependencies.

### 6.4 `ash-platform`

Keep the current machine-oriented `ProcessSpec` behavior stable. The native
`NativeProcessSpec` launch API now sits underneath it for already-resolved
native paths, native argument/environment strings, and direct process-tree
creation. Generalized standard-I/O and terminal-attachment modes remain staged;
the existing `ProcessSpec` still selects null or captured pipes exactly as it
did before this API was added.

The platform layer owns:

- native command lookup and argument-vector process creation;
- anonymous pipe construction and handle inheritance;
- foreground process groups and process-tree ownership;
- inherited-terminal attachment;
- optional Unix PTY and Windows ConPTY sessions;
- interrupt, terminate, suspend, resume, wait, and resize where supported;
- Windows-to-WSL command and path adaptation behind an explicit backend;
- native environment and path rules.

### 6.5 `ash-protocol` and `ash-store`

ASH/1 does not gain a free-form shell-string operation as part of this design.
If remote or agent-driven interactive sessions are added later, they require a
separate capability and protocol proposal.

The result store remains the machine capture boundary. Human foreground output
normally streams to the terminal and is not reduced or retained. An explicit
capture feature may opt into the store.

## 7. Shell frontend pipeline

Each submitted command passes through these stages:

```text
source text
  -> lexical tokens with source spans
  -> lossless shell AST
  -> expansion against ShellState
  -> command and backend resolution
  -> typed ShellPlan
  -> execution and job supervision
  -> state/status update and terminal rendering
```

Parsing and expansion remain separate. Words retain segment structure until
expansion so quoted and unquoted values cannot be confused.

```rust
enum WordPart {
    Literal {
        value: String,
        quote: QuoteMode,
        span: SourceSpan,
    },
    Parameter {
        parameter: Parameter,
        quote: QuoteMode,
        span: SourceSpan,
    },
    CommandSubstitution(CommandList),
}

struct Word {
    parts: Vec<WordPart>,
    span: SourceSpan,
}
```

The initial grammar includes:

- simple commands and assignments;
- single and double quotes plus backslash escaping;
- sequential lists separated by `;` or newline;
- `&&` and `||`;
- pipelines with `|`;
- foreground and background execution with `&`;
- `<`, `>`, `>>`, `2>`, `2>>`, and descriptor duplication;
- parenthesized subshells;
- parameter, tilde, command, and glob expansion in a documented order.

Here documents, arrays, arithmetic expressions, process substitution, and
shell functions can be staged later. Unsupported syntax must produce a
source-spanned diagnostic, never a best-effort reinterpretation.

The dialect specification must define expansion order and exit-status behavior
before script execution is declared stable. `.sh` files are not interpreted as
`ash` scripts; users invoke Bash explicitly when Bash semantics are required.

### 7.1 Current H1 parameter expansion contract

The current expansion stage recognizes exactly `$NAME`, `${NAME}`, and `$?`.
`NAME` is an ASCII shell identifier and the unbraced form consumes the longest
valid name. Braces only delimit a plain name: empty names, positional or other
special parameters, `${NAME:-fallback}`-style operators, and command
substitution fail with source-spanned diagnostics instead of being
reinterpreted. A `$` not followed by a supported or reserved parameter starter
remains literal.

Expansion is quote-aware and runs once for each simple command immediately
before command resolution:

- single-quoted and backslash-escaped dollars remain literal;
- double-quoted parameters expand without field splitting, including one
  preserved empty argument for an empty or undefined value;
- unquoted parameter values split only on the fixed ASCII space, tab, and LF
  separators. Literal and quoted word segments are protected, but separators
  inside an adjacent unquoted expansion still form field boundaries;
- an empty or undefined unquoted expansion contributes no field, so a word made
  only from that expansion disappears. If every word disappears, the command is
  a successful no-op; a quoted empty command name remains an explicit empty-name
  resolution error;
- shell variables take precedence over the exported environment. Environment
  fallback uses the host's name rules, undefined names expand to empty, and
  `IFS` does not configure splitting in this checkpoint;
- `$?` expands to the decimal status of the previously completed command. Since
  state updates after every sequential command, a following command observes a
  native exit, resolution failure, or successful no-op deterministically.

Values and resulting fields remain native `OsString` data. Unix non-UTF-8 bytes
and Windows unpaired native units survive lookup, splitting, concatenation, and
direct argv launch. Only a command name must be UTF-8 because command
classification is textual; an unrepresentable expanded name fails explicitly
with status 127. Tilde, command, arithmetic, and glob expansion remain staged
work and do not run implicitly.

## 8. Persistent shell state

```rust
struct ShellState {
    cwd: PathBuf,
    environment: PlatformEnvironment,
    variables: VariableTable,
    aliases: AliasTable,
    functions: FunctionTable,
    last_status: ShellStatus,
    options: ShellOptions,
    jobs: JobTable,
}
```

Stateful builtins such as `cd`, `export`, `unset`, `alias`, `jobs`, `fg`, and
`bg` run in the parent shell only when they are a standalone foreground
command. In a pipeline, background job, or parenthesized subshell they receive
a cloned state and cannot mutate the parent.

The human shell uses native `PathBuf` and `OsString` values internally so Unix
non-UTF-8 arguments and Windows native strings are not lost. ASH/1 continues to
use its existing UTF-8 logical-path contract. Boundary adapters must reject or
losslessly escape values that cannot be represented in the destination model.

Environment names follow host rules: case-sensitive on Unix and
case-insensitive while preserving spelling on Windows.

## 9. Command resolution

Resolution is deterministic and follows this order:

1. reserved syntax and stateful builtins;
2. aliases and shell functions according to the dialect rules;
3. portable `ash` commands;
4. explicit `native:` or `linux:` backend prefix;
5. native host `PATH` lookup;
6. command-not-found diagnostic.

There is no implicit step from native lookup failure to WSL.

Examples:

```sh
ls -a .                   # portable ash command
grep -n TODO README.md    # portable ash command
cargo test                # native executable from host PATH
native:python app.py      # explicit native backend
linux:make -j8            # explicit WSL backend on Windows
linux:bash -lc 'echo $SHELL' # explicit Linux shell interpretation
```

Backend prefixes are command-position syntax owned by the resolver. They are
not added to the child argument vector.

The selected backend is recorded in job metadata and diagnostics so users can
always tell where a process ran.

## 10. Portable command surface

The first portable command set reuses existing semantics where possible:

| Command | Shared service | Notes |
| --- | --- | --- |
| `pwd`, `cd` | shell state | Native paths; forward slashes accepted on Windows. |
| `echo`, `printf` | shell builtin | Exact documented escaping, not host-shell delegation. |
| `ls` | list service | Familiar common options; not all GNU options. |
| `cat` | read service | Bounded raw bytes now; streaming and text modes later. |
| `grep` | search service | Bounded single-file literal and Rust-regex modes now. |
| `cp`, `mv`, `rm`, `touch` | filesystem service | Preserve transactional conflict behavior. |
| `mkdir`, `rmdir` | future directory service | Requires explicit recovery and capability design. |
| `env`, `export`, `unset` | shell state | Literal single-name `export`/`unset` now; listing later. |

The current executable `ls` contract accepts at most one native path and uses
`.` by default. A directory lists only its immediate children; a file lists
itself; `-d`/`--directory` lists a directory itself. Output is one native name
per line in the list service's collision-free stable order. `-a`/`--all`,
`-d`/`--directory`, `-1`, combined short options, and `--` are accepted.
Unsupported options and multiple paths fail explicitly. Relative paths inherit
the human shell's current directory and ordinary OS authority, not ASH/1
workspace confinement.

The current executable `cat` contract requires exactly one native file path and
accepts `--` for a leading-dash operand. It writes exact file bytes without text
conversion or an added newline. The semantic per-file read ceiling and the
aggregate synchronous shell capture ceiling are both 128 MiB. Options, multiple
files, and the standard-input operand `-` fail explicitly. Generalized
streaming, multi-file concatenation, and text modes remain tied to the H2 stdio
plan rather than pretending that the current buffered executor is streaming.

The current executable `grep` contract requires one pattern and one native
regular-file path. Rust regular expressions are the default; `-E`/
`--extended-regexp` selects that mode explicitly, while `-F`/`--fixed-strings`
selects literal matching. `-i`/`--ignore-case`, `-n`/`--line-number`, combined
short options, and `--` are accepted. Matching UTF-8 lines are emitted in source
order with LF endings; CRLF input is normalized. Each file is limited to 64
MiB, and the rendered result cannot take synchronous shell capture above 128
MiB. No matches return status 1 without a diagnostic. Invalid regular
expressions and argument errors return status 2; missing files, directories,
non-UTF-8 input, and work-limit failures return status 1. Recursive search,
multiple files, filename prefixes, stdin `-`, and streaming remain later work.

The current stateful environment contract accepts exactly one expanded
`NAME=VALUE` argument for `export` and exactly one expanded `NAME` for `unset`.
Both accept `--`. Names are non-empty ASCII identifiers beginning with a letter
or underscore; `export` splits only the first `=`, preserves an empty value, and
updates both shell-variable and exported environment state. `unset` removes
both states and succeeds when the name is absent. Ordinary quote and field rules
apply before the builtin sees its arguments, so values containing separators
must be quoted, for example `export COPY="$SOURCE"`. Neither command emits
output. Listing, export-without-assignment, multiple names, and options remain
unimplemented and fail explicitly where applicable.

Every portable command has a versioned option contract. Common Linux spelling
is preferred, but unsupported GNU flags fail clearly. The project must not
claim GNU compatibility based only on matching command names.

Tools such as `sed`, `awk`, `find`, Git, compilers, and package managers remain
external until a concrete cross-platform semantic service justifies adding a
portable builtin.

## 11. Execution plans

The frontend lowers expanded syntax into a shell-specific plan:

```rust
struct ShellPlan {
    chains: Vec<ConditionalChain>,
}

struct PipelinePlan {
    commands: Vec<CommandPlan>,
    links: Vec<StreamLink>,
    foreground: bool,
}

enum CommandPlan {
    StatefulBuiltin(BuiltinCall),
    PortableBuiltin(PortableCall),
    External(ExternalCommand),
    Subshell(Box<ShellPlan>),
}

struct ExternalCommand {
    backend: ExecutionBackend,
    executable: OsString,
    argv: Vec<OsString>,
    cwd: PathBuf,
    environment: PlatformEnvironment,
    redirections: Vec<Redirection>,
}
```

The current H1 buffered executor lowers a resolved native command directly to
`NativeProcessSpec`. It rebuilds the child environment from `ShellState`, uses
the state's current directory, passes every parsed operand as one native argv
entry, closes child stdin, and never inserts `sh -c`, `cmd /c`, or
`pwsh -Command`. Stdout and stderr are read concurrently under the remaining
128 MiB aggregate synchronous-capture allowance. Exceeding that allowance
terminates the owned process tree and returns status 1 with a process
diagnostic; launch or capture infrastructure failure returns status 126. A
normal nonzero child exit produces no shell diagnostic and becomes the command
status. On Unix, signal termination records the signal and exposes `128 +
signal` as the conventional status. Streaming and inherited-terminal modes
remain part of H2/H4 rather than being simulated by this buffered path.

Redirections remain an ordered vector because these are observably different:

```sh
command >out 2>&1
command 2>&1 >out
```

The executor applies redirections from left to right after pipeline endpoints
are assigned.

## 12. Streaming pipelines

Add a dedicated platform-neutral I/O model:

```rust
enum StdioEndpoint {
    InheritTerminal,
    Null,
    Pipe(PipeId),
    File(FileEndpoint),
    Capture,
    ManagedTerminal(TerminalId),
}
```

Pipeline construction follows this lifecycle:

1. validate the complete plan and all redirection targets;
2. reserve process, descriptor, and job capacity;
3. create every required operating-system pipe;
4. spawn consumers and producers without waiting for predecessor completion;
5. close parent copies of inherited pipe handles immediately;
6. supervise all members as one job;
7. compute pipeline status according to the configured `pipefail` rule;
8. reap every child and release all handles.

Data flows directly through OS pipes. It does not pass through ASON or the
result store, and it is not materialized in memory before the next command
starts. This preserves backpressure and permits unbounded streams within normal
OS and job limits.

Portable in-process builtins in a pipeline run as bounded asynchronous tasks
connected to the same stream abstraction. A stateful builtin in a pipeline uses
subshell state.

Mixed native/WSL pipelines may exchange byte streams, but they do not share a
filesystem namespace or process group. Their job metadata must expose the
boundary. Managed cross-backend interactive pipelines are deferred until they
have platform contract tests.

## 13. Terminal and job control

The first interactive milestone uses inherited terminal handles for foreground
programs. When `ash shell` itself runs in Windows Terminal, a normally spawned
foreground child can inherit that terminal; the shell does not need to create a
nested ConPTY merely to run a basic interactive program.

Managed PTY/ConPTY sessions are a later mode for shell-owned terminal sessions,
background terminal jobs, capture, resize mediation, or remote attachment.

Required job behavior:

- Ctrl+C interrupts the foreground job and does not terminate the shell;
- foreground completion restores terminal ownership to the shell;
- background jobs remain owned by `JobTable` and are not killed by premature
  `ProcessHandle` drop;
- shell exit offers a defined policy for active jobs;
- termination waits for owned descendants and inherited pipes;
- Unix uses process groups and terminal foreground control;
- Windows uses process groups where applicable plus Job Objects for tree
  ownership and console-control behavior tested under Windows Terminal;
- suspend/resume is exposed only on platforms with a defined implementation.

The current machine process API remains kill-on-drop. The interactive job table
holds handles for the entire job lifetime, preserving that safety property.

## 14. Windows and WSL backend

Portable builtins use Windows APIs directly and operate on NTFS paths. WSL is
not required for `ls`, `cat`, `grep`, `cp`, `mv`, or `rm` when those names
resolve to portable commands.

The optional WSL backend exists for actual Linux executables and Linux system
behavior:

```rust
enum ExecutionBackend {
    Native,
    Wsl { distribution: Option<String> },
}
```

Rules:

- `linux:<command>` is the initial explicit selector on Windows;
- absence of WSL produces a typed command-resolution error;
- the selected distribution is explicit configuration or the user's WSL
  default and is recorded in job metadata;
- arguments are passed as an argument vector, not concatenated into a shell
  string;
- Linux shell syntax requires an explicit `linux:bash -lc ...` invocation;
- path conversion is centralized in a tested `WslPathMapper` and must account
  for the selected distribution's automount configuration;
- environment forwarding uses an allowlist and explicit overrides;
- exit status and interruption are normalized without hiding backend details.

For a long-running, fully Linux-oriented session, the preferred topology is to
run the Linux build of `ash shell` inside WSL. Per-command `linux:` dispatch is
a convenience for mixed workflows, not a claim that Windows and Linux process
state are identical.

## 15. Path model

Human mode accepts native absolute paths and relative paths. Forward slashes
are accepted on Windows, for example `D:/code/a3s`, without inventing a fake
Linux root. Portable scripts should prefer relative paths when they need to run
unchanged on every host.

The existing workspace-confined logical path model remains unchanged for
ASH/1. Human interactive mode normally inherits the logged-in user's host
authority, like other shells. An optional `--workspace <root>` mode can confine
portable builtins, but documentation must state that arbitrary external
processes still inherit OS authority unless an external sandbox is used.

Symlinks, reparse points, case sensitivity, executable suffixes, file locking,
and permission differences remain platform contracts. Portable commands
normalize only behavior that can be made truthful on every supported host.

## 16. Safety boundaries

- Machine requests keep current capability negotiation, approval permits,
  budgets, workspace confinement, retention, and canonical error behavior.
- Human mode never weakens those checks by routing interactive input through a
  privileged machine session.
- Human mode uses inherited user authority and labels optional confinement
  accurately.
- Portable destructive commands preserve no-overwrite and conflict checks.
- WSL execution is explicit and can be disabled by policy.
- Command substitution and scripts execute the parsed typed plan; they do not
  construct host-shell strings.
- History persistence must support secret suppression and restrictive local
  permissions.
- Terminal escape sequences from child programs are treated as untrusted
  terminal output; captured rendering requires sanitization policy.

## 17. Error and status model

`ShellStatus` retains backend detail while exposing a conventional numeric
status to the language:

```rust
struct ShellStatus {
    code: i64,
    kind: ShellStatusKind,
    signal: Option<i64>,
    backend: ExecutionBackend,
}
```

Parse, expansion, resolution, spawn, redirection, timeout, interruption, and
native exit are distinct diagnostic categories. Human diagnostics carry source
spans and remediation where known. Machine errors remain stable numeric ASH/1
records.

Pipeline status defaults to the last command. A documented `pipefail` option
returns the rightmost unsuccessful status. Backend-specific native codes remain
available through job inspection even when the language exposes a normalized
status.

## 18. Configuration and startup

The initial entrypoint is:

```text
ash shell [--no-profile | --profile FILE] [-c SOURCE | FILE]
```

Configuration owns only shell concerns: prompt, history, completion, aliases,
portable command options, backend policy, and WSL distribution selection. It
does not duplicate engine governor or machine security configuration.

Startup Profiles are opt-in and use the `ash` dialect. `--profile FILE` selects
one explicitly, while a non-empty native `ASH_PROFILE` value supplies the
configured default. `--no-profile` disables both for deterministic recovery.
Relative Profile paths are anchored to the shell's initial cwd. Profile and
ordinary file/stdin sources share the 1 MiB valid-UTF-8 ceiling. Each Profile is
parsed completely before execution, so a parse failure never applies a prefix.
Non-interactive startup returns status 2 after the source-spanned diagnostic;
interactive startup reports the same diagnostic and opens in safe mode.

Terminal input uses a cross-platform line editor. The default prompt is
`ash> `; `ASH_PROMPT` replaces it and must be valid UTF-8. Ctrl+C at the prompt
sets status 130 and presents another prompt, while EOF returns the previous
status. `exit [STATUS]` stops the remaining submitted source and the REPL;
an omitted status reuses `$?`, explicit values are limited to 0 through 255,
and invalid arguments return status 2 without exiting.

Persistent history is configured after the Profile, but relative
`ASH_HISTORY` paths remain anchored to the initial cwd. An empty
`ASH_HISTORY` disables persistence. Without it, Unix-like hosts use
`$XDG_STATE_HOME/ash/history` or `$HOME/.local/state/ash/history`, and Windows
uses `%LOCALAPPDATA%\ash\history`. Lines beginning with an ASCII space or tab
are omitted for sensitive-command suppression. A history target must be a
regular non-symbolic-link file; Unix files are forced to mode `0600`. An
unavailable or unsafe target disables only persistent history, emits a warning,
and leaves the line editor and in-memory session usable.

## 19. Verification strategy

### 19.1 Parser and expansion

- golden AST fixtures with exact source spans;
- property tests for tokenize/parse/format stability where formatting exists;
- fuzzing for malformed quotes, nesting, substitutions, and redirections;
- exact parameter AST spans plus explicit expansion-order, native-value, empty,
  field-boundary, quoting, and previous-status fixtures;
- rejection tests for unsupported Bash syntax.

### 19.2 Portable commands

- one semantic contract suite on Linux, macOS, and Windows;
- paths containing spaces, Unicode, non-UTF-8 Unix bytes, reserved names,
  symlinks, and Windows reparse points;
- option-contract fixtures independent of host utility versions;
- conflict and rollback tests for interactive mutations;
- exact-byte tests for binary `cat` and pipeline transport.

### 19.3 Pipelines and jobs

- producer/consumer concurrency and backpressure;
- stdout/stderr pressure beyond the current capture memory ceiling;
- ordered redirection cases including descriptor duplication;
- early consumer exit and broken-pipe behavior;
- Ctrl+C during foreground pipelines;
- background job completion, shell exit, and descendant cleanup;
- repeated handle and descriptor leak checks;
- terminal resize and managed PTY/ConPTY tests when that mode lands.

### 19.4 WSL

- opt-in Windows CI on a runner with a known WSL distribution;
- direct operation on a Windows-mounted fixture;
- spaces, Unicode, path conversion, environment, exit status, and Ctrl+C;
- clear failure when WSL or the selected distribution is unavailable;
- proof that an unresolved native command never falls back implicitly.

### 19.5 Regression gates

- all current ASH/1 request and response fixtures remain byte-identical;
- existing process capture, cancellation, and tree-empty tests continue to
  pass on Linux, macOS, and Windows;
- `ash run` and `ash rpc` cold and warm paths do not initialize REPL or terminal
  dependencies;
- shell-only dependencies do not enter minimal machine builds unless the
  distribution explicitly includes `ash shell`.

## 20. Delivery sequence

### H0: contract and crate boundary

- accept the shell dialect scope and this architecture decision;
- add `a3s-ash-shell` with parser, AST, diagnostics, and `ShellState`;
- refactor reusable semantic services without changing ASH/1 output;
- lock initial parser and command-resolution fixtures.

Current checkpoint: every H0 item is implemented. Semantic paths use
provider-owned `PathBuf` values plus collision-free stable ordering keys, and
byte-exact regression fixtures prove that the ASH/1 read/list/search responses
remain unchanged. H1 began with the non-interactive CLI route and portable
builtin adapters, then added the terminal lifecycle without changing ASH/1.

### H1: native shell foundation

- implement `ash shell`, prompt, history, simple commands, `cd`, environment,
  status, and native executable resolution;
- add portable `pwd`, `echo`, `ls`, `cat`, and `grep`;
- support script files without pipelines or background jobs;
- verify Linux, macOS, and Windows behavior.

Current checkpoint: `ash shell` opens a cross-platform line-edited terminal
loop with the prompt, persistent-history, interruption, EOF, and `exit`
contracts in section 18. The same entrypoint accepts `-c SOURCE`, bounded stdin,
or one native script path. Opt-in `--profile FILE`/`ASH_PROFILE` startup uses
the same dialect and input ceiling, while `--no-profile` remains the recovery
route. Every source parses the H0 subset and executes sequential `pwd`, `echo`,
`cd`, expanded `export`/`unset`, portable `ls`, bounded raw-byte `cat`, bounded
text `grep`, and native host executables against one native `ShellState`, with
source-spanned human diagnostics and conventional status propagation. Named
`$NAME`/`${NAME}` parameters and `$?` expand in a distinct, quote-aware stage
with native-string preservation and the fixed field contract in section 7.1.
File, stdin, and Profile sources share a 1 MiB valid-UTF-8 ceiling, while file
and external-command operands retain native representation. `ls`, `cat`, and
`grep` reuse the provider-neutral list/read/search services with an unconfined
native provider and the option contracts in section 10. Native programs resolve
from the persistent environment, launch through the owned `ash-platform`
process boundary with exact argv/cwd/environment, and still use the bounded
dual-stream contract in section 11. Their stdin remains closed, so foreground
terminal programs and Ctrl+C job-tree delivery remain H4 rather than being
claimed by this REPL checkpoint. The `human-shell` feature is enabled for the
normal binary and can be disabled for a minimal machine-only build. Broader
expansion, streaming stdio, mutations, pipelines, job control, and WSL launch
remain for later increments.

### H2: streaming execution

- add generalized stdio endpoints;
- implement OS pipes and ordered redirections;
- supervise multi-process foreground pipelines;
- add `pipefail` and exact status behavior.

### H3: mutations and command language

- add portable `cp`, `mv`, `rm`, and `touch` adapters;
- add conditional lists, command substitution, globbing, aliases, functions,
  and subshell state;
- complete the first stable `ash` dialect specification.

### H4: interactive jobs

- implement inherited-terminal foreground execution;
- add Ctrl+C behavior, background jobs, `jobs`, `fg`, and `bg`;
- add managed PTY/ConPTY only for requirements not met by terminal inheritance.

### H5: explicit Linux backend on Windows

- add WSL detection, backend policy, path mapping, and direct argv launch;
- implement `linux:` resolution and diagnostics;
- add opt-in WSL integration tests;
- document running the Linux `ash shell` build inside WSL for full sessions.

## 21. Completion criteria

The architecture is fully implemented only when all of the following are true:

- `ash shell` can replace the default interactive profile in Windows Terminal;
- `cd` and environment changes persist across commands;
- portable `ls`, `cat`, and `grep` operate directly on Windows files without
  WSL;
- native external programs receive an exact argument vector;
- an eight-megabyte producer/consumer pipeline completes without materializing
  the entire stream or deadlocking;
- ordered stdout/stderr redirections match the dialect specification;
- foreground interactive programs work through inherited terminal handles;
- Ctrl+C terminates the foreground job tree but leaves the shell alive;
- unresolved native commands do not invoke WSL;
- explicit `linux:` commands can operate on a Windows-mounted fixture when WSL
  is configured;
- all existing ASH/1 canonical fixtures and cross-platform tests still pass.
