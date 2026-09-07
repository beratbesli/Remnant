use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{RemnantError, Result};
use crate::graph::{Dependency, StateGraph};
use crate::reducer::{Experiment, ReductionSession};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReductionReport {
    pub project: String,
    pub session_id: String,
    pub snapshot_fingerprint: String,
    pub original_count: usize,
    pub reduced_count: usize,
    pub removed_count: usize,
    pub reduction_percent: f64,
    pub experiments: usize,
    pub failure_reproduced: bool,
    pub minimality: String,
    pub counts_by_source: BTreeMap<String, SourceCounts>,
    pub retained_objects: Vec<ReportObject>,
    pub evidence: Vec<Experiment>,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceCounts {
    pub original: usize,
    pub retained: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportObject {
    pub id: String,
    pub source: String,
    pub group: String,
    pub kind: String,
    pub label: String,
    pub value: serde_json::Value,
    pub fingerprint: String,
}

impl ReductionReport {
    pub fn from_session(session: &ReductionSession) -> Result<Self> {
        let result = session.result.as_ref().ok_or_else(|| {
            RemnantError::Unsupported("session has no completed reduction result".to_string())
        })?;
        let mut counts_by_source: BTreeMap<String, SourceCounts> = BTreeMap::new();
        for object in &session.objects {
            let counts = counts_by_source
                .entry(object.source.clone())
                .or_insert(SourceCounts {
                    original: 0,
                    retained: 0,
                });
            counts.original += 1;
            if session.retained_ids.contains(&object.id) {
                counts.retained += 1;
            }
        }
        let retained_objects = session
            .objects
            .iter()
            .filter(|object| session.retained_ids.contains(&object.id))
            .map(|object| ReportObject {
                id: object.id.clone(),
                source: object.source.clone(),
                group: object.group.clone(),
                kind: object.kind.clone(),
                label: object.label.clone(),
                value: object.value.clone(),
                fingerprint: object.fingerprint.clone(),
            })
            .collect();
        Ok(Self {
            project: session.project.clone(),
            session_id: session.id.clone(),
            snapshot_fingerprint: session.baseline.fingerprint.clone(),
            original_count: result.original_count,
            reduced_count: result.retained_count,
            removed_count: result.removed_count,
            reduction_percent: if result.original_count == 0 {
                0.0
            } else {
                (result.removed_count as f64 / result.original_count as f64) * 100.0
            },
            experiments: session.experiments.len(),
            failure_reproduced: result.failure_reproduced,
            minimality: result.minimality.clone(),
            counts_by_source,
            retained_objects,
            evidence: session.experiments.clone(),
            dependencies: StateGraph::build(&session.objects).dependencies,
        })
    }

    pub fn render_text(&self) -> String {
        let mut output = String::new();
        output.push_str("REMNANT REDUCTION REPORT\n\n");
        output.push_str(&format!("Project: {}\n", self.project));
        output.push_str(&format!("Session: {}\n", self.session_id));
        output.push_str(&format!(
            "Original state: {} objects\n",
            self.original_count
        ));
        output.push_str(&format!("Reduced state: {} objects\n", self.reduced_count));
        output.push_str(&format!(
            "Removed: {} ({:.2}%)\n",
            self.removed_count, self.reduction_percent
        ));
        output.push_str(&format!("Experiments: {}\n", self.experiments));
        output.push_str(&format!(
            "Failure reproduced: {}\n",
            if self.failure_reproduced { "YES" } else { "NO" }
        ));
        output.push_str(&format!("Minimality: {}\n\n", self.minimality));
        output.push_str("COUNTS BY SOURCE\n");
        for (source, counts) in &self.counts_by_source {
            output.push_str(&format!(
                "  {source}: {} -> {}\n",
                counts.original, counts.retained
            ));
        }
        output.push_str("\nRETAINED STATE\n");
        for object in &self.retained_objects {
            output.push_str(&format!(
                "  [{}] {} ({})\n",
                object.source, object.label, object.id
            ));
        }
        if !self.dependencies.is_empty() {
            output.push_str("\nRELATIONSHIP HYPOTHESES\n");
            for dependency in &self.dependencies {
                output.push_str(&format!(
                    "  {} -> {} ({}){}\n",
                    dependency.from,
                    dependency.to,
                    dependency.reason,
                    if dependency.verified {
                        " [verified]"
                    } else {
                        " [unverified]"
                    }
                ));
            }
        }
        output
    }

    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<()> {
        let contents = serde_json::to_vec_pretty(self)
            .map_err(|error| RemnantError::Persistence(format!("serialize report: {error}")))?;
        fs::write(path, contents).map_err(|error| RemnantError::Persistence(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::model::{Snapshot, SourceSnapshot, StateObject};
    use crate::oracle::{OracleOutcome, OracleResult};
    use crate::reducer::{ReductionResult, ReductionSession, SessionStatus};

    #[test]
    fn report_counts_retained_objects_by_source() {
        let objects = vec![
            StateObject::new(
                "pg:1",
                "postgres",
                "users",
                "row",
                "users",
                json!({"id": 1}),
            ),
            StateObject::new("redis:key", "redis", "redis:user", "key", "key", json!({})),
        ];
        let session = ReductionSession {
            id: "session-test".to_string(),
            project: "test".to_string(),
            status: SessionStatus::Completed,
            strategy: "ddmin".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            baseline: Snapshot {
                id: "snapshot-test".to_string(),
                captured_at: Utc::now(),
                sources: BTreeMap::from([(
                    "fake".to_string(),
                    SourceSnapshot {
                        source: "fake".to_string(),
                        kind: "fake".to_string(),
                        captured_at: Utc::now(),
                        fingerprint: "fingerprint".to_string(),
                        object_count: 2,
                        payload: json!({}),
                    },
                )]),
                fingerprint: "fingerprint".to_string(),
            },
            retained_ids: BTreeSet::from(["pg:1".to_string()]),
            baseline_oracle: OracleResult {
                command: "fake".to_string(),
                outcome: OracleOutcome::FailureReproduced,
                exit_code: Some(1),
                duration_ms: 0,
                started_at: Utc::now(),
                stdout: String::new(),
                stderr: String::new(),
            },
            experiments: Vec::new(),
            result: Some(ReductionResult {
                original_count: 2,
                retained_count: 1,
                removed_count: 1,
                retained_ids: BTreeSet::from(["pg:1".to_string()]),
                failure_reproduced: true,
                minimality: "1-minimal".to_string(),
                experiments: 1,
            }),
            last_error: None,
            objects,
        };
        let report = ReductionReport::from_session(&session).expect("report");
        assert_eq!(report.counts_by_source["postgres"].retained, 1);
        assert_eq!(report.counts_by_source["redis"].retained, 0);
    }
}
