use std::collections::{BTreeMap, BTreeSet};
use std::env;

use async_trait::async_trait;

use crate::config::{ProjectConfig, SourceConfig};
use crate::error::{RemnantError, Result};
use crate::model::{SourceDescription, SourceSnapshot, StateObject};

pub mod postgres;
pub mod redis;

#[async_trait]
pub trait StateSource: Send + Sync {
    fn name(&self) -> &str;
    async fn describe(&self) -> Result<SourceDescription>;
    async fn enumerate_state(&self) -> Result<Vec<StateObject>>;
    async fn snapshot(&self) -> Result<SourceSnapshot>;
    fn objects_from_snapshot(&self, snapshot: &SourceSnapshot) -> Result<Vec<StateObject>>;
    async fn restore(
        &self,
        snapshot: &SourceSnapshot,
        retained: Option<&BTreeSet<String>>,
    ) -> Result<()>;

    /// Check that the source now contains exactly the state selected from a
    /// snapshot. Adapters may override this when their live representation has
    /// expected volatility (for example, Redis key TTLs).
    async fn verify_restored(
        &self,
        snapshot: &SourceSnapshot,
        retained: Option<&BTreeSet<String>>,
    ) -> Result<()> {
        let expected = object_fingerprints(self.objects_from_snapshot(snapshot)?, retained);
        let actual_snapshot = self.snapshot().await?;
        let actual = object_fingerprints(self.objects_from_snapshot(&actual_snapshot)?, None);
        if actual == expected {
            return Ok(());
        }

        Err(RemnantError::Adapter(format!(
            "{} restore verification failed: expected {} objects, found {}",
            self.name(),
            expected.len(),
            actual.len()
        )))
    }
}

fn object_fingerprints(
    objects: Vec<StateObject>,
    retained: Option<&BTreeSet<String>>,
) -> BTreeMap<String, String> {
    objects
        .into_iter()
        .filter(|object| retained.is_none_or(|ids| ids.contains(&object.id)))
        .map(|object| (object.id, object.fingerprint))
        .collect()
}

pub fn from_config(config: &ProjectConfig) -> Result<Vec<Box<dyn StateSource>>> {
    config
        .sources
        .iter()
        .map(|(name, source)| match source {
            SourceConfig::Postgres { url_env, schema } => {
                let url = env::var(url_env).map_err(|_| {
                    crate::error::RemnantError::InvalidConfig(format!(
                        "environment variable {url_env} for source {name} is not set"
                    ))
                })?;
                Ok(Box::new(postgres::PostgresAdapter::new(
                    name.clone(),
                    url,
                    schema.clone(),
                )) as Box<dyn StateSource>)
            }
            SourceConfig::Redis { url_env, database } => {
                let url = env::var(url_env).map_err(|_| {
                    crate::error::RemnantError::InvalidConfig(format!(
                        "environment variable {url_env} for source {name} is not set"
                    ))
                })?;
                Ok(
                    Box::new(redis::RedisAdapter::new(name.clone(), url, *database))
                        as Box<dyn StateSource>,
                )
            }
        })
        .collect()
}
