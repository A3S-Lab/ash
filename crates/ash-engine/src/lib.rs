#![forbid(unsafe_code)]

//! Deterministic scheduling primitives for the ash execution engine.

mod budget;
mod cancellation;
mod dag;
mod governor;
mod parallel;
mod runtime;

pub use budget::{BudgetError, BudgetRemaining, BudgetTracker};
pub use cancellation::CancellationToken;
pub use dag::{DagCompletion, DagError, DagNode, DagOutcome, execute_dag};
pub use governor::{
    Governor, GovernorError, GovernorLimits, HierarchicalGovernor, HierarchicalPermit, PermitKind,
};
pub use parallel::{ComputePool, Parallelism, ParallelismError};
pub use runtime::{Engine, EngineError, Program, RegisteredProgram, Session, SessionConfig};
