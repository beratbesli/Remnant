use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

use crate::adapters::postgres::{PostgresSnapshot, postgres_object_id};
use crate::adapters::redis::{RedisSnapshot, redis_object_id_from_base64};
use crate::config::{ProjectConfig, ReproductionEnvironmentValue, SourceConfig};
use crate::error::{RemnantError, Result};
use crate::reducer::ReductionSession;

pub fn export_reproduction_bundle(
    config: &ProjectConfig,
    config_path: &Path,
    session: &ReductionSession,
    output: &Path,
) -> Result<()> {
    let result = session.result.as_ref().ok_or_else(|| {
        RemnantError::Unsupported("only a completed reduction can be exported".to_string())
    })?;
    if !result.failure_reproduced || result.verification_runs != result.verification_successes {
        return Err(RemnantError::Unsupported(
            "only a fully verified minimum state can be exported".to_string(),
        ));
    }
    config.reproduction.as_ref().ok_or_else(|| {
        RemnantError::InvalidConfig(
            "reproduction.app is required to export a portable bundle".to_string(),
        )
    })?;
    if output.exists() {
        return Err(RemnantError::Persistence(format!(
            "refusing to overwrite existing reproduction bundle {}",
            output.display()
        )));
    }
    let context = config
        .resolve_reproduction_context(config_path)
        .ok_or_else(|| {
            RemnantError::InvalidConfig("missing reproduction app context".to_string())
        })?;
    if !context.join("Dockerfile").is_file() {
        return Err(RemnantError::InvalidConfig(format!(
            "reproduction app context {} must contain a Dockerfile",
            context.display()
        )));
    }

    let (postgres_name, postgres_config) = configured_source(config, "postgres")?;
    let (redis_name, redis_config) = configured_source(config, "redis")?;
    let postgres_snapshot = session.baseline.sources.get(postgres_name).ok_or_else(|| {
        RemnantError::InvalidSnapshot("session has no configured postgres snapshot".to_string())
    })?;
    let redis_snapshot = session.baseline.sources.get(redis_name).ok_or_else(|| {
        RemnantError::InvalidSnapshot("session has no configured redis snapshot".to_string())
    })?;
    let postgres: PostgresSnapshot = serde_json::from_value(postgres_snapshot.payload.clone())
        .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
    let redis: RedisSnapshot = serde_json::from_value(redis_snapshot.payload.clone())
        .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;

    fs::create_dir_all(output.join("app")).map_err(io_error("create bundle"))?;
    copy_context(&context, &output.join("app"))?;
    write_schema_dump(postgres_config, output)?;
    fs::write(
        output.join("postgres.sql"),
        postgres_sql(&postgres, &session.retained_ids),
    )
    .map_err(io_error("write postgres state"))?;
    fs::write(
        output.join("redis.snapshot.json"),
        serde_json::to_vec_pretty(&redis).map_err(json_error("serialize redis snapshot"))?,
    )
    .map_err(io_error("write redis snapshot"))?;
    fs::write(
        output.join("redis.restore.resp"),
        redis_restore_protocol(&redis, &session.retained_ids)?,
    )
    .map_err(io_error("write redis restore"))?;
    fs::write(
        output.join("docker-compose.yml"),
        compose_yaml(config, postgres_config, redis_config),
    )
    .map_err(io_error("write compose file"))?;
    fs::write(
        output.join("remnant.yaml.example"),
        example_config(config, postgres_config, redis_config),
    )
    .map_err(io_error("write example config"))?;
    fs::write(
        output.join("reproduce.sh"),
        reproduce_script(config.oracle.failure_exit_code),
    )
    .map_err(io_error("write reproduce script"))?;
    set_executable(&output.join("reproduce.sh"))?;
    fs::write(output.join("README.md"), bundle_readme())
        .map_err(io_error("write bundle readme"))?;
    let manifest = json!({
        "format_version": 1,
        "project": session.project,
        "session_id": session.id,
        "snapshot_fingerprint": session.baseline.fingerprint,
        "sources": session.baseline.sources.iter().map(|(name, source)| json!({
            "name": name,
            "kind": source.kind,
            "target_identity": source.target_identity,
            "fingerprint": source.fingerprint,
        })).collect::<Vec<_>>(),
        "retained_objects": result.retained_count,
        "verification": {"runs": result.verification_runs, "successes": result.verification_successes},
        "images": {"postgres": "postgres:16-alpine", "redis": "redis:7-alpine"},
        "expected_failure_exit_code": config.oracle.failure_exit_code,
    });
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(json_error("serialize manifest"))?,
    )
    .map_err(io_error("write manifest"))?;
    Ok(())
}

fn configured_source<'a>(
    config: &'a ProjectConfig,
    kind: &str,
) -> Result<(&'a str, &'a SourceConfig)> {
    config
        .sources
        .iter()
        .find_map(|(name, source)| (source.kind() == kind).then_some((name.as_str(), source)))
        .ok_or_else(|| {
            RemnantError::InvalidConfig(format!("portable export requires a {kind} source"))
        })
}

