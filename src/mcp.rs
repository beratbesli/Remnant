use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use remnant::adapters::from_config;
use remnant::config::ProjectConfig;
use remnant::graph::StateGraph;
use remnant::oracle::Oracle;
use remnant::persistence::SessionStore;
use remnant::reducer::{ReductionEngine, ReductionSession};
use remnant::report::ReductionReport;
use remnant::safety::ensure_mutation_allowed;

pub async fn serve(project: PathBuf) -> Result<()> {
    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = io::BufWriter::new(io::stdout());
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {"code": -32700, "message": error.to_string()}
                    }),
                )
                .await?;
                continue;
            }
        };
        if request.get("id").is_none() {
            continue;
        }
        let id = request["id"].clone();
        let response = match handle_request(&project, &request).await {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32000, "message": error.to_string()}
            }),
        };
        write_response(&mut stdout, &response).await?;
    }
    Ok(())
}

async fn write_response(stdout: &mut io::BufWriter<io::Stdout>, response: &Value) -> Result<()> {
    stdout
        .write_all(serde_json::to_string(response)?.as_bytes())
        .await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

async fn handle_request(project: &PathBuf, request: &Value) -> Result<Value> {
    let method = request["method"].as_str().unwrap_or_default();
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "remnant", "version": env!("CARGO_PKG_VERSION")}
        })),
        "notifications/initialized" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tool_definitions()})),
        "tools/call" => {
            let name = request["params"]["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("tools/call requires params.name"))?;
            let arguments = request["params"]
                .get("arguments")
                .cloned()
                .unwrap_or(json!({}));
            let value = call_tool(project, name, &arguments).await?;
            Ok(json!({
                "content": [{"type": "text", "text": serde_json::to_string_pretty(&value)?}],
                "structuredContent": value
            }))
        }
        direct if direct.starts_with("remnant/") => {
            let value = call_tool(
                project,
                direct.trim_start_matches("remnant/"),
                &request["params"],
            )
            .await?;
            Ok(json!({"result": value}))
        }
        _ => Err(anyhow::anyhow!("unsupported MCP method: {method}")),
    }
}

async fn call_tool(project: &PathBuf, name: &str, arguments: &Value) -> Result<Value> {
    let config = ProjectConfig::load(project)?;
    let store = SessionStore::new(config.resolve_state_dir(project));
    match name {
        "get_project_status" => Ok(json!({
            "project": config.project.name,
            "sources": config.sources.keys().collect::<Vec<_>>(),
            "sessions": store.list()?,
            "safety": config.safety,
        })),
        "doctor" => Ok(json!({
            "valid": true,
            "project": config.project.name,
            "missing_environment_variables": config.missing_environment_variables(),
            "oracle_command": config.oracle.command,
        })),
        "verify_failure" => {
            let result = Oracle::new(config.oracle).run().await?;
            Ok(serde_json::to_value(result)?)
        }
        "list_state_sources" => {
            let sources = from_config(&config)?;
            let mut descriptions = Vec::new();
            for source in sources {
                descriptions.push(serde_json::to_value(source.describe().await?)?);
            }
            Ok(json!({"sources": descriptions}))
        }
        "inspect_state" => {
            let session = load_session(&store, arguments)?;
            Ok(json!({
                "session": session.id,
                "status": session.status,
                "objects": session.objects,
                "retained_ids": session.retained_ids,
                "graph": StateGraph::build(&session.objects),
            }))
        }
        "start_reduction" => {
            ensure_mutation_allowed(&config, false)?;
            let sources = from_config(&config)?;
            let oracle = Oracle::new(config.oracle.clone());
            let engine =
                ReductionEngine::new(&sources, &oracle, &store, config.reduction.max_experiments);
            let session = engine
                .begin(config.project.name, config.reduction.strategy)
                .await?;
            Ok(json!({
                "session_id": session.id,
                "status": session.status,
                "objects": session.objects.len(),
                "snapshot_fingerprint": session.baseline.fingerprint,
            }))
        }
        "run_reduction" => {
            ensure_mutation_allowed(&config, false)?;
            let session_id = required_session_id(arguments)?;
            let sources = from_config(&config)?;
            let oracle = Oracle::new(config.oracle.clone());
            let mut session = store.load(&session_id)?;
            let engine =
                ReductionEngine::new(&sources, &oracle, &store, config.reduction.max_experiments);
            engine.run(&mut session).await?;
            Ok(serde_json::to_value(session.result)?)
        }
        "get_reduction_status" => {
            let session = load_session(&store, arguments)?;
            Ok(serde_json::to_value(session.summary())?)
        }
        "generate_report" => {
            let session = load_session(&store, arguments)?;
            Ok(serde_json::to_value(ReductionReport::from_session(
                &session,
            )?)?)
        }
        _ => Err(anyhow::anyhow!("unsupported Remnant tool: {name}")),
    }
}

fn load_session(store: &SessionStore, arguments: &Value) -> Result<ReductionSession> {
    let session_id = required_session_id(arguments)?;
    Ok(store.load(&session_id)?)
}

fn required_session_id(arguments: &Value) -> Result<String> {
    arguments["session_id"]
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("session_id is required"))
}

fn tool_definitions() -> Vec<Value> {
    let no_args = json!({"type": "object", "properties": {}, "additionalProperties": false});
    let session_args = json!({
        "type": "object",
        "properties": {"session_id": {"type": "string"}},
        "required": ["session_id"],
        "additionalProperties": false
    });
    vec![
        json!({"name": "get_project_status", "description": "Return project and persisted session status.", "inputSchema": no_args}),
        json!({"name": "doctor", "description": "Validate Remnant configuration without mutating state.", "inputSchema": no_args}),
        json!({"name": "verify_failure", "description": "Run the configured failure oracle.", "inputSchema": no_args}),
        json!({"name": "list_state_sources", "description": "Describe configured state sources and capabilities.", "inputSchema": no_args}),
        json!({"name": "inspect_state", "description": "Inspect captured objects and deterministic relationship hypotheses.", "inputSchema": session_args}),
        json!({"name": "start_reduction", "description": "Verify the baseline and capture a resumable reduction session.", "inputSchema": no_args}),
        json!({"name": "run_reduction", "description": "Run or resume experiments for a captured session.", "inputSchema": session_args}),
        json!({"name": "get_reduction_status", "description": "Read persisted reduction progress.", "inputSchema": session_args}),
        json!({"name": "generate_report", "description": "Generate the structured reduction report.", "inputSchema": session_args}),
    ]
}
