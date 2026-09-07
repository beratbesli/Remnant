use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::process::Command as ProcessCommand;

use remnant::adapters::from_config;
use remnant::config::{CONFIG_FILE_NAME, ProjectConfig};
use remnant::graph::StateGraph;
use remnant::model::group_objects;
use remnant::oracle::{Oracle, OracleOutcome};
use remnant::persistence::SessionStore;
use remnant::reducer::{ReductionEngine, ReductionSession};
use remnant::report::ReductionReport;
use remnant::safety::ensure_mutation_allowed;

#[derive(Debug, Parser)]
#[command(
    name = "remnant",
    version,
    about = "Reduce a broken application to its smallest persistent state"
)]
pub struct Cli {
    /// Project configuration file.
    #[arg(long, short, global = true, default_value = CONFIG_FILE_NAME)]
    project: PathBuf,

    /// Emit structured JSON where the command supports it.
    #[arg(long, global = true)]
    json: bool,

    /// Explicitly permit mutating targets that are not recognized as local.
    #[arg(long, global = true)]
    allow_non_local: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write a starter project configuration.
    Init {
        /// Do not overwrite an existing configuration.
        #[arg(long)]
        force: bool,
    },
    /// Validate configuration, configured sources, and local prerequisites.
    Doctor,
    /// Run the configured reproducer and verify the expected failure.
    Verify,
    /// Verify the failure and capture a persisted baseline session.
    Capture,
    /// Show the state objects and groups captured in a session.
    Inspect {
        /// Session id. Defaults to the newest session.
        session: Option<String>,
    },
    /// Start a new reduction from the current environment.
    Reduce,
    /// Continue a previously captured or interrupted reduction.
    Resume { session: String },
    /// Show reduction progress.
    Status {
        /// Session id. Omit to list all sessions.
        session: Option<String>,
    },
    /// Render a completed reduction report.
    Report {
        /// Session id. Defaults to the newest session.
        session: Option<String>,
        /// Optional JSON output file.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Restore a session's retained state into the configured environment.
    Replay {
        /// Session id. Defaults to the newest session.
        session: Option<String>,
        /// Required acknowledgement for the destructive restore.
        #[arg(long)]
        confirm: bool,
    },
    /// Serve the controlled MCP-compatible JSON-RPC interface over stdio.
    Mcp,
}

pub async fn run(cli: Cli) -> Result<()> {
    match &cli.command {
        Command::Init { force } => init(&cli.project, *force, cli.json),
        Command::Doctor => doctor(&cli.project, cli.json).await,
        Command::Verify => verify(&cli.project, cli.json).await,
        Command::Capture => capture(&cli).await,
        Command::Inspect { session } => inspect(&cli, session.clone()).await,
        Command::Reduce => reduce(&cli).await,
        Command::Resume { session } => resume(&cli, session).await,
        Command::Status { session } => status(&cli, session.clone()),
        Command::Report { session, output } => report(&cli, session.clone(), output.clone()),
        Command::Replay { session, confirm } => replay(&cli, session.clone(), *confirm).await,
        Command::Mcp => crate::mcp::serve(cli.project.clone()).await,
    }
}

fn init(path: &PathBuf, force: bool, json: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force only if replacing it is intentional",
            path.display()
        );
    }
    let config = ProjectConfig::default();
    config.save(path)?;
    if json {
        println!("{}", serde_json::json!({"config": path, "created": true}));
    } else {
        println!("Created {}", path.display());
        println!("Set DATABASE_URL and REDIS_URL, then run remnant doctor.");
    }
    Ok(())
}

