use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_postgres::{Client, NoTls};

use super::StateSource;
use crate::error::{RemnantError, Result};
use crate::model::{SourceDescription, SourceSnapshot, StateObject, digest_bytes};

#[derive(Debug, Clone)]
pub struct PostgresAdapter {
    name: String,
    url: String,
    schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostgresSnapshot {
    pub tables: Vec<PostgresTableSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostgresTableSnapshot {
    pub schema: String,
    pub table: String,
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<PostgresForeignKey>,
    pub rows: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PostgresForeignKey {
    pub columns: Vec<String>,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

impl PostgresAdapter {
    pub fn new(name: impl Into<String>, url: impl Into<String>, schema: Option<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            schema,
        }
    }

    async fn client(&self) -> Result<Client> {
        let (client, connection) = tokio_postgres::connect(&self.url, NoTls)
            .await
            .map_err(|error| RemnantError::Adapter(format!("postgres {}: {error}", self.name)))?;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::error!(%error, "postgres connection ended");
            }
        });
        Ok(client)
    }

    async fn table_names(&self, client: &Client) -> Result<Vec<(String, String)>> {
        let rows = client
            .query(
                "SELECT table_schema, table_name
                 FROM information_schema.tables
                 WHERE table_type = 'BASE TABLE'
                   AND table_schema NOT IN ('pg_catalog', 'information_schema')
                   AND ($1::text IS NULL OR table_schema = $1)
                 ORDER BY table_schema, table_name",
                &[&self.schema],
            )
            .await
            .map_err(|error| RemnantError::Adapter(format!("postgres table discovery: {error}")))?;
        Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
    }

    async fn primary_key(&self, client: &Client, schema: &str, table: &str) -> Result<Vec<String>> {
        let rows = client
            .query(
                "SELECT kcu.column_name
                 FROM information_schema.table_constraints tc
                 JOIN information_schema.key_column_usage kcu
                   ON tc.constraint_name = kcu.constraint_name
                  AND tc.table_schema = kcu.table_schema
                  AND tc.table_name = kcu.table_name
                 WHERE tc.constraint_type = 'PRIMARY KEY'
                   AND tc.table_schema = $1 AND tc.table_name = $2
                 ORDER BY kcu.ordinal_position",
                &[&schema, &table],
            )
            .await
            .map_err(|error| {
                RemnantError::Adapter(format!("postgres primary key discovery: {error}"))
            })?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    async fn foreign_keys(
        &self,
        client: &Client,
        schema: &str,
        table: &str,
    ) -> Result<Vec<PostgresForeignKey>> {
        let rows = client
            .query(
                "SELECT kcu.column_name, ccu.table_schema, ccu.table_name, ccu.column_name,
                        tc.constraint_name, kcu.ordinal_position
                 FROM information_schema.table_constraints tc
                 JOIN information_schema.key_column_usage kcu
                   ON tc.constraint_name = kcu.constraint_name
                  AND tc.table_schema = kcu.table_schema
                  AND tc.table_name = kcu.table_name
                 JOIN information_schema.constraint_column_usage ccu
                   ON tc.constraint_name = ccu.constraint_name
                  AND tc.constraint_schema = ccu.constraint_schema
                 WHERE tc.constraint_type = 'FOREIGN KEY'
                   AND tc.table_schema = $1 AND tc.table_name = $2
                 ORDER BY tc.constraint_name, kcu.ordinal_position",
                &[&schema, &table],
            )
            .await
            .map_err(|error| {
                RemnantError::Adapter(format!("postgres foreign key discovery: {error}"))
            })?;

        let mut grouped: BTreeMap<String, PostgresForeignKey> = BTreeMap::new();
        for row in rows {
            let constraint: String = row.get(4);
            let entry = grouped
                .entry(constraint)
                .or_insert_with(|| PostgresForeignKey {
                    columns: Vec::new(),
                    referenced_schema: row.get(1),
                    referenced_table: row.get(2),
                    referenced_columns: Vec::new(),
                });
            entry.columns.push(row.get(0));
            entry.referenced_columns.push(row.get(3));
        }
        Ok(grouped.into_values().collect())
    }

    async fn table_snapshot(
        &self,
        client: &Client,
        schema: &str,
        table: &str,
    ) -> Result<PostgresTableSnapshot> {
        let primary_key = self.primary_key(client, schema, table).await?;
        let foreign_keys = self.foreign_keys(client, schema, table).await?;
        let query = format!(
            "SELECT to_jsonb(t) FROM {} t",
            qualified_identifier(schema, table)
        );
        let rows = client.query(&query, &[]).await.map_err(|error| {
            RemnantError::Adapter(format!("postgres row capture {schema}.{table}: {error}"))
        })?;
        Ok(PostgresTableSnapshot {
            schema: schema.to_string(),
            table: table.to_string(),
            primary_key,
            foreign_keys,
            rows: rows.into_iter().map(|row| row.get(0)).collect(),
        })
    }

    fn objects_from_payload(&self, snapshot: &PostgresSnapshot) -> Vec<StateObject> {
        snapshot
            .tables
            .iter()
            .flat_map(|table| {
                table.rows.iter().map(|row| {
                    let id = postgres_object_id(table, row);
                    StateObject::new(
                        id,
                        self.name.clone(),
                        format!("postgres:{}.{}", table.schema, table.table),
                        "postgres_row",
                        format!("{}.{}", table.schema, table.table),
                        row.clone(),
                    )
                })
            })
            .collect()
    }
}

#[async_trait]
impl StateSource for PostgresAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn describe(&self) -> Result<SourceDescription> {
        let client = self.client().await?;
        let tables = self.table_names(&client).await?;
        let mut description = SourceDescription {
            name: self.name.clone(),
            kind: "postgres".to_string(),
            endpoint: redacted_endpoint(&self.url),
            capabilities: vec![
                "discover_schema".to_string(),
                "enumerate_rows".to_string(),
                "snapshot".to_string(),
                "restore_subset".to_string(),
            ],
        };
        description
            .capabilities
            .push(format!("{} tables discovered", tables.len()));
        Ok(description)
    }

