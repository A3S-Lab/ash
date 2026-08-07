# Portable Human Shell Architecture

- Status: accepted; H1 in progress
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
deadline, budget, projection, retention, and ASON ownership. H1 has started with
a feature-gated `ash shell -c SOURCE` and bounded-stdin route plus sequential
`pwd`, `echo`, and `cd` execution. Interactive input, external execution, the
remaining portable commands, expansion, pipelines, jobs, and WSL launch remain
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

Keep the current machine-oriented `ProcessSpec` behavior stable. Introduce a
more general launch API underneath it with explicit standard-I/O and terminal
attachment modes. The existing `ProcessSpec` becomes a strict adapter selecting
null or captured pipes exactly as it does today.

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
    Literal(String),
    Parameter(String),
    CommandSubstitution(CommandList),
}

struct Word {
    parts: Vec<WordPart>,
    quote: QuoteMode,
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
ls -la .                  # portable ash command
grep -R TODO crates       # portable ash command
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
| `cat` | streaming read service | Raw bytes to stdout unless a text option is selected. |
| `grep` | search service | Literal and regular-expression modes. |
| `cp`, `mv`, `rm`, `touch` | filesystem service | Preserve transactional conflict behavior. |
| `mkdir`, `rmdir` | future directory service | Requires explicit recovery and capability design. |
| `env`, `export`, `unset` | shell state | Host-aware environment names. |

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
ash shell
```

Configuration owns only shell concerns: prompt, history, completion, aliases,
portable command options, backend policy, and WSL distribution selection. It
does not duplicate engine governor or machine security configuration.

Startup files are opt-in and use the `ash` dialect. The shell starts with a
`--no-profile` option for deterministic recovery. A malformed startup file
reports a source-spanned error and continues in a safe interactive mode rather
than executing a partial remainder silently.

## 19. Verification strategy

### 19.1 Parser and expansion

- golden AST fixtures with exact source spans;
- property tests for tokenize/parse/format stability where formatting exists;
- fuzzing for malformed quotes, nesting, substitutions, and redirections;
- explicit expansion-order and quoting fixtures;
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
remain unchanged. H1 begins with the non-interactive CLI route and portable
builtin adapters.

### H1: native non-interactive shell

- implement `ash shell`, prompt, history, simple commands, `cd`, environment,
  status, and native executable resolution;
- add portable `pwd`, `echo`, `ls`, `cat`, and `grep`;
- support script files without pipelines or background jobs;
- verify Linux, macOS, and Windows behavior.

Current checkpoint: `ash shell -c SOURCE` and `ash shell < script.ash` parse the
H0 syntax subset, execute sequential `pwd`, `echo`, and `cd` commands against
one native `ShellState`, emit source-spanned human diagnostics, and return the
last command status. The `human-shell` feature is enabled for the normal binary
and can be disabled for a minimal machine-only build. Interactive input,
profiles, native process launch, script-file arguments, and `ls`/`cat`/`grep`
remain for later H1 increments.

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
