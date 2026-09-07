use std::collections::BTreeSet;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::adapters::StateSource;
use crate::error::{RemnantError, Result};
use crate::model::{Snapshot, StateObject};
use crate::oracle::{OracleOutcome, OracleResult, OracleRunner};
use crate::persistence::SessionStore;
use crate::snapshot::{capture_sources, restore_sources};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Created,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Experiment {
    pub number: u64,
    pub candidate_removed: Vec<String>,
    pub candidate_retained: Vec<String>,
    pub outcome: OracleOutcome,
    pub accepted: bool,
    pub duration_ms: u128,
    pub baseline_fingerprint: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReductionResult {
    pub original_count: usize,
    pub retained_count: usize,
    pub removed_count: usize,
    pub retained_ids: BTreeSet<String>,
    pub failure_reproduced: bool,
    pub minimality: String,
    pub experiments: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReductionSession {
    pub id: String,
    pub project: String,
    pub status: SessionStatus,
    pub strategy: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub baseline: Snapshot,
    pub objects: Vec<StateObject>,
    pub retained_ids: BTreeSet<String>,
    pub baseline_oracle: OracleResult,
    pub experiments: Vec<Experiment>,
    pub result: Option<ReductionResult>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub project: String,
    pub status: SessionStatus,
    pub original_count: usize,
    pub retained_count: usize,
    pub experiments: usize,
    pub updated_at: DateTime<Utc>,
}

impl ReductionSession {
    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            project: self.project.clone(),
            status: self.status.clone(),
            original_count: self.objects.len(),
            retained_count: self.retained_ids.len(),
            experiments: self.experiments.len(),
            updated_at: self.updated_at,
        }
    }
}

pub struct ReductionEngine<'a> {
    sources: &'a [Box<dyn StateSource>],
    oracle: &'a dyn OracleRunner,
    store: &'a SessionStore,
    max_experiments: u64,
}

impl<'a> ReductionEngine<'a> {
    pub fn new(
        sources: &'a [Box<dyn StateSource>],
        oracle: &'a dyn OracleRunner,
        store: &'a SessionStore,
        max_experiments: u64,
    ) -> Self {
        Self {
            sources,
            oracle,
            store,
            max_experiments,
        }
    }

    pub async fn begin(
        &self,
        project: impl Into<String>,
        strategy: impl Into<String>,
    ) -> Result<ReductionSession> {
        let baseline_oracle = self.oracle.run().await?;
        if baseline_oracle.outcome != OracleOutcome::FailureReproduced {
            return Err(RemnantError::InvalidConfig(format!(
                "cannot start reduction: baseline oracle outcome was {:?}",
                baseline_oracle.outcome
            )));
        }
        let (baseline, objects) = capture_sources(self.sources).await?;
        let retained_ids = objects.iter().map(|object| object.id.clone()).collect();
        let now = Utc::now();
        let session = ReductionSession {
            id: format!("session-{}", Uuid::new_v4()),
            project: project.into(),
            status: SessionStatus::Created,
            strategy: strategy.into(),
            created_at: now,
            updated_at: now,
            baseline,
            objects,
            retained_ids,
            baseline_oracle,
            experiments: Vec::new(),
            result: None,
            last_error: None,
        };
        self.store.save(&session)?;
        Ok(session)
    }

    pub async fn run(&self, session: &mut ReductionSession) -> Result<()> {
        if session.status == SessionStatus::Completed {
            return Ok(());
        }
        session.status = SessionStatus::Running;
        session.last_error = None;
        session.updated_at = Utc::now();
        self.store.save(session)?;

        let result = self.reduce(session).await;
        match result {
            Ok(()) => {
                session.status = SessionStatus::Completed;
                session.updated_at = Utc::now();
                self.store.save(session)?;
                Ok(())
            }
            Err(error) => {
                session.status = SessionStatus::Failed;
                session.last_error = Some(error.to_string());
                session.updated_at = Utc::now();
                self.store.save(session)?;
                Err(error)
            }
        }
    }

