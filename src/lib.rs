//! Remnant is a black-box, cross-service persistent-state reducer.
//!
//! The library contains the deterministic core used by both the human CLI and
//! the machine-facing JSON/MCP interface. Adapters are deliberately kept
//! behind traits so that state operations are never coupled to the reducer.

pub mod adapters;
pub mod config;
pub mod error;
pub mod model;
pub mod oracle;
pub mod persistence;
pub mod reducer;
pub mod snapshot;

pub use config::{ProjectConfig, SourceConfig};
pub use error::{RemnantError, Result};
pub use model::{
    CandidateSet, Snapshot, SourceDescription, SourceSnapshot, StateGroup, StateObject,
};
pub use oracle::{Oracle, OracleOutcome, OracleResult};
