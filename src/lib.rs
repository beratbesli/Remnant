//! Remnant is a black-box, cross-service persistent-state reducer.
//!
//! The library contains the deterministic core used by both the human CLI and
//! the machine-facing JSON/MCP interface. Adapters are deliberately kept
//! behind traits so that state operations are never coupled to the reducer.

pub mod config;
pub mod error;
pub mod oracle;

pub use config::{ProjectConfig, SourceConfig};
pub use error::{RemnantError, Result};
pub use oracle::{Oracle, OracleOutcome, OracleResult};