    async fn reduce(&self, session: &mut ReductionSession) -> Result<()> {
        let mut current: Vec<String> = session.retained_ids.iter().cloned().collect();
        let original_count = current.len();
        let mut partitions = if current.len() > 1 { 2 } else { 1 };

        if current.is_empty() {
            return Err(RemnantError::InvalidConfig(
                "cannot reduce an empty state".to_string(),
            ));
        }

        while current.len() > 1 && partitions <= current.len() {
            let chunks = split_evenly(&current, partitions);
            let mut accepted = false;
            for chunk in chunks {
                if session.experiments.len() as u64 >= self.max_experiments {
                    return Err(RemnantError::Unsupported(format!(
                        "reduction reached max_experiments ({}) before reaching local minimality",
                        self.max_experiments
                    )));
                }
                let chunk_set: BTreeSet<String> = chunk.iter().cloned().collect();
                let candidate: BTreeSet<String> = current
                    .iter()
                    .filter(|id| !chunk_set.contains(*id))
                    .cloned()
                    .collect();
                let experiment = self.test_candidate(session, &candidate, &chunk_set).await?;
                let accepted_this_round = experiment.accepted;
                session.experiments.push(experiment);
                session.retained_ids = candidate.clone();
                session.updated_at = Utc::now();
                if accepted_this_round {
                    current = candidate.into_iter().collect();
                    partitions = partitions.saturating_sub(1).max(2);
                    accepted = true;
                    self.store.save(session)?;
                    break;
                }
                session.retained_ids = current.iter().cloned().collect();
                self.store.save(session)?;
            }
            if !accepted {
                if partitions == current.len() {
                    break;
                }
                partitions = (partitions * 2).min(current.len());
            }
        }

        if current.len() == 1 {
            if session.experiments.len() as u64 >= self.max_experiments {
                return Err(RemnantError::Unsupported(format!(
                    "reduction reached max_experiments ({}) before reaching local minimality",
                    self.max_experiments
                )));
            }
            let removed = BTreeSet::from([current[0].clone()]);
            let empty = BTreeSet::new();
            let experiment = self.test_candidate(session, &empty, &removed).await?;
            let accepted = experiment.accepted;
            session.experiments.push(experiment);
            if accepted {
                current.clear();
            }
            session.retained_ids = current.iter().cloned().collect();
            session.updated_at = Utc::now();
            self.store.save(session)?;
        }

        let retained: BTreeSet<String> = current.iter().cloned().collect();
        let final_result = self
            .test_candidate(session, &retained, &BTreeSet::new())
            .await?;
        if final_result.outcome != OracleOutcome::FailureReproduced {
            return Err(RemnantError::Unsupported(
                "final retained candidate no longer reproduces the failure".to_string(),
            ));
        }
        session.experiments.push(final_result);
        session.retained_ids = retained.clone();
        session.result = Some(ReductionResult {
            original_count,
            retained_count: retained.len(),
            removed_count: original_count.saturating_sub(retained.len()),
            retained_ids: retained,
            failure_reproduced: true,
            minimality: "1-minimal".to_string(),
            experiments: session.experiments.len() as u64,
        });
        session.updated_at = Utc::now();
        self.store.save(session)?;
        Ok(())
    }

    async fn test_candidate(
        &self,
        session: &ReductionSession,
        candidate: &BTreeSet<String>,
        removed: &BTreeSet<String>,
    ) -> Result<Experiment> {
        let started = Instant::now();
        restore_sources(self.sources, &session.baseline, Some(candidate)).await?;
        let oracle_result = self.oracle.run().await;
        let restore_result = restore_sources(self.sources, &session.baseline, None).await;
        restore_result?;
        let oracle_result = oracle_result?;
        if oracle_result.outcome == OracleOutcome::TimedOut {
            return Err(RemnantError::Unsupported(
                "oracle timed out during reduction; no decision was recorded".to_string(),
            ));
        }
        Ok(Experiment {
            number: session.experiments.len() as u64 + 1,
            candidate_removed: removed.iter().cloned().collect(),
            candidate_retained: candidate.iter().cloned().collect(),
            outcome: oracle_result.outcome,
            accepted: oracle_result.outcome == OracleOutcome::FailureReproduced,
            duration_ms: started.elapsed().as_millis(),
            baseline_fingerprint: session.baseline.fingerprint.clone(),
            recorded_at: Utc::now(),
        })
    }
}

