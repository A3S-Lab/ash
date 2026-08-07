#![forbid(unsafe_code)]

//! Portable human-shell syntax, state, and command resolution.
//!
//! This crate is deliberately independent from the ASH/1 machine protocol.
//! It establishes the human frontend boundary without adding a free-form
//! shell-string operation to the machine interface.

mod diagnostic;
mod execution;
mod parser;
mod resolver;
mod state;
mod syntax;

pub use diagnostic::{Diagnostic, DiagnosticCode};
pub use execution::{
    ExecutionDiagnostic, ExecutionDiagnosticCode, ShellExecution, execute_source,
    execute_source_with,
};
pub use parser::parse;
pub use resolver::{
    CommandResolver, HostPlatform, NativeCommandLookup, PathCommandLookup, PortableCommand,
    ResolutionError, ResolvedCommand, StatefulBuiltin,
};
pub use state::{
    ExecutionBackend, JobState, JobSummary, JobTable, PlatformEnvironment, ShellFunction,
    ShellOptions, ShellState, ShellStatus, ShellStatusKind, StateError,
};
pub use syntax::{QuoteMode, Script, SimpleCommand, SourceSpan, Word, WordPart};