async fn doctor(path: &PathBuf, json: bool) -> Result<()> {
    let config = ProjectConfig::load(path)?;
    let missing = config.missing_environment_variables();
    let docker_available = ProcessCommand::new("docker")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    let mut sources_report = Vec::new();
    if missing.is_empty() {
        match from_config(&config) {
            Ok(sources) => {
                for source in sources {
                    match source.describe().await {
                        Ok(description) => sources_report.push(serde_json::json!({
                            "name": description.name,
                            "kind": description.kind,
                            "endpoint": description.endpoint,
                            "reachable": true,
                            "capabilities": description.capabilities,
                        })),
                        Err(error) => sources_report.push(serde_json::json!({
                            "name": source.name(),
                            "reachable": false,
                            "error": error.to_string(),
                        })),
                    }
                }
            }
            Err(error) => sources_report.push(serde_json::json!({"error": error.to_string()})),
        }
    }
    let report = serde_json::json!({
        "config": path,
        "valid": true,
        "project": config.project.name,
        "sources": sources_report,
        "missing_environment_variables": missing,
        "docker_available": docker_available,
        "oracle_command": config.oracle.command,
        "state_dir": config.resolve_state_dir(path),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Project: {}", report["project"]);
        println!("Configuration: valid");
        println!(
            "Docker: {}",
            if docker_available {
                "available"
            } else {
                "not found"
            }
        );
        println!("Oracle: {}", report["oracle_command"]);
        if missing.is_empty() {
            println!("Environment: configured source variables are present");
        } else {
            println!("Environment: missing {}", missing.join(", "));
        }
        for source in report["sources"].as_array().into_iter().flatten() {
            println!(
                "Source {}: {}",
                source["name"],
                if source["reachable"].as_bool().unwrap_or(false) {
                    "reachable"
                } else {
                    "unreachable"
                }
            );
            if let Some(error) = source["error"].as_str() {
                println!("  {error}");
            }
        }
    }
    Ok(())
}

async fn verify(path: &PathBuf, json: bool) -> Result<()> {
    let config = ProjectConfig::load(path)?;
    let result = Oracle::new(config.oracle).run().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Oracle: {}", result.command);
        println!("Outcome: {:?}", result.outcome);
        println!("Duration: {}ms", result.duration_ms);
        if !result.stdout.is_empty() {
            println!("stdout:\n{}", result.stdout.trim_end());
        }
        if !result.stderr.is_empty() {
            println!("stderr:\n{}", result.stderr.trim_end());
        }
    }
    if result.outcome != OracleOutcome::FailureReproduced {
        anyhow::bail!("configured oracle did not reproduce the expected failure");
    }
    Ok(())
}

async fn capture(cli: &Cli) -> Result<()> {
    let config = load_config(&cli.project)?;
    ensure_mutation_allowed(&config, cli.allow_non_local)?;
    let sources = from_config(&config)?;
    let oracle = Oracle::new(config.oracle.clone());
    let store = SessionStore::new(config.resolve_state_dir(&cli.project));
    let engine = ReductionEngine::new(&sources, &oracle, &store, config.reduction.max_experiments);
    let session = engine
        .begin(config.project.name, config.reduction.strategy)
        .await?;
    print_session_created(&session, cli.json)
}

async fn reduce(cli: &Cli) -> Result<()> {
    let config = load_config(&cli.project)?;
    ensure_mutation_allowed(&config, cli.allow_non_local)?;
    let sources = from_config(&config)?;
    let oracle = Oracle::new(config.oracle.clone());
    let store = SessionStore::new(config.resolve_state_dir(&cli.project));
    let engine = ReductionEngine::new(&sources, &oracle, &store, config.reduction.max_experiments);
    let mut session = engine
        .begin(config.project.name, config.reduction.strategy)
        .await?;
    engine.run(&mut session).await?;
    print_result(&session, cli.json)
}

async fn resume(cli: &Cli, session_id: &str) -> Result<()> {
    let config = load_config(&cli.project)?;
    ensure_mutation_allowed(&config, cli.allow_non_local)?;
    let sources = from_config(&config)?;
    let oracle = Oracle::new(config.oracle.clone());
    let store = SessionStore::new(config.resolve_state_dir(&cli.project));
    let mut session = store.load(session_id)?;
    let engine = ReductionEngine::new(&sources, &oracle, &store, config.reduction.max_experiments);
    engine.run(&mut session).await?;
    print_result(&session, cli.json)
}

async fn inspect(cli: &Cli, session_id: Option<String>) -> Result<()> {
    let store = store_for(cli)?;
    let session = load_selected(&store, session_id.as_deref())?;
    let groups = group_objects(&session.objects);
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session": session.id,
                "status": session.status,
                "groups": groups,
                "objects": session.objects,
                "retained_ids": session.retained_ids,
                "graph": StateGraph::build(&session.objects),
            }))?
        );
    } else {
        println!("Session: {} ({:?})", session.id, session.status);
        println!(
            "Objects: {} total, {} retained",
            session.objects.len(),
            session.retained_ids.len()
        );
        for group in groups {
            let retained = group
                .object_ids
                .iter()
                .filter(|id| session.retained_ids.contains(*id))
                .count();
            println!(
                "  {}: {}/{} retained",
                group.id,
                retained,
                group.object_ids.len()
            );
        }
        for object in session
            .objects
            .iter()
            .filter(|object| session.retained_ids.contains(&object.id))
        {
            println!("  [{}] {}", object.source, object.label);
        }
    }
    Ok(())
}

