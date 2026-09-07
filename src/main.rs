mod cli;
mod mcp;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    cli::run(cli::Cli::parse()).await
}
