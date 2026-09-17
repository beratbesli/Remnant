use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::adapters::StateSource;
use crate::error::{RemnantError, Result};
use crate::model::{Snapshot, SourceSnapshot, StateObject, fingerprint};

pub async fn capture_sources(
    sources: &[Box<dyn StateSource>],
) -> Result<(Snapshot, Vec<StateObject>)> {
    let mut source_snapshots = BTreeMap::new();
    let mut objects = Vec::new();
    for source in sources {
        let snapshot = source.snapshot().await?;
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
    let mut source_fingerprints = BTreeMap::new();
    for (name, source) in &snapshot.sources {
        if source.source != *name {
            return Err(RemnantError::InvalidSnapshot(format!(
                "snapshot source name mismatch: map key {name}, payload {}",
                source.source
            )));
        }
        let actual = fingerprint(&source.payload);
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