fn split_evenly(items: &[String], parts: usize) -> Vec<Vec<String>> {
    let parts = parts.max(1).min(items.len().max(1));
    let base = items.len() / parts;
    let remainder = items.len() % parts;
    let mut chunks = Vec::with_capacity(parts);
    let mut start = 0;
    for index in 0..parts {
        let size = base + usize::from(index < remainder);
        let end = start + size;
        chunks.push(items[start..end].to_vec());
        start = end;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Utc;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::model::{SourceSnapshot, StateObject};

    #[derive(Clone)]
    struct FakeSource {
        state: Arc<Mutex<BTreeSet<String>>>,
        objects: Vec<StateObject>,
    }

    #[async_trait]
    impl StateSource for FakeSource {
        fn name(&self) -> &str {
            "fake"
        }

        async fn describe(&self) -> Result<crate::model::SourceDescription> {
            Ok(crate::model::SourceDescription {
                name: "fake".to_string(),
                kind: "fake".to_string(),
                endpoint: "memory".to_string(),
                capabilities: vec!["restore_subset".to_string()],
            })
        }

        async fn enumerate_state(&self) -> Result<Vec<StateObject>> {
            Ok(self.objects.clone())
        }

        async fn snapshot(&self) -> Result<crate::model::SourceSnapshot> {
            Ok(SourceSnapshot {
                source: "fake".to_string(),
                kind: "fake".to_string(),
                captured_at: Utc::now(),
                fingerprint: "fake-fingerprint".to_string(),
                object_count: self.objects.len(),
                payload: json!({"objects": self.objects}),
            })
        }

        fn objects_from_snapshot(&self, snapshot: &SourceSnapshot) -> Result<Vec<StateObject>> {
            serde_json::from_value(snapshot.payload["objects"].clone())
                .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))
        }

        async fn restore(
            &self,
            _snapshot: &SourceSnapshot,
            retained: Option<&BTreeSet<String>>,
        ) -> Result<()> {
            let mut state = self.state.lock().expect("state lock");
            *state = retained.cloned().unwrap_or_else(|| {
                self.objects
                    .iter()
                    .map(|object| object.id.clone())
                    .collect()
            });
            Ok(())
        }
    }

    struct FakeOracle {
        state: Arc<Mutex<BTreeSet<String>>>,
    }

    #[async_trait]
    impl OracleRunner for FakeOracle {
        async fn run(&self) -> Result<OracleResult> {
            let state = self.state.lock().expect("state lock");
            let reproduced = state.contains("a") && state.contains("b");
            Ok(OracleResult {
                command: "fake".to_string(),
                outcome: if reproduced {
                    OracleOutcome::FailureReproduced
                } else {
                    OracleOutcome::FailureAbsent
                },
                exit_code: Some(if reproduced { 1 } else { 0 }),
                duration_ms: 0,
                started_at: Utc::now(),
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn split_evenly_preserves_order_and_cardinality() {
        let items = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let chunks = split_evenly(&items, 3);
        assert_eq!(chunks, vec![vec!["a", "b"], vec!["c"], vec!["d"]]);
    }

    #[tokio::test]
    async fn reducer_finds_a_locally_minimal_cross_object_subset() {
        let objects = ["a", "b", "noise-1", "noise-2", "noise-3"]
            .into_iter()
            .map(|id| StateObject::new(id, "fake", "objects", "item", id, json!({"id": id})))
            .collect::<Vec<_>>();
        let state = Arc::new(Mutex::new(
            objects.iter().map(|object| object.id.clone()).collect(),
        ));
        let source: Box<dyn StateSource> = Box::new(FakeSource {
            state: Arc::clone(&state),
            objects,
        });
        let oracle = FakeOracle { state };
        let directory = tempdir().expect("tempdir");
        let store = SessionStore::new(directory.path());
        let sources = vec![source];
        let engine = ReductionEngine::new(&sources, &oracle, &store, 100);
        let mut session = engine.begin("test", "ddmin").await.expect("begin");
        engine.run(&mut session).await.expect("reduce");
        assert_eq!(session.status, SessionStatus::Completed);
        assert_eq!(
            session.retained_ids,
            BTreeSet::from(["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            session.result.as_ref().expect("result").minimality,
            "1-minimal"
        );
        assert!(store.load(&session.id).is_ok());
    }
}