fn status(cli: &Cli, session_id: Option<String>) -> Result<()> {
    let store = store_for(cli)?;
    if let Some(session_id) = session_id {
        let session = store.load(&session_id)?;
        print_summary(&session, cli.json)?;
    } else {
        let summaries = store
            .list()?
            .into_iter()
            .map(|id| store.summary(&id))
            .collect::<remnant::Result<Vec<_>>>()?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&summaries)?);
        } else if summaries.is_empty() {
            println!("No Remnant sessions found.");
        } else {
            for summary in summaries {
                println!(
                    "{} {:?}: {}/{} objects, {} experiments",
                    summary.id,
                    summary.status,
                    summary.retained_count,
                    summary.original_count,
                    summary.experiments
                );
            }
        }
    }
    Ok(())
}

fn report(cli: &Cli, session_id: Option<String>, output: Option<PathBuf>) -> Result<()> {
    let store = store_for(cli)?;
    let session = load_selected(&store, session_id.as_deref())?;
    let report = ReductionReport::from_session(&session)?;
    if let Some(path) = output {
        report.write_json(path)?;
    }
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render_text());
    }
    Ok(())
}

async fn replay(cli: &Cli, session_id: Option<String>, confirm: bool) -> Result<()> {
    if !confirm {
        anyhow::bail!(
            "replay modifies configured state; pass --confirm after verifying the target"
        );
    }
    let config = load_config(&cli.project)?;
    ensure_mutation_allowed(&config, cli.allow_non_local)?;
    let sources = from_config(&config)?;
    let store = SessionStore::new(config.resolve_state_dir(&cli.project));
    let session = load_selected(&store, session_id.as_deref())?;
    remnant::snapshot::restore_sources(&sources, &session.baseline, Some(&session.retained_ids))
        .await?;
    if cli.json {
        println!(
            "{}",
            serde_json::json!({"session": session.id, "replayed": true, "retained": session.retained_ids.len()})
        );
    } else {
        println!(
            "Replayed {} retained objects from {}.",
            session.retained_ids.len(),
            session.id
        );
    }
    Ok(())
}

fn load_config(path: &PathBuf) -> Result<ProjectConfig> {
    Ok(ProjectConfig::load(path)?)
}

fn store_for(cli: &Cli) -> Result<SessionStore> {
    let config = load_config(&cli.project)?;
    Ok(SessionStore::new(config.resolve_state_dir(&cli.project)))
}

fn load_selected(store: &SessionStore, requested: Option<&str>) -> Result<ReductionSession> {
    let session_id = match requested {
        Some(id) => id.to_string(),
        None => store
            .list()?
            .into_iter()
            .next_back()
            .ok_or_else(|| anyhow::anyhow!("no Remnant sessions found"))?,
    };
    Ok(store.load(&session_id)?)
}

fn print_session_created(session: &ReductionSession, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "session": session.id,
                "status": session.status,
                "objects": session.objects.len(),
                "snapshot": session.baseline.fingerprint
            })
        );
    } else {
        println!("Captured session {}", session.id);
        println!("Objects: {}", session.objects.len());
        println!("Snapshot: {}", session.baseline.fingerprint);
        println!("Run remnant resume {} to reduce it.", session.id);
    }
    Ok(())
}

fn print_result(session: &ReductionSession, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&session.result)?);
    } else if let Some(result) = &session.result {
        println!(
            "Reduction complete: {} -> {} objects",
            result.original_count, result.retained_count
        );
        println!("Failure reproduced: {}", result.failure_reproduced);
        println!("Minimality: {}", result.minimality);
        println!("Experiments: {}", result.experiments);
        println!("Report: remnant report {}", session.id);
    }
    Ok(())
}

fn print_summary(session: &ReductionSession, json: bool) -> Result<()> {
    let summary = session.summary();
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("Session: {}", summary.id);
        println!("Status: {:?}", summary.status);
        println!(
            "Objects: {} original, {} retained",
            summary.original_count, summary.retained_count
        );
        println!("Experiments: {}", summary.experiments);
        if let Some(error) = &session.last_error {
            println!("Last error: {error}");
        }
    }
    Ok(())
}
