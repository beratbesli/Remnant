use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::StateSource;
use crate::error::{RemnantError, Result};
use crate::model::{SNAPSHOT_FORMAT_VERSION, SourceDescription, SourceSnapshot, StateObject};

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
    /// Redis keys are bytes, not UTF-8 strings. Base64 preserves arbitrary
    /// binary keys in the portable JSON snapshot format.
    pub key_base64: String,
    pub kind: String,
    pub ttl_ms: i64,
    pub payload_base64: String,
}

struct DecodedRedisEntry {
    key: Vec<u8>,
    kind: String,
    ttl_ms: i64,
    payload: Vec<u8>,
}

const TTL_RESTORE_DRIFT_MS: i64 = 5_000;

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
            let (next, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
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
                    .map_err(|error| RemnantError::Adapter(format!("redis type: {error}")))?;
                if kind == "none" {
                    continue;
                }
                let ttl_ms: i64 = redis::cmd("PTTL")
                    .arg(&key)
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| RemnantError::Adapter(format!("redis ttl: {error}")))?;
                if ttl_ms == -2 {
                    continue;
                }
                let payload: Vec<u8> = redis::cmd("DUMP")
                    .arg(&key)
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| RemnantError::Adapter(format!("redis dump: {error}")))?;
                entries.push(RedisEntry {
                    key_base64: STANDARD.encode(&key),
                    kind,
                    ttl_ms,
                    payload_base64: STANDARD.encode(payload),
                });
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        entries.sort_by(|left, right| left.key_base64.cmp(&right.key_base64));
        Ok(entries)
    }

    fn objects_from_payload(&self, snapshot: &RedisSnapshot) -> Vec<StateObject> {
        snapshot
            .entries
            .iter()
            .map(|entry| {
                let value = json!({
                    "key_base64": entry.key_base64,
                    "type": entry.kind,
                    "ttl_ms": entry.ttl_ms,
                });
                StateObject::new(
                    redis_object_id_from_base64(&entry.key_base64),
                    self.name.clone(),
                    redis_group(&entry.key_base64),
                    "redis_key",
                    redis_label(&entry.key_base64),
                    value,
                )
            })
            .collect()
    }

    fn decode_snapshot(&self, snapshot: &SourceSnapshot) -> Result<RedisSnapshot> {
        validate_snapshot(snapshot, &self.name, "redis")?;
        let payload: RedisSnapshot = serde_json::from_value(snapshot.payload.clone())
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        if payload.database != self.database {
            return Err(RemnantError::InvalidSnapshot(format!(
                "redis snapshot database {} does not match configured database {}",
                payload.database, self.database
            )));
        }
        Ok(payload)
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

    async fn target_identity(&self) -> Result<String> {
        Ok(format!(
            "{} database={}",
            redacted_endpoint(&self.url),
            self.database
        ))
    }

    fn objects_from_snapshot(&self, snapshot: &SourceSnapshot) -> Result<Vec<StateObject>> {
        let payload = self.decode_snapshot(snapshot)?;
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
            format_version: SNAPSHOT_FORMAT_VERSION,
            source: self.name.clone(),
            kind: "redis".to_string(),
            target_identity: String::new(),
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
        let payload = self.decode_snapshot(snapshot)?;
        let all_entries = decode_entries(&payload.entries)?;
        let candidate_entries = select_entries(&payload.entries, retained)?;
        let mut connection = self.connection().await?;
        match replace_entries(&mut connection, &candidate_entries).await {
            Ok(()) => Ok(()),
            Err(error) => match replace_entries(&mut connection, &all_entries).await {
                Ok(()) => Err(error),
                Err(recovery_error) => Err(RemnantError::UnsafeOperation(format!(
                    "redis candidate restore failed and full snapshot recovery also failed: {recovery_error}"
                ))),
            },
        }
    }

    async fn verify_restored(
        &self,
        snapshot: &SourceSnapshot,
        retained: Option<&BTreeSet<String>>,
    ) -> Result<()> {
        let payload = self.decode_snapshot(snapshot)?;
        let expected = select_entries(&payload.entries, retained)?;
        let actual = self.capture_entries().await?;
        verify_entries(&expected, &actual)
    }
}

fn decode_entries(entries: &[RedisEntry]) -> Result<Vec<DecodedRedisEntry>> {
    entries
        .iter()
        .map(|entry| {
            let key = STANDARD.decode(&entry.key_base64).map_err(|error| {
                RemnantError::InvalidSnapshot(format!("invalid Redis key: {error}"))
            })?;
            let payload = STANDARD.decode(&entry.payload_base64).map_err(|error| {
                RemnantError::InvalidSnapshot(format!("invalid Redis payload: {error}"))
            })?;
            if entry.ttl_ms <= -2 {
                return Err(RemnantError::InvalidSnapshot(format!(
                    "invalid Redis TTL {} for {}",
                    entry.ttl_ms,
                    redis_label(&entry.key_base64)
                )));
            }
            Ok(DecodedRedisEntry {
                key,
                kind: entry.kind.clone(),
                ttl_ms: entry.ttl_ms,
                payload,
            })
        })
        .collect()
}

fn select_entries(
    entries: &[RedisEntry],
    retained: Option<&BTreeSet<String>>,
) -> Result<Vec<DecodedRedisEntry>> {
    let selected = entries
        .iter()
        .filter(|entry| {
            retained.is_none_or(|ids| ids.contains(&redis_object_id_from_base64(&entry.key_base64)))
        })
        .cloned()
        .collect::<Vec<_>>();
    decode_entries(&selected)
}

