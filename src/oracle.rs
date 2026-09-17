use std::io;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::config::OracleConfig;
use crate::error::{RemnantError, Result};

/// The outcome of an oracle invocation. Only `FailureReproduced` is a
/// positive reduction result. Timeouts, unexpected exit codes, and process
/// errors never silently count as reproduced failures.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OracleOutcome {
    FailureReproduced,
    FailureAbsent,
    TimedOut,
    UnexpectedExit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OracleResult {
    /// A safe command summary, never the raw configured command text.
    pub command: String,
    pub outcome: OracleOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub started_at: DateTime<Utc>,
    pub stdout: String,
    pub stderr: String,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
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
        let mut command = self.command();
        command
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(RemnantError::OracleStart)?;
        let process_id = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| oracle_process_error("could not capture oracle stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| oracle_process_error("could not capture oracle stderr"))?;
        let mut stdout_task = tokio::spawn(read_capped(stdout, self.config.max_output_bytes));
        let mut stderr_task = tokio::spawn(read_capped(stderr, self.config.max_output_bytes));

        let completed = async {
            let status = child.wait().await.map_err(RemnantError::OracleProcess)?;
            let stdout = collect_output(&mut stdout_task).await?;
            let stderr = collect_output(&mut stderr_task).await?;
            Ok::<_, RemnantError>((status, stdout, stderr))
        };
        let (status, stdout, stderr) =
            match timeout(self.config.timeout_duration()?, completed).await {
                Ok(completed) => completed?,
                Err(_) => {
                    terminate(&mut child, process_id).await?;
                    let stdout = collect_output(&mut stdout_task).await?;
                    let stderr = collect_output(&mut stderr_task).await?;
                    return Ok(OracleResult {
                        command: self.config.display_command(),
                        outcome: OracleOutcome::TimedOut,
                        exit_code: None,
                        duration_ms: started.elapsed().as_millis(),
                        started_at,
                        stdout: stdout.text,
                        stderr: stderr.text,
                        stdout_truncated: stdout.truncated,
                        stderr_truncated: stderr.truncated,
                    });
                }
            };
        let exit_code = status.code();
        let outcome = match exit_code {
            Some(code) if code == self.config.failure_exit_code => OracleOutcome::FailureReproduced,
            Some(0) => OracleOutcome::FailureAbsent,
            _ => OracleOutcome::UnexpectedExit,
        };

        Ok(OracleResult {
            command: self.config.display_command(),
            outcome,
            exit_code,
            duration_ms: started.elapsed().as_millis(),
            started_at,
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        })
    }

    fn command(&self) -> Command {
        if self.config.args.is_empty() {
            let mut command = Command::new("sh");
            command.arg("-c").arg(&self.config.command);
            command
        } else {
            let mut command = Command::new(&self.config.command);
            command.args(&self.config.args);
            command
        }
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

struct CapturedOutput {
    text: String,
    truncated: bool,
}

async fn read_capped<R>(mut reader: R, limit: usize) -> io::Result<CapturedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(captured.len());
        let keep = remaining.min(read);
        captured.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(CapturedOutput {
        text: redact_sensitive_output(&String::from_utf8_lossy(&captured)),
        truncated,
    })
}

fn redact_sensitive_output(value: &str) -> String {
    let mut redacted = redact_url_userinfo(value);
    for key in [
        "password",
        "passwd",
        "token",
        "secret",
        "api_key",
        "apikey",
        "authorization",
    ] {
        redacted = redact_assignment(&redacted, key);
    }
    redacted
}

fn redact_url_userinfo(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(scheme) = remaining.find("://") {
        let credentials_start = scheme + 3;
        let authority_end = remaining[credentials_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '/' | '?' | '#')
            })
            .map_or(remaining.len(), |offset| credentials_start + offset);
        let authority = &remaining[credentials_start..authority_end];
        if let Some(at) = authority.rfind('@') {
            redacted.push_str(&remaining[..credentials_start]);
            redacted.push_str("***@");
            remaining = &remaining[credentials_start + at + 1..];
        } else {
            let next = authority_end.max(credentials_start);
            redacted.push_str(&remaining[..next]);
            remaining = &remaining[next..];
        }
    }
    redacted.push_str(remaining);
    redacted
}