fn write_schema_dump(source: &SourceConfig, output: &Path) -> Result<()> {
    let SourceConfig::Postgres { url_env, schema } = source else {
        unreachable!()
    };
    let url = env::var(url_env).map_err(|_| {
        RemnantError::InvalidConfig(format!("environment variable {url_env} is not set"))
    })?;
    let mut command = Command::new("pg_dump");
    command.args(["--schema-only", "--no-owner", "--no-privileges"]);
    if let Some(schema) = schema {
        command.arg(format!("--schema={schema}"));
    }
    let dump = command
        .arg(url)
        .output()
        .map_err(|error| RemnantError::Adapter(format!("start pg_dump: {error}")))?;
    if !dump.status.success() {
        return Err(RemnantError::Adapter(
            "pg_dump could not capture the postgres schema".to_string(),
        ));
    }
    let schema = String::from_utf8(dump.stdout).map_err(|error| {
        RemnantError::Adapter(format!("pg_dump emitted non-UTF-8 schema SQL: {error}"))
    })?;
    fs::write(
        output.join("postgres-schema.sql"),
        sanitize_schema_dump(&schema),
    )
    .map_err(io_error("write postgres schema"))
}

fn sanitize_schema_dump(schema: &str) -> String {
    schema
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !line.starts_with("SET transaction_timeout") && line != "CREATE SCHEMA public;"
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn postgres_sql(
    snapshot: &PostgresSnapshot,
    retained: &std::collections::BTreeSet<String>,
) -> String {
    let tables = snapshot
        .tables
        .iter()
        .map(|table| quote_table(&table.schema, &table.table))
        .collect::<Vec<_>>();
    let mut sql = String::from("SET session_replication_role = replica;\n");
    if !tables.is_empty() {
        sql.push_str(&format!(
            "TRUNCATE {} RESTART IDENTITY CASCADE;\n",
            tables.join(", ")
        ));
    }
    for table in &snapshot.tables {
        let target = quote_table(&table.schema, &table.table);
        for row in &table.rows {
            if retained.contains(&postgres_object_id(table, row)) {
                let encoded = sql_quote(&serde_json::to_string(row).expect("json serializes"));
                sql.push_str(&format!("INSERT INTO {target} SELECT * FROM jsonb_populate_record(NULL::{target}, {encoded}::jsonb);\n"));
            }
        }
    }
    for sequence in &snapshot.sequences {
        let sequence_name = quote_table(&sequence.schema, &sequence.sequence);
        sql.push_str(&format!(
            "SELECT setval({}::regclass, {}, {});\n",
            sql_quote(&sequence_name),
            sequence.last_value,
            sequence.is_called
        ));
    }
    sql.push_str("SET session_replication_role = origin;\n");
    sql
}

fn redis_restore_protocol(
    snapshot: &RedisSnapshot,
    retained: &std::collections::BTreeSet<String>,
) -> Result<Vec<u8>> {
    let mut protocol = command_protocol(&[b"FLUSHDB", b"SYNC"]);
    for entry in &snapshot.entries {
        if !retained.contains(&redis_object_id_from_base64(&entry.key_base64)) {
            continue;
        }
        let key = STANDARD
            .decode(&entry.key_base64)
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        let value = STANDARD
            .decode(&entry.payload_base64)
            .map_err(|error| RemnantError::InvalidSnapshot(error.to_string()))?;
        let ttl = entry.ttl_ms.max(0).to_string();
        protocol.extend(command_protocol(&[
            b"RESTORE",
            &key,
            ttl.as_bytes(),
            &value,
            b"REPLACE",
        ]));
    }
    Ok(protocol)
}

fn command_protocol(parts: &[&[u8]]) -> Vec<u8> {
    let mut result = format!("*{}\r\n", parts.len()).into_bytes();
    for part in parts {
        result.extend(format!("${}\r\n", part.len()).bytes());
        result.extend(*part);
        result.extend(b"\r\n");
    }
    result
}

fn compose_yaml(config: &ProjectConfig, postgres: &SourceConfig, redis: &SourceConfig) -> String {
    let postgres_env = postgres.url_env();
    let redis_env = redis.url_env();
    let command = config
        .reproduction
        .as_ref()
        .expect("validated")
        .app
        .command
        .replace('\'', "''");
    let mut environment = format!(
        "      {postgres_env}: postgresql://remnant:remnant@postgres:5432/remnant\n      {redis_env}: redis://redis:6379/0\n"
    );
    for (name, value) in &config
        .reproduction
        .as_ref()
        .expect("validated")
        .app
        .environment
    {
        let value = match value {
            ReproductionEnvironmentValue::PostgresUrl => {
                "postgresql://remnant:remnant@postgres:5432/remnant"
            }
            ReproductionEnvironmentValue::RedisUrl => "redis://redis:6379/0",
        };
        environment.push_str(&format!("      {name}: {value}\n"));
    }
    format!(
        "services:\n  postgres:\n    image: postgres:16-alpine\n    environment:\n      POSTGRES_USER: remnant\n      POSTGRES_PASSWORD: remnant\n      POSTGRES_DB: remnant\n    healthcheck:\n      test: [\"CMD-SHELL\", \"pg_isready -U remnant -d remnant\"]\n      interval: 2s\n      timeout: 3s\n      retries: 20\n    volumes:\n      - ./postgres-schema.sql:/docker-entrypoint-initdb.d/01-schema.sql:ro\n      - ./postgres.sql:/docker-entrypoint-initdb.d/02-state.sql:ro\n  redis:\n    image: redis:7-alpine\n    healthcheck:\n      test: [\"CMD\", \"redis-cli\", \"ping\"]\n      interval: 2s\n      timeout: 3s\n      retries: 20\n  redis-loader:\n    image: redis:7-alpine\n    depends_on:\n      redis:\n        condition: service_healthy\n    volumes:\n      - ./redis.restore.resp:/bundle/redis.restore.resp:ro\n    command: [\"sh\", \"-ec\", \"redis-cli -h redis --pipe < /bundle/redis.restore.resp\"]\n  app:\n    build: ./app\n    environment:\n{environment}    command: [\"sh\", \"-ec\", '{command}']\n"
    )
}

fn example_config(config: &ProjectConfig, postgres: &SourceConfig, redis: &SourceConfig) -> String {
    format!(
        "version: 1\nproject:\n  name: {}\nsources:\n  postgres:\n    type: postgres\n    url_env: {}\n  redis:\n    type: redis\n    url_env: {}\noracle:\n  command: ./reproduce.sh\n  failure_exit_code: {}\n",
        config.project.name,
        postgres.url_env(),
        redis.url_env(),
        config.oracle.failure_exit_code
    )
}

fn reproduce_script(expected: i32) -> String {
    format!(
        "#!/bin/sh\nset -eu\ncd \"$(dirname \"$0\")\"\ncleanup() {{ docker compose down -v --remove-orphans; }}\ntrap cleanup EXIT\ndocker compose up -d --wait --build postgres redis\ndocker compose run --rm --no-deps redis-loader\nset +e\ndocker compose run --no-deps app\nrun_status=$?\nset -e\napp_id=$(docker compose ps -aq app | tail -n 1)\nif [ -z \"$app_id\" ]; then\n  echo \"reproduction setup failed before the app container was created (compose=$run_status)\" >&2\n  exit 1\nfi\napp_state=$(docker inspect -f '{{{{.State.Status}}}}' \"$app_id\")\napp_exit=$(docker inspect -f '{{{{.State.ExitCode}}}}' \"$app_id\")\nif [ \"$app_state\" != exited ]; then\n  echo \"reproduction app did not exit normally (state=$app_state)\" >&2\n  exit 1\nfi\nif [ \"$app_exit\" -eq {expected} ]; then\n  echo 'expected failure reproduced'\n  exit 0\nfi\necho \"expected failure exit code {expected}, app exited $app_exit\" >&2\nexit 1\n"
    )
}
fn bundle_readme() -> &'static str {
    "# Remnant reproduction bundle\n\nRun `./reproduce.sh`. It starts isolated PostgreSQL and Redis services, loads the verified minimum state, runs the configured application command, and succeeds only when the expected failure exit code is observed.\n"
}
fn quote_table(schema: &str, table: &str) -> String {
    format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        table.replace('"', "\"\"")
    )
}
fn sql_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn json_error(action: &'static str) -> impl FnOnce(serde_json::Error) -> RemnantError {
    move |error| RemnantError::Persistence(format!("{action}: {error}"))
}
fn io_error(action: &'static str) -> impl FnOnce(std::io::Error) -> RemnantError {
    move |error| RemnantError::Persistence(format!("{action}: {error}"))
}
fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(io_error("mark reproduce script executable"))?;
    }
    Ok(())
}

fn copy_context(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source).map_err(io_error("read app context"))? {
        let entry = entry.map_err(io_error("read app context entry"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), ".git" | "target" | "__pycache__") {
            continue;
        }
        if name == ".env" || name.ends_with(".pem") || name.ends_with(".key") || name == "id_rsa" {
            return Err(RemnantError::UnsafeOperation(format!(
                "refusing to copy sensitive app file {name}"
            )));
        }
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
            .map_err(io_error("inspect app context entry"))?
            .is_dir()
        {
            fs::create_dir_all(&target).map_err(io_error("create app directory"))?;
            copy_context(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target).map_err(io_error("copy app file"))?;
        }
    }
    Ok(())
}
