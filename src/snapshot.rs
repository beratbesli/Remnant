use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::adapters::StateSource;
use crate::error::Result;
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
    Ok((snapshot, objects))
}

pub async fn restore_sources(
    sources: &[Box<dyn StateSource>],
    snapshot: &Snapshot,
    retained: Option<&BTreeSet<String>>,
) -> Result<()> {
    for source in sources {
        let source_snapshot: &SourceSnapshot =
            snapshot.sources.get(source.name()).ok_or_else(|| {
                crate::error::RemnantError::InvalidSnapshot(format!(
                    "snapshot has no source named {}",
                    source.name()
                ))
            })?;
        source.restore(source_snapshot, retained).await?;
    }
    Ok(())
}
