use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::StateObject;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dependency {
    pub from: String,
    pub to: String,
    pub reason: String,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateGraph {
    pub object_ids: Vec<String>,
    pub dependencies: Vec<Dependency>,
}

impl StateGraph {
    pub fn build(objects: &[StateObject]) -> Self {
        let mut token_index: BTreeMap<String, Vec<(&str, &str)>> = BTreeMap::new();
        for object in objects
            .iter()
            .filter(|object| object.kind == "postgres_row")
        {
            let mut tokens = BTreeSet::new();
            collect_stable_tokens(&object.value, &mut tokens);
            for token in tokens {
                token_index
                    .entry(token)
                    .or_default()
                    .push((&object.id, &object.group));
            }
        }

        let mut dependencies = BTreeSet::new();
        for object in objects {
            let tokens: BTreeSet<String> = if object.kind == "redis_key" {
                object
                    .label
                    .split(':')
                    .filter(|segment| !segment.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            } else if object.kind == "postgres_row" {
                let mut tokens = BTreeSet::new();
                collect_stable_tokens(&object.value, &mut tokens);
                tokens
            } else {
                BTreeSet::new()
            };
            for token in tokens {
                for (target_id, target_group) in token_index.get(&token).into_iter().flatten() {
                    if *target_id == object.id || *target_group == object.group {
                        continue;
                    }
                    let reason = if object.kind == "redis_key" {
                        "Redis key segment matches PostgreSQL row value"
                    } else {
                        "PostgreSQL row value matches another table's value"
                    };
                    dependencies.insert(Dependency {
                        from: object.id.clone(),
                        to: (*target_id).to_string(),
                        reason: reason.to_string(),
                        verified: false,
                    });
                }
            }
        }
        Self {
            object_ids: objects.iter().map(|object| object.id.clone()).collect(),
            dependencies: dependencies.into_iter().collect(),
        }
    }
}

fn collect_stable_tokens(value: &Value, tokens: &mut BTreeSet<String>) {
    match value {
        Value::Number(number) => {
            tokens.insert(number.to_string());
        }
        Value::String(string)
            if string.len() >= 6 || string.chars().all(|char| char.is_ascii_digit()) =>
        {
            tokens.insert(string.clone());
        }
        Value::Array(values) => {
            for value in values {
                collect_stable_tokens(value, tokens);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_stable_tokens(value, tokens);
            }
        }
        Value::Bool(_) | Value::Null | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn links_redis_identifier_to_postgres_state_deterministically() {
        let objects = vec![
            StateObject::new(
                "postgres:public.users:{\"id\":174}",
                "postgres",
                "postgres:public.users",
                "postgres_row",
                "public.users",
                json!({"id": 174}),
            ),
            StateObject::new(
                "postgres:public.subscriptions:{\"id\":991}",
                "postgres",
                "postgres:public.subscriptions",
                "postgres_row",
                "public.subscriptions",
                json!({"id": 991, "user_id": 174}),
            ),
            StateObject::new(
                "redis:user:174:plan",
                "redis",
                "redis:user",
                "redis_key",
                "user:174:plan",
                json!({"key": "user:174:plan"}),
            ),
        ];
        let graph = StateGraph::build(&objects);
        assert!(graph.dependencies.iter().any(|dependency| {
            dependency.from == "redis:user:174:plan"
                && dependency.to == "postgres:public.users:{\"id\":174}"
        }));
        assert!(graph.dependencies.iter().any(|dependency| {
            dependency.from == "postgres:public.subscriptions:{\"id\":991}"
                && dependency.to == "postgres:public.users:{\"id\":174}"
        }));
        assert!(
            graph
                .dependencies
                .iter()
                .all(|dependency| !dependency.verified)
        );
    }
}
