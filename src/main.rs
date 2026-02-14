mod config;
mod indexer;
mod ingest;
mod kiwix;
mod ollama;
mod search;
mod server;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::AppConfig;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "bunker-search")]
#[command(about = "Local full-text search for offline datasets", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Build or update the search index from configured sources.
    Index {
        /// Path to TOML config.
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,

        /// Ignore manifest and rebuild all documents.
        #[arg(long)]
        rebuild: bool,
    },

    /// Serve search API and embeddable widget.
    Serve {
        /// Path to TOML config.
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Index { config, rebuild } => {
            let app_config = AppConfig::from_file(config)?;
            let stats = indexer::index_sources(&app_config, rebuild)?;
            tracing::info!(
                scanned = stats.scanned,
                indexed = stats.indexed,
                skipped = stats.skipped,
                removed = stats.removed,
                "indexing completed"
            );
        }
        Commands::Serve { config } => {
            let app_config = AppConfig::from_file(config)?;
            server::serve(app_config).await?;
        }
    }

    Ok(())
}
