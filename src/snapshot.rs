use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::adapters::StateSource;
use crate::error::{RemnantError, Result};
use crate::model::{SNAPSHOT_FORMAT_VERSION, Snapshot, SourceSnapshot, StateObject, fingerprint};

const MAX_SNAPSHOT_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

pub async fn capture_sources(
    sources: &[Box<dyn StateSource>],
) -> Result<(Snapshot, Vec<StateObject>)> {
    let mut source_snapshots = BTreeMap::new();
    let mut objects = Vec::new();
    for source in sources {
        let mut snapshot = source.snapshot().await?;
        snapshot.target_identity = source.target_identity().await?;
        objects.extend(source.objects_from_snapshot(&snapshot)?);
        source_snapshots.insert(source.name().to_string(), snapshot);
    }
    let snapshot_fingerprint = fingerprint(&json!({
        "sources": source_snapshots
            .iter()
            .map(|(name, snapshot)| (name, &snapshot.fingerprint))
            .collect::<BTreeMap<_, _>>(),
    }));
    let snapshot = Snapshot {
        format_version: SNAPSHOT_FORMAT_VERSION,
        id: format!("snapshot-{}", Uuid::new_v4()),
        captured_at: Utc::now(),
        sources: source_snapshots,
        fingerprint: snapshot_fingerprint,
    };
    objects.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((snapshot, objects))
}

pub async fn restore_sources(
    sources: &[Box<dyn StateSource>],
    snapshot: &Snapshot,
    retained: Option<&BTreeSet<String>>,
) -> Result<()> {
    validate_snapshot_for_sources(sources, snapshot)?;
    let mut failures = Vec::new();
    for source in sources {
        let source_snapshot: &SourceSnapshot =
            snapshot.sources.get(source.name()).ok_or_else(|| {
                crate::error::RemnantError::InvalidSnapshot(format!(
                    "snapshot has no source named {}",
                    source.name()
                ))
            })?;
        if let Err(error) = source.restore(source_snapshot, retained).await {
            failures.push(format!("{}: {error}", source.name()));
        }
    }
    if !failures.is_empty() {
        return Err(RemnantError::Adapter(format!(
            "restore failed for {}",
            failures.join("; ")
        )));
    }
    Ok(())
}

/// Verify that the stored snapshot is internally intact and that every source
/// currently matches the selected state from it. This deliberately compares
/// state objects rather than capture timestamps.
pub async fn verify_sources(
    sources: &[Box<dyn StateSource>],
    snapshot: &Snapshot,
    retained: Option<&BTreeSet<String>>,
) -> Result<()> {
    validate_snapshot_for_sources(sources, snapshot)?;
    let mut failures = Vec::new();
    for source in sources {
        let source_snapshot = snapshot.sources.get(source.name()).ok_or_else(|| {
            RemnantError::InvalidSnapshot(format!("snapshot has no source named {}", source.name()))
        })?;
        let actual_target = source.target_identity().await?;
        if source_snapshot.target_identity != actual_target {
            return Err(RemnantError::UnsafeOperation(format!(
                "source {} target identity changed: expected {}, found {}",
                source.name(),
                source_snapshot.target_identity,
                actual_target
            )));
        }
        if let Err(error) = source.verify_restored(source_snapshot, retained).await {
            failures.push(format!("{}: {error}", source.name()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(RemnantError::Adapter(format!(
            "state verification failed for {}",
            failures.join("; ")
        )))
    }
}

fn validate_snapshot_for_sources(
    sources: &[Box<dyn StateSource>],
    snapshot: &Snapshot,
) -> Result<()> {
    validate_snapshot(snapshot)?;
    let configured: BTreeSet<&str> = sources.iter().map(|source| source.name()).collect();
    let captured: BTreeSet<&str> = snapshot.sources.keys().map(String::as_str).collect();
    if configured != captured {
        return Err(RemnantError::InvalidSnapshot(format!(
            "snapshot sources do not match configured sources: captured [{}], configured [{}]",
            captured.into_iter().collect::<Vec<_>>().join(", "),
            configured.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(())
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(RemnantError::InvalidSnapshot(format!(
            "snapshot format version {} is not supported; expected {SNAPSHOT_FORMAT_VERSION}",
            snapshot.format_version
        )));
    }
    let mut source_fingerprints = BTreeMap::new();
    for (name, source) in &snapshot.sources {
        if source.format_version != SNAPSHOT_FORMAT_VERSION {
            return Err(RemnantError::InvalidSnapshot(format!(
                "source {name} uses unsupported snapshot format version {}; expected {SNAPSHOT_FORMAT_VERSION}",
                source.format_version
            )));
        }
        if source.source != *name {
            return Err(RemnantError::InvalidSnapshot(format!(
                "snapshot source name mismatch: map key {name}, payload {}",
                source.source
            )));
        }
        if source.target_identity.trim().is_empty() {
            return Err(RemnantError::InvalidSnapshot(format!(
                "source {name} has no target identity"
            )));
        }
        let payload = serde_json::to_vec(&source.payload)
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        if payload.len() > MAX_SNAPSHOT_PAYLOAD_BYTES {
            return Err(RemnantError::InvalidSnapshot(format!(
                "source {name} payload is {} bytes, limit is {MAX_SNAPSHOT_PAYLOAD_BYTES}",
                payload.len()
            )));
        }
        let actual = crate::model::digest_bytes(&payload);
        if actual != source.fingerprint {
            return Err(RemnantError::InvalidSnapshot(format!(
                "fingerprint mismatch for source {name}: expected {}, got {actual}",
                source.fingerprint
            )));
        }
        source_fingerprints.insert(name, &source.fingerprint);
    }
    let actual = fingerprint(&json!({"sources": source_fingerprints}));
    if actual != snapshot.fingerprint {
        return Err(RemnantError::InvalidSnapshot(format!(
            "snapshot fingerprint mismatch: expected {}, got {actual}",
            snapshot.fingerprint
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_snapshot_format_before_restore() {
        let sources = BTreeMap::new();
        let snapshot = Snapshot {
            format_version: SNAPSHOT_FORMAT_VERSION + 1,
            id: "snapshot-test".to_string(),
            captured_at: Utc::now(),
            sources,
            fingerprint: fingerprint(&json!({"sources": BTreeMap::<String, String>::new()})),
        };

        assert!(validate_snapshot(&snapshot).is_err());
    }
}
