use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::timeout;

use crate::config::OracleConfig;
use crate::error::{RemnantError, Result};

/// The outcome of an oracle invocation. Only `FailureReproduced` is a
/// positive reduction result; timeouts and process errors are never silently
/// treated as a reproduced failure.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OracleOutcome {
    FailureReproduced,
    FailureAbsent,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OracleResult {
    pub command: String,
    pub outcome: OracleOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub started_at: DateTime<Utc>,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait OracleRunner: Send + Sync {
    async fn run(&self) -> Result<OracleResult>;
}

impl OracleResult {
    pub fn failure_reproduced(&self) -> bool {
        self.outcome == OracleOutcome::FailureReproduced
    }
}

#[derive(Debug, Clone)]
pub struct Oracle {
    config: OracleConfig,
}

impl Oracle {
    pub fn new(config: OracleConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &OracleConfig {
        &self.config
    }

    pub async fn run(&self) -> Result<OracleResult> {
        let started_at = Utc::now();
        let started = Instant::now();
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&self.config.command)
            .kill_on_drop(true);

        let output = match timeout(self.config.timeout_duration()?, command.output()).await {
            Ok(output) => output.map_err(RemnantError::OracleProcess)?,
            Err(_) => {
                return Ok(OracleResult {
                    command: self.config.command.clone(),
                    outcome: OracleOutcome::TimedOut,
                    exit_code: None,
                    duration_ms: started.elapsed().as_millis(),
                    started_at,
                    stdout: String::new(),
                    stderr: format!("oracle timed out after {}", self.config.timeout),
                });
            }
        };
        let exit_code = output.status.code();
        let outcome = if exit_code == Some(self.config.failure_exit_code) {
            OracleOutcome::FailureReproduced
        } else {
            OracleOutcome::FailureAbsent
        };

        Ok(OracleResult {
            command: self.config.command.clone(),
            outcome,
            exit_code,
            duration_ms: started.elapsed().as_millis(),
            started_at,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    pub fn timeout(&self) -> Duration {
        self.config
            .timeout_duration()
            .unwrap_or(Duration::from_secs(30))
    }
}

#[async_trait]
impl OracleRunner for Oracle {
    async fn run(&self) -> Result<OracleResult> {
        Oracle::run(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(command: &str, timeout: &str) -> OracleConfig {
        OracleConfig {
            command: command.to_string(),
            timeout: timeout.to_string(),
            failure_exit_code: 1,
        }
    }

    #[tokio::test]
    async fn recognizes_configured_failure_exit_code() {
        let result = Oracle::new(config("printf 'boom'; exit 1", "1s"))
            .run()
            .await
            .expect("oracle runs");
        assert_eq!(result.outcome, OracleOutcome::FailureReproduced);
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.stdout, "boom");
    }

    #[tokio::test]
    async fn distinguishes_a_passing_reproducer() {
        let result = Oracle::new(config("printf 'ok'; exit 0", "1s"))
            .run()
            .await
            .expect("oracle runs");
        assert_eq!(result.outcome, OracleOutcome::FailureAbsent);
        assert!(!result.failure_reproduced());
    }

    #[tokio::test]
    async fn returns_timeout_instead_of_hanging() {
        let result = Oracle::new(config("sleep 1", "10ms"))
            .run()
            .await
            .expect("timeout is a result");
        assert_eq!(result.outcome, OracleOutcome::TimedOut);
        assert!(result.duration_ms < 500);
    }
}
