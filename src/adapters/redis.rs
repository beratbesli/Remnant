use std::collections::{BTreeSet, HashMap};

use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::StateSource;
use crate::error::{RemnantError, Result};
use crate::model::{SourceDescription, SourceSnapshot, StateObject, digest_bytes};

#[derive(Debug, Clone)]
pub struct RedisAdapter {
    name: String,
    url: String,
    database: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedisSnapshot {
    pub database: u8,
    pub entries: Vec<RedisEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedisEntry {
    pub key: String,
    pub kind: String,
    pub ttl_ms: i64,
    pub payload_base64: String,
}

impl RedisAdapter {
    pub fn new(name: impl Into<String>, url: impl Into<String>, database: u8) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            database,
        }
    }

    async fn connection(&self) -> Result<redis::aio::MultiplexedConnection> {
        let client = redis::Client::open(self.url.as_str())
            .map_err(|error| RemnantError::Adapter(format!("redis {}: {error}", self.name)))?;
        let mut connection = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|error| {
                RemnantError::Adapter(format!("redis connect {}: {error}", self.name))
            })?;
        redis::cmd("SELECT")
            .arg(self.database)
            .query_async::<()>(&mut connection)
            .await
            .map_err(|error| {
                RemnantError::Adapter(format!("redis select db {}: {error}", self.database))
            })?;
        Ok(connection)
    }

    async fn capture_entries(&self) -> Result<Vec<RedisEntry>> {
        let mut connection = self.connection().await?;
        let mut cursor = 0u64;
        let mut entries = Vec::new();
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut connection)
                .await
                .map_err(|error| RemnantError::Adapter(format!("redis scan: {error}")))?;
            for key in keys {
                let kind: String = redis::cmd("TYPE")
                    .arg(&key)
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| RemnantError::Adapter(format!("redis type {key}: {error}")))?;
                let ttl_ms: i64 = redis::cmd("PTTL")
                    .arg(&key)
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| RemnantError::Adapter(format!("redis ttl {key}: {error}")))?;
                let payload: Vec<u8> = redis::cmd("DUMP")
                    .arg(&key)
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| RemnantError::Adapter(format!("redis dump {key}: {error}")))?;
                entries.push(RedisEntry {
                    key,
                    kind,
                    ttl_ms,
                    payload_base64: base64::engine::general_purpose::STANDARD.encode(payload),
                });
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        entries.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(entries)
    }

    fn objects_from_payload(&self, snapshot: &RedisSnapshot) -> Vec<StateObject> {
        snapshot
            .entries
            .iter()
            .map(|entry| {
                let value = json!({
                    "key": entry.key,
                    "type": entry.kind,
                    "ttl_ms": entry.ttl_ms,
                });
                StateObject::new(
                    redis_object_id(&entry.key),
                    self.name.clone(),
                    redis_group(&entry.key),
                    "redis_key",
                    entry.key.clone(),
                    value,
                )
            })
            .collect()
    }
}

#[async_trait]
impl StateSource for RedisAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn describe(&self) -> Result<SourceDescription> {
        let entries = self.capture_entries().await?;
        Ok(SourceDescription {
            name: self.name.clone(),
            kind: "redis".to_string(),
            endpoint: redacted_endpoint(&self.url),
            capabilities: vec![
                "scan_keys".to_string(),
                "dump_restore".to_string(),
                format!("{} keys discovered", entries.len()),
            ],
        })
    }

    async fn enumerate_state(&self) -> Result<Vec<StateObject>> {
        let snapshot = self.snapshot().await?;
        <Self as StateSource>::objects_from_snapshot(self, &snapshot)
    }

    fn objects_from_snapshot(&self, snapshot: &SourceSnapshot) -> Result<Vec<StateObject>> {
        if snapshot.source != self.name || snapshot.kind != "redis" {
            return Err(RemnantError::InvalidSnapshot(format!(
                "expected redis snapshot for {}, got {} snapshot for {}",
                self.name, snapshot.kind, snapshot.source
            )));
        }
        let payload: RedisSnapshot = serde_json::from_value(snapshot.payload.clone())
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        Ok(self.objects_from_payload(&payload))
    }

    async fn snapshot(&self) -> Result<SourceSnapshot> {
        let payload = serde_json::to_value(RedisSnapshot {
            database: self.database,
            entries: self.capture_entries().await?,
        })
        .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        let decoded: RedisSnapshot = serde_json::from_value(payload.clone())
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        Ok(SourceSnapshot {
            source: self.name.clone(),
            kind: "redis".to_string(),
            captured_at: Utc::now(),
            fingerprint: crate::model::fingerprint(&payload),
            object_count: decoded.entries.len(),
            payload,
        })
    }

    async fn restore(
        &self,
        snapshot: &SourceSnapshot,
        retained: Option<&BTreeSet<String>>,
    ) -> Result<()> {
        if snapshot.source != self.name || snapshot.kind != "redis" {
            return Err(RemnantError::InvalidSnapshot(format!(
                "expected redis snapshot for {}, got {} snapshot for {}",
                self.name, snapshot.kind, snapshot.source
            )));
        }
        let payload: RedisSnapshot = serde_json::from_value(snapshot.payload.clone())
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        let mut connection = self.connection().await?;
        let mut cursor = 0u64;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut connection)
                .await
                .map_err(|error| {
                    RemnantError::Adapter(format!("redis scan before restore: {error}"))
                })?;
            if !keys.is_empty() {
                redis::cmd("DEL")
                    .arg(keys)
                    .query_async::<()>(&mut connection)
                    .await
                    .map_err(|error| {
                        RemnantError::Adapter(format!("redis clear before restore: {error}"))
                    })?;
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        for entry in payload.entries {
            let object_id = redis_object_id(&entry.key);
            if retained.is_some_and(|ids| !ids.contains(&object_id)) {
                continue;
            }
            let payload = base64::engine::general_purpose::STANDARD
                .decode(entry.payload_base64)
                .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
            let ttl = if entry.ttl_ms > 0 { entry.ttl_ms } else { 0 };
            redis::cmd("RESTORE")
                .arg(entry.key)
                .arg(ttl)
                .arg(payload)
                .arg("REPLACE")
                .query_async::<()>(&mut connection)
                .await
                .map_err(|error| {
                    RemnantError::Adapter(format!("redis restore {object_id}: {error}"))
                })?;
        }
        Ok(())
    }
}

fn redis_object_id(key: &str) -> String {
    format!("redis:{key}")
}

fn redis_group(key: &str) -> String {
    let prefix = key.split(':').next().unwrap_or(key);
    format!("redis:{prefix}")
}

fn redacted_endpoint(url: &str) -> String {
    url.split('@').next_back().unwrap_or(url).to_string()
}

#[allow(dead_code)]
fn _entry_fingerprint(entry: &RedisEntry) -> String {
    digest_bytes(entry.payload_base64.as_bytes())
}

#[allow(dead_code)]
fn _entry_map(entries: &[RedisEntry]) -> HashMap<&str, &RedisEntry> {
    entries
        .iter()
        .map(|entry| (entry.key.as_str(), entry))
        .collect()
}