    async fn enumerate_state(&self) -> Result<Vec<StateObject>> {
        let snapshot = self.snapshot().await?;
        <Self as StateSource>::objects_from_snapshot(self, &snapshot)
    }

    fn objects_from_snapshot(&self, snapshot: &SourceSnapshot) -> Result<Vec<StateObject>> {
        validate_snapshot(snapshot, &self.name, "postgres")?;
        let payload: PostgresSnapshot = serde_json::from_value(snapshot.payload.clone())
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        Ok(self.objects_from_payload(&payload))
    }

    async fn snapshot(&self) -> Result<SourceSnapshot> {
        let client = self.client().await?;
        let tables = self.table_names(&client).await?;
        let mut captured = Vec::with_capacity(tables.len());
        for (schema, table) in tables {
            captured.push(self.table_snapshot(&client, &schema, &table).await?);
        }
        let payload = serde_json::to_value(PostgresSnapshot { tables: captured })
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        let fingerprint = crate::model::fingerprint(&payload);
        let decoded: PostgresSnapshot = serde_json::from_value(payload.clone())
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        let object_count = self.objects_from_payload(&decoded).len();
        Ok(SourceSnapshot {
            source: self.name.clone(),
            kind: "postgres".to_string(),
            captured_at: Utc::now(),
            fingerprint,
            object_count,
            payload,
        })
    }

    async fn restore(
        &self,
        snapshot: &SourceSnapshot,
        retained: Option<&BTreeSet<String>>,
    ) -> Result<()> {
        validate_snapshot(snapshot, &self.name, "postgres")?;
        let payload: PostgresSnapshot = serde_json::from_value(snapshot.payload.clone())
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        let client = self.client().await?;
        client
            .batch_execute("BEGIN")
            .await
            .map_err(|error| RemnantError::Adapter(format!("postgres begin restore: {error}")))?;

        let result = restore_tables(&client, &payload, retained).await;
        match result {
            Ok(()) => client.batch_execute("COMMIT").await.map_err(|error| {
                RemnantError::Adapter(format!("postgres commit restore: {error}"))
            }),
            Err(error) => {
                let _ = client.batch_execute("ROLLBACK").await;
                Err(error)
            }
        }
    }
}

