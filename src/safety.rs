use crate::config::ProjectConfig;
use crate::error::{RemnantError, Result};

pub fn ensure_mutation_allowed(config: &ProjectConfig, explicit_override: bool) -> Result<()> {
    if config.safety.allow_non_local || explicit_override {
        return Ok(());
    }
    let mut suspicious = Vec::new();
    for (name, source) in &config.sources {
        let url = std::env::var(source.url_env()).map_err(|_| {
            RemnantError::InvalidConfig(format!(
                "environment variable {} for source {name} is not set",
                source.url_env()
            ))
        })?;
        if !is_local_target(&url) {
            suspicious.push(format!("{name} ({})", source.kind()));
        }
    }
    if suspicious.is_empty() {
        Ok(())
    } else {
        Err(RemnantError::UnsafeOperation(format!(
            "refusing to modify suspicious non-local targets: {}; set safety.allow_non_local=true or pass --allow-non-local only after verifying isolation",
            suspicious.join(", ")
        )))
    }
}

pub fn is_local_target(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("unix://") || lower.contains("host=/") {
        return true;
    }
    let host = lower
        .split("//")
        .nth(1)
        .unwrap_or(&lower)
        .split('@')
        .next_back()
        .unwrap_or(&lower)
        .split(['/', '?'])
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    matches!(
        host,
        "localhost" | "127.0.0.1" | "::1" | "postgres" | "redis" | "db" | "database" | "cache"
    ) || (!host.contains('.')
        && !host.contains("prod")
        && !host.contains("rds")
        && !host.contains("amazonaws"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_local_and_docker_service_targets() {
        assert!(is_local_target("postgres://localhost:5432/app"));
        assert!(is_local_target("redis://redis:6379/0"));
        assert!(is_local_target("postgres://postgres:5432/app"));
    }

    #[test]
    fn rejects_hostnames_that_look_remote() {
        assert!(!is_local_target("postgres://prod-db.example.com:5432/app"));
        assert!(!is_local_target("redis://cache.amazonaws.com:6379"));
    }
}
