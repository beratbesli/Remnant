use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use remnant::config::{CONFIG_FILE_NAME, ProjectConfig};
use remnant::oracle::{Oracle, OracleOutcome};

#[derive(Debug, Parser)]
#[command(
    name = "remnant",
    version,
    about = "Reduce a broken application to its smallest persistent state"
)]
struct Cli {
    /// Project configuration file.
    #[arg(long, short, global = true, default_value = CONFIG_FILE_NAME)]
    project: PathBuf,

    /// Emit structured JSON where the command supports it.
    #[arg(long, global = true)]
    json: bool,

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
    /// Validate configuration and local prerequisites.
    Doctor,
    /// Run the configured reproducer and verify the expected failure.
    Verify,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::Init { force } => init(&cli.project, force, cli.json),
        Command::Doctor => doctor(&cli.project, cli.json),
        Command::Verify => verify(&cli.project, cli.json),
    }
}

#[tokio::main]
async fn verify_async(path: &PathBuf, json: bool) -> Result<()> {
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

fn verify(path: &PathBuf, json: bool) -> Result<()> {
    verify_async(path, json)
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
        println!("Set DATABASE_URL and REDIS_URL, then run `remnant doctor`.");
    }
    Ok(())
}

fn doctor(path: &PathBuf, json: bool) -> Result<()> {
    let config = ProjectConfig::load(path)?;
    let missing = config.missing_environment_variables();
    let report = serde_json::json!({
        "config": path,
        "valid": true,
        "project": config.project.name,
        "sources": config.sources.len(),
        "oracle": config.oracle.command,
        "missing_environment_variables": missing,
    });
    if json {
        println!("{report}");
    } else {
        println!("Project: {}", report["project"]);
        println!("Configuration: valid");
        println!("Sources: {}", report["sources"]);
        println!("Oracle: {}", report["oracle"]);
        if missing.is_empty() {
            println!("Environment: all configured source variables are present");
        } else {
            println!("Environment: missing {}", missing.join(", "));
        }
    }
    Ok(())
}
