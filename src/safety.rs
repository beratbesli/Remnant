use std::collections::BTreeMap;
use std::net::IpAddr;

use serde_json::{Value, json};
use url::Url;

use crate::adapters::StateSource;
use crate::config::ProjectConfig;
use crate::error::{RemnantError, Result};

pub async fn ensure_mutation_allowed(
    config: &ProjectConfig,
    sources: &[Box<dyn StateSource>],
    explicit_override: bool,
    supplied_fingerprint: Option<&str>,
) -> Result<()> {
    let mut suspicious = Vec::new();
    if !config.safety.allow_non_local && !explicit_override {
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
    }
    if !suspicious.is_empty() {
        return Err(RemnantError::UnsafeOperation(format!(
            "refusing to modify suspicious non-local targets: {}; set safety.allow_non_local=true or pass --allow-non-local only after verifying isolation",
            suspicious.join(", ")
        )));
    }
    if config.safety.require_fingerprint {
        let observed = target_fingerprint(sources).await?;
        check_target_fingerprint(&observed, supplied_fingerprint)?;
    }
    Ok(())
}

pub async fn target_fingerprint(sources: &[Box<dyn StateSource>]) -> Result<String> {
    let mut identities: BTreeMap<String, Value> = BTreeMap::new();
    for source in sources {
        identities.insert(source.name().to_string(), source.target_identity().await?);
    }
    Ok(crate::model::fingerprint(&json!(identities)))
}

fn check_target_fingerprint(observed: &str, supplied: Option<&str>) -> Result<()> {
    if supplied == Some(observed) {
        Ok(())
    } else {
        Err(RemnantError::UnsafeOperation(format!(
            "target fingerprint confirmation required or mismatched; connected target fingerprint is {observed}; verify the target and pass --target-fingerprint {observed} (MCP: target_fingerprint)"
        )))
    }
}

pub fn is_local_target(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    if url.scheme() == "unix" {
        return true;
    }
    if url
        .query_pairs()
        .any(|(key, value)| key == "host" && value.starts_with('/'))
    {
        return true;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
        || matches!(
            host.to_ascii_lowercase().as_str(),
            "postgres" | "redis" | "db" | "database" | "cache"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_local_and_docker_service_targets() {
        assert!(is_local_target("postgres://localhost:5432/app"));
        assert!(is_local_target("postgres://[::1]:5432/app"));
        assert!(is_local_target("redis://redis:6379/0"));
        assert!(is_local_target("postgres://postgres:5432/app"));
    }

    #[test]
    fn rejects_hostnames_that_look_remote() {
        assert!(!is_local_target("postgres://prod-db.example.com:5432/app"));
        assert!(!is_local_target("redis://cache.amazonaws.com:6379"));
        assert!(!is_local_target("postgres://staging:5432/app"));
        assert!(!is_local_target("postgres://production:5432/app"));
        assert!(!is_local_target("postgres://user:pass@db.internal/app"));
        assert!(!is_local_target(
            "postgres://db.internal/app?note=host=/tmp"
        ));
    }

    #[test]
    fn fingerprint_confirmation_requires_an_exact_match() {
        assert!(check_target_fingerprint("abc", Some("abc")).is_ok());
        assert!(check_target_fingerprint("abc", None).is_err());
        assert!(check_target_fingerprint("abc", Some("def")).is_err());
    }
}
