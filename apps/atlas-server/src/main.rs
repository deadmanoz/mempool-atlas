use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use atlas_server::{Store, router};
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "atlas-server")]
#[command(about = "Mempool Atlas ingest and read API")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Migrate {
        #[arg(long, env = "ATLAS_DATABASE")]
        database: PathBuf,
    },
    Serve {
        #[arg(long, env = "ATLAS_DATABASE")]
        database: PathBuf,
        #[arg(long, env = "ATLAS_BIND", default_value = "127.0.0.1:3101")]
        bind: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Migrate { database } => {
            Store::migrate(&database)
                .with_context(|| format!("migrating {}", database.display()))?;
            info!(database = %database.display(), "database migrated");
        }
        Command::Serve { database, bind } => {
            let store = Store::open(&database)
                .with_context(|| format!("opening {}", database.display()))?;
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("binding {bind}"))?;
            info!(%bind, database = %database.display(), "atlas server listening");
            axum::serve(listener, router(store)).await?;
        }
    }
    Ok(())
}
