use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateObject {
    pub id: String,
    pub source: String,
    pub group: String,
    pub kind: String,
    pub label: String,
    pub value: Value,
    pub fingerprint: String,
}

impl StateObject {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        group: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
        value: Value,
    ) -> Self {
        let fingerprint = fingerprint(&value);
        Self {
            id: id.into(),
            source: source.into(),
            group: group.into(),
            kind: kind.into(),
            label: label.into(),
            value,
            fingerprint,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateGroup {
    pub id: String,
    pub source: String,
    pub label: String,
    pub object_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceDescription {
    pub name: String,
    pub kind: String,
    pub endpoint: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceSnapshot {
    pub source: String,
    pub kind: String,
    pub captured_at: DateTime<Utc>,
    pub fingerprint: String,
    pub object_count: usize,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub id: String,
    pub captured_at: DateTime<Utc>,
    pub sources: BTreeMap<String, SourceSnapshot>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateSet {
    pub retained: BTreeSet<String>,
}

impl CandidateSet {
    pub fn from_objects(objects: impl IntoIterator<Item = StateObject>) -> Self {
        Self {
            retained: objects.into_iter().map(|object| object.id).collect(),
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.retained.contains(id)
    }

    pub fn len(&self) -> usize {
        self.retained.len()
    }

    pub fn is_empty(&self) -> bool {
        self.retained.is_empty()
    }
}

pub fn fingerprint(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("serde_json::Value is always serializable");
    digest_bytes(&bytes)
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn group_objects(objects: &[StateObject]) -> Vec<StateGroup> {
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for object in objects {
        groups
            .entry((object.source.clone(), object.group.clone()))
            .or_default()
            .push(object.id.clone());
    }
    groups
        .into_iter()
        .map(|((source, label), object_ids)| StateGroup {
            id: format!("{source}:{label}"),
            source,
            label,
            object_ids,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn object_fingerprint_is_stable_for_same_value() {
        let first = StateObject::new("one", "test", "group", "row", "one", json!({"a": 1}));
        let second = StateObject::new("one", "test", "group", "row", "one", json!({"a": 1}));
        assert_eq!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn groups_are_deterministic() {
        let objects = vec![
            StateObject::new("b", "redis", "keys", "key", "b", json!(null)),
            StateObject::new("a", "postgres", "users", "row", "a", json!(null)),
            StateObject::new("c", "redis", "keys", "key", "c", json!(null)),
        ];
        let groups = group_objects(&objects);
        assert_eq!(groups[0].id, "postgres:users");
        assert_eq!(groups[1].object_ids, vec!["b", "c"]);
    }
}