fn redact_assignment(value: &str, key: &str) -> String {
    let mut redacted = String::with_capacity(value.len());
    let mut remaining = value;
    let lowercase_key = key.to_ascii_lowercase();
    loop {
        let lowercase = remaining.to_ascii_lowercase();
        let Some(key_start) = lowercase.find(&lowercase_key) else {
            redacted.push_str(remaining);
            return redacted;
        };
        let separator_start = key_start + key.len();
        let separator = remaining[separator_start..].chars().next();
        if !matches!(separator, Some('=' | ':')) {
            let next = separator_start;
            redacted.push_str(&remaining[..next]);
            remaining = &remaining[next..];
            continue;
        }
        let value_start = separator_start + separator.expect("matched separator").len_utf8();
        let value_end = remaining[value_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '&' | ',' | ';')
            })
            .map_or(remaining.len(), |offset| value_start + offset);
        redacted.push_str(&remaining[..value_start]);
        redacted.push_str("***");
        remaining = &remaining[value_end..];
    }
}

async fn collect_output(
    task: &mut JoinHandle<io::Result<CapturedOutput>>,
) -> Result<CapturedOutput> {
    task.await
        .map_err(|error| oracle_process_error(format!("oracle output task failed: {error}")))?
        .map_err(RemnantError::OracleProcess)
}

async fn terminate(child: &mut tokio::process::Child, process_id: Option<u32>) -> Result<()> {
    let exited = child
        .try_wait()
        .map_err(RemnantError::OracleProcess)?
        .is_some();
    #[cfg(unix)]
    if let Some(process_id) = process_id {
        let process_id = i32::try_from(process_id)
            .map_err(|_| oracle_process_error("oracle process id exceeded platform limits"))?;
        // The child owns a dedicated process group, so this also terminates
        // shell-spawned descendants that would survive killing only the shell.
        let result = unsafe { libc::kill(-process_id, libc::SIGKILL) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(RemnantError::OracleProcess(error));
            }
        }
    }
    #[cfg(not(unix))]
    if !exited {
        child.kill().await.map_err(RemnantError::OracleProcess)?;
    }

    if !exited {
        child.wait().await.map_err(RemnantError::OracleProcess)?;
    }
    Ok(())
}

fn oracle_process_error(message: impl Into<String>) -> RemnantError {
    RemnantError::OracleProcess(io::Error::other(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(command: &str, timeout: &str) -> OracleConfig {
        OracleConfig {
            command: command.to_string(),
            args: Vec::new(),
            timeout: timeout.to_string(),
            failure_exit_code: 1,
            max_output_bytes: 64 * 1024,
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
        assert_eq!(result.command, "shell command");
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
    async fn classifies_unexpected_exit_codes_without_accepting_them() {
        let result = Oracle::new(config("exit 17", "1s"))
            .run()
            .await
            .expect("oracle runs");
        assert_eq!(result.outcome, OracleOutcome::UnexpectedExit);
        assert_eq!(result.exit_code, Some(17));
    }

    #[tokio::test]
    async fn runs_structured_commands_without_shell_interpretation() {
        let mut config = config("printf", "1s");
        config.args = vec!["%s".to_string(), "literal; text".to_string()];
        let result = Oracle::new(config).run().await.expect("oracle runs");
        assert_eq!(result.outcome, OracleOutcome::FailureAbsent);
        assert_eq!(result.stdout, "literal; text");
        assert_eq!(result.command, "printf (2 arguments)");
    }

    #[tokio::test]
    async fn bounds_captured_output() {
        let mut config = config("head -c 32 /dev/zero | tr '\\0' x", "1s");
        config.max_output_bytes = 8;
        let result = Oracle::new(config).run().await.expect("oracle runs");
        assert_eq!(result.stdout.len(), 8);
        assert!(result.stdout_truncated);
    }

    #[test]
    fn redacts_common_credentials_from_captured_output() {
        let output = redact_sensitive_output(
            "DATABASE_URL=postgres://user:password@example.test/app token=hunter2",
        );
        assert_eq!(
            output,
            "DATABASE_URL=postgres://***@example.test/app token=***"
        );
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

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_terminates_shell_descendants() {
        let result = Oracle::new(config("sleep 10 & child=$!; echo $child", "50ms"))
            .run()
            .await
            .expect("timeout is a result");
        let process_id = result
            .stdout
            .trim()
            .parse::<i32>()
            .expect("child pid is printed");

        for _ in 0..20 {
            let exists = unsafe { libc::kill(process_id, 0) == 0 };
            if !exists {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("oracle descendant {process_id} survived timeout");
    }
}