async fn restore_tables(
    client: &Client,
    snapshot: &PostgresSnapshot,
    retained: Option<&BTreeSet<String>>,
) -> Result<()> {
    let order = dependency_order(snapshot)?;
    for index in (0..order.len()).rev() {
        let table = &snapshot.tables[order[index]];
        let query = format!(
            "DELETE FROM {}",
            qualified_identifier(&table.schema, &table.table)
        );
        client.execute(&query, &[]).await.map_err(|error| {
            RemnantError::Adapter(format!(
                "postgres clear {}.{}: {error}",
                table.schema, table.table
            ))
        })?;
    }

    for index in order {
        let table = &snapshot.tables[index];
        let identifier = qualified_identifier(&table.schema, &table.table);
        let record_type = identifier.clone();
        let query = format!(
            "INSERT INTO {identifier} SELECT * FROM jsonb_populate_record(NULL::{record_type}, $1::jsonb)"
        );
        for row in &table.rows {
            let object_id = postgres_object_id(table, row);
            if retained.is_some_and(|ids| !ids.contains(&object_id)) {
                continue;
            }
            client.execute(&query, &[row]).await.map_err(|error| {
                RemnantError::Adapter(format!("postgres restore {object_id}: {error}"))
            })?;
        }
    }
    Ok(())
}

fn dependency_order(snapshot: &PostgresSnapshot) -> Result<Vec<usize>> {
    let indexes: HashMap<(&str, &str), usize> = snapshot
        .tables
        .iter()
        .enumerate()
        .map(|(index, table)| ((table.schema.as_str(), table.table.as_str()), index))
        .collect();
    let mut outgoing: Vec<HashSet<usize>> = vec![HashSet::new(); snapshot.tables.len()];
    let mut incoming = vec![0usize; snapshot.tables.len()];
    for (index, table) in snapshot.tables.iter().enumerate() {
        for foreign_key in &table.foreign_keys {
            if let Some(&parent) = indexes.get(&(
                foreign_key.referenced_schema.as_str(),
                foreign_key.referenced_table.as_str(),
            )) && outgoing[parent].insert(index)
            {
                incoming[index] += 1;
            }
        }
    }
    let mut queue: VecDeque<usize> = incoming
        .iter()
        .enumerate()
        .filter_map(|(index, &count)| (count == 0).then_some(index))
        .collect();
    let mut order = Vec::with_capacity(snapshot.tables.len());
    while let Some(index) = queue.pop_front() {
        order.push(index);
        for &child in &outgoing[index] {
            incoming[child] -= 1;
            if incoming[child] == 0 {
                queue.push_back(child);
            }
        }
    }
    if order.len() != snapshot.tables.len() {
        return Err(RemnantError::Unsupported(
            "postgres restore cannot order cyclic foreign-key tables safely".to_string(),
        ));
    }
    Ok(order)
}

fn postgres_object_id(table: &PostgresTableSnapshot, row: &Value) -> String {
    let key = if table.primary_key.is_empty() {
        digest_bytes(
            serde_json::to_string(row)
                .expect("row serializes")
                .as_bytes(),
        )
    } else {
        table
            .primary_key
            .iter()
            .map(|column| {
                row.get(column)
                    .map_or_else(|| "<missing>".to_string(), value_key)
            })
            .collect::<Vec<_>>()
            .join("|")
    };
    format!("postgres:{}.{}:{key}", table.schema, table.table)
}

fn value_key(value: &Value) -> String {
    serde_json::to_string(value).expect("json value serializes")
}

fn qualified_identifier(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_identifier(schema), quote_identifier(table))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
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
    let actual_fingerprint = crate::model::fingerprint(&snapshot.payload);
    if actual_fingerprint != snapshot.fingerprint {
        return Err(RemnantError::InvalidSnapshot(format!(
            "fingerprint mismatch for {source}: expected {}, got {actual_fingerprint}",
            snapshot.fingerprint
        )));
    }
    Ok(())
}
