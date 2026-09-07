use std::collections::BTreeSet;
use std::env;

use async_trait::async_trait;

use crate::config::{ProjectConfig, SourceConfig};
use crate::error::Result;
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