async fn replace_entries(
    connection: &mut redis::aio::MultiplexedConnection,
    entries: &[DecodedRedisEntry],
) -> Result<()> {
    redis::cmd("FLUSHDB")
        .arg("SYNC")
        .query_async::<()>(connection)
        .await
        .map_err(|error| RemnantError::Adapter(format!("redis clear before restore: {error}")))?;
    for entry in entries {
        let ttl = if entry.ttl_ms > 0 { entry.ttl_ms } else { 0 };
        redis::cmd("RESTORE")
            .arg(&entry.key)
            .arg(ttl)
            .arg(&entry.payload)
            .arg("REPLACE")
            .query_async::<()>(connection)
            .await
            .map_err(|error| {
                RemnantError::Adapter(format!(
                    "redis restore {}: {error}",
                    redis_label(&STANDARD.encode(&entry.key))
                ))
            })?;
    }
    Ok(())
}

fn verify_entries(expected: &[DecodedRedisEntry], actual: &[RedisEntry]) -> Result<()> {
    let expected = expected
        .iter()
        .map(|entry| (STANDARD.encode(&entry.key), entry))
        .collect::<BTreeMap<_, _>>();
    let actual = actual
        .iter()
        .map(|entry| (entry.key_base64.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    if expected.len() != actual.len() {
        return Err(RemnantError::Adapter(format!(
            "redis restore verification failed: expected {} keys, found {}",
            expected.len(),
            actual.len()
        )));
    }
    for (key, expected_entry) in expected {
        let actual_entry = actual.get(key.as_str()).ok_or_else(|| {
            RemnantError::Adapter(format!(
                "redis restore verification failed: missing key {}",
                redis_label(&key)
            ))
        })?;
        if actual_entry.kind != expected_entry.kind
            || actual_entry.payload_base64 != STANDARD.encode(&expected_entry.payload)
            || !ttl_matches(expected_entry.ttl_ms, actual_entry.ttl_ms)
        {
            return Err(RemnantError::Adapter(format!(
                "redis restore verification failed for key {}",
                redis_label(&key)
            )));
        }
    }
    Ok(())
}

fn ttl_matches(expected: i64, actual: i64) -> bool {
    match expected {
        -1 => actual == -1,
        value if value > 0 => {
            actual > 0 && actual <= value && value.saturating_sub(actual) <= TTL_RESTORE_DRIFT_MS
        }
        _ => false,
    }
}

pub(crate) fn redis_object_id_from_base64(key_base64: &str) -> String {
    match STANDARD.decode(key_base64) {
        Ok(key) => format!("redis:{}", URL_SAFE_NO_PAD.encode(key)),
        Err(_) => format!("redis:invalid-{key_base64}"),
    }
}

fn redis_group(key_base64: &str) -> String {
    let Ok(key) = STANDARD.decode(key_base64) else {
        return "redis:invalid".to_string();
    };
    let prefix = key.split(|byte| *byte == b':').next().unwrap_or(&key);
    format!("redis:{}", URL_SAFE_NO_PAD.encode(prefix))
}

fn redis_label(key_base64: &str) -> String {
    STANDARD
        .decode(key_base64)
        .ok()
        .and_then(|key| String::from_utf8(key).ok())
        .filter(|key| key.chars().all(|character| !character.is_control()))
        .unwrap_or_else(|| format!("base64:{key_base64}"))
}

fn redacted_endpoint(url: &str) -> String {
    url.split('@').next_back().unwrap_or(url).to_string()
}

fn validate_snapshot(snapshot: &SourceSnapshot, source: &str, kind: &str) -> Result<()> {
    if snapshot.source != source || snapshot.kind != kind {
        return Err(RemnantError::InvalidSnapshot(format!(
            "expected {kind} snapshot for {source}, got {} snapshot for {}",
            snapshot.kind, snapshot.source
        )));
    }
    if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(RemnantError::InvalidSnapshot(format!(
            "Redis snapshot format version {} is not supported; expected {SNAPSHOT_FORMAT_VERSION}",
            snapshot.format_version
        )));
    }
    let actual_fingerprint = crate::model::fingerprint(&snapshot.payload);
    if actual_fingerprint != snapshot.fingerprint {
        return Err(RemnantError::InvalidSnapshot(format!(
            "fingerprint mismatch for {source}: expected {}, got {actual_fingerprint}",
            snapshot.fingerprint
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_keys_have_safe_distinct_object_ids() {
        let first = STANDARD.encode([0, 255]);
        let second = STANDARD.encode([0, 254]);

        assert_ne!(
            redis_object_id_from_base64(&first),
            redis_object_id_from_base64(&second)
        );
        assert_eq!(redis_label(&first), format!("base64:{first}"));
    }

    #[test]
    fn ttl_verification_allows_restore_drift_but_not_a_changed_expiry() {
        assert!(ttl_matches(10_000, 9_999));
        assert!(ttl_matches(10_000, 5_000));
        assert!(!ttl_matches(10_000, 4_999));
        assert!(!ttl_matches(10_000, -1));
        assert!(ttl_matches(-1, -1));
    }

    #[test]
    fn invalid_binary_snapshot_data_is_rejected_before_mutation() {
        let entries = vec![RedisEntry {
            key_base64: "not base64".to_string(),
            kind: "string".to_string(),
            ttl_ms: -1,
            payload_base64: STANDARD.encode(b"payload"),
        }];

        assert!(decode_entries(&entries).is_err());
    }
}
