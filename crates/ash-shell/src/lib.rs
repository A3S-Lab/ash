#![forbid(unsafe_code)]

//! Portable human-shell syntax, native-string expansion, state, execution, and interaction.
//!
//! This crate does not expose the ASH/1 machine protocol. It establishes the
//! human frontend boundary without adding a free-form shell-string operation to
//! the machine interface, while portable builtins may reuse protocol-neutral
//! semantic services.

mod diagnostic;
mod execution;
mod expand;
mod glob;
mod interactive;
mod parser;
mod resolver;
mod state;
mod syntax;

pub use diagnostic::{Diagnostic, DiagnosticCode};
pub use execution::{
    ExecutionDiagnostic, ExecutionDiagnosticCode, MAX_NATIVE_PIPELINE_STAGES, ShellExecution,
    execute_source, execute_source_with,
};
pub use glob::{
    MAX_PATHNAME_EXPANSION_ENTRIES, MAX_PATHNAME_EXPANSION_MATCHES, MAX_PATHNAME_PATTERN_UNITS,
};
pub use interactive::{
    DEFAULT_INTERACTIVE_PROMPT, InteractiveConfig, InteractiveEditor, InteractiveError,
    InteractiveEvent,
};
pub use parser::{MAX_COMMAND_SUBSTITUTION_DEPTH, parse};
pub use resolver::{
    CommandResolver, HostPlatform, NativeCommandLookup, PathCommandLookup, PortableCommand,
    ResolutionError, ResolvedCommand, StatefulBuiltin,
};
pub use state::{
    ExecutionBackend, JobState, JobSummary, JobTable, PlatformEnvironment, ShellFunction,
    ShellOptions, ShellState, ShellStatus, ShellStatusKind, StateError,
};
pub use syntax::{
    CommandSubstitution, ConditionalOperator, Parameter, Pipeline, PipelineCondition, QuoteMode,
    Redirection, RedirectionDescriptor, RedirectionFileMode, RedirectionTarget, Script,
    SimpleCommand, SourceSpan, Word, WordPart,
};
