use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use anyhow::Context;
use atlas_model::SourceId;
use atlas_server::{Store, StoreLimits, router};
use atlas_storage::SqliteStorageLimits;
use clap::{Args, Parser, Subcommand};
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
        #[command(flatten)]
        storage: StorageArgs,
    },
    Serve(Box<ServeArgs>),
}

#[derive(Clone, Debug, Args)]
struct StorageArgs {
    #[arg(long, env = "ATLAS_SERVER_DB_MAX_BYTES", default_value = "3221225472")]
    database_max_bytes: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_SERVER_SQLITE_MAX_BYTES",
        default_value = "3758096384"
    )]
    sqlite_max_bytes: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_SERVER_FILESYSTEM_RESERVE_BYTES",
        default_value = "268435456"
    )]
    filesystem_reserve_bytes: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_SERVER_FILESYSTEM_RESERVE_PERCENT",
        default_value = "5"
    )]
    filesystem_reserve_percent: u8,
    #[arg(
        long,
        env = "ATLAS_SERVER_WAL_HIGH_WATER_BYTES",
        default_value = "67108864"
    )]
    wal_high_water_bytes: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_SERVER_WAL_AUTOCHECKPOINT_PAGES",
        default_value = "1000"
    )]
    wal_autocheckpoint_pages: NonZeroU32,
}

impl StorageArgs {
    fn limits(&self) -> anyhow::Result<SqliteStorageLimits> {
        let limits = SqliteStorageLimits {
            max_database_bytes: self.database_max_bytes.get(),
            max_total_sqlite_bytes: self.sqlite_max_bytes.get(),
            filesystem_reserve_bytes: self.filesystem_reserve_bytes.get(),
            filesystem_reserve_percent: self.filesystem_reserve_percent,
            retained_wal_high_water_bytes: self.wal_high_water_bytes.get(),
            wal_autocheckpoint_pages: self.wal_autocheckpoint_pages.get(),
        };
        limits
            .validate()
            .context("validating SQLite storage limits")?;
        Ok(limits)
    }
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, env = "ATLAS_DATABASE")]
    database: PathBuf,
    #[arg(long, env = "ATLAS_BIND", default_value = "127.0.0.1:3101")]
    bind: SocketAddr,
    #[command(flatten)]
    storage: StorageArgs,
    #[arg(long, env = "ATLAS_SERVER_MAX_SOURCES", default_value = "2")]
    max_sources: NonZeroUsize,
    #[arg(
        long = "max-mempool-entries",
        env = "ATLAS_SERVER_MAX_MEMPOOL_ENTRIES",
        default_value = "200000"
    )]
    max_mempool_entries: NonZeroU64,
    #[arg(long, env = "ATLAS_SERVER_ALLOWED_SOURCE_IDS", value_delimiter = ',')]
    allowed_source_ids: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Migrate { database, storage } => {
            let storage = storage.limits()?;
            Store::migrate_with_limits(&database, &storage)
                .with_context(|| format!("migrating {}", database.display()))?;
            info!(database = %database.display(), "database migrated");
        }
        Command::Serve(args) => {
            let storage = args.storage.limits()?;
            let allowed_source_ids = parse_allowed_source_ids(&args.allowed_source_ids)?;
            validate_serve_exposure(args.bind, allowed_source_ids.as_ref())?;
            let limits = StoreLimits::new(
                storage,
                args.max_sources.get(),
                args.max_mempool_entries.get(),
                allowed_source_ids.clone(),
            )
            .context("validating server store limits")?;
            let store = Store::open_with_limits(&args.database, limits)
                .with_context(|| format!("opening {}", args.database.display()))?;
            let listener = tokio::net::TcpListener::bind(args.bind)
                .await
                .with_context(|| format!("binding {}", args.bind))?;
            info!(
                bind = %args.bind,
                database = %args.database.display(),
                max_database_bytes = storage.max_database_bytes,
                max_total_sqlite_bytes = storage.max_total_sqlite_bytes,
                filesystem_reserve_bytes = storage.filesystem_reserve_bytes,
                filesystem_reserve_percent = storage.filesystem_reserve_percent,
                retained_wal_high_water_bytes = storage.retained_wal_high_water_bytes,
                wal_autocheckpoint_pages = storage.wal_autocheckpoint_pages,
                max_sources = args.max_sources.get(),
                max_mempool_entries = args.max_mempool_entries.get(),
                allowed_source_ids = ?allowed_source_ids,
                "atlas server listening with bounded storage"
            );
            axum::serve(listener, router(store)).await?;
        }
    }
    Ok(())
}

fn validate_serve_exposure(
    bind: SocketAddr,
    allowed_source_ids: Option<&BTreeSet<SourceId>>,
) -> anyhow::Result<()> {
    if !bind.ip().is_loopback() && allowed_source_ids.is_none() {
        anyhow::bail!(
            "non-loopback bind {bind} requires an exact source allowlist via ATLAS_SERVER_ALLOWED_SOURCE_IDS or --allowed-source-ids"
        );
    }
    Ok(())
}

fn parse_allowed_source_ids(values: &[String]) -> anyhow::Result<Option<BTreeSet<SourceId>>> {
    if values.is_empty() {
        return Ok(None);
    }
    let mut allowed = BTreeSet::new();
    for value in values {
        let source = SourceId::new(value.clone())
            .with_context(|| format!("validating allowed source ID {value:?}"))?;
        if !allowed.insert(source) {
            anyhow::bail!("allowed source ID {value:?} appears more than once");
        }
    }
    Ok(Some(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_args(extra: &[&str]) -> ServeArgs {
        let mut arguments = vec!["atlas-server", "serve", "--database", "/tmp/atlas.db"];
        arguments.extend_from_slice(extra);
        match Cli::try_parse_from(arguments)
            .expect("parse server CLI")
            .command
        {
            Command::Serve(args) => *args,
            Command::Migrate { .. } => panic!("expected serve command"),
        }
    }

    #[test]
    fn production_defaults_fit_the_four_gib_server_image() {
        let args = serve_args(&[]);
        let storage = args.storage.limits().expect("storage limits");

        assert_eq!(storage.max_database_bytes, 3 * 1024 * 1024 * 1024);
        assert_eq!(storage.max_total_sqlite_bytes, 3_584 * 1024 * 1024);
        assert_eq!(storage.filesystem_reserve_bytes, 256 * 1024 * 1024);
        assert_eq!(storage.filesystem_reserve_percent, 5);
        assert_eq!(storage.retained_wal_high_water_bytes, 64 * 1024 * 1024);
        assert_eq!(storage.wal_autocheckpoint_pages, 1_000);
        assert_eq!(args.max_sources.get(), 2);
        assert_eq!(args.max_mempool_entries.get(), 200_000);
        assert!(args.allowed_source_ids.is_empty());
    }

    #[test]
    fn exact_source_allowlist_is_comma_delimited_and_rejects_duplicates() {
        let args = serve_args(&["--allowed-source-ids", "core,knots,libre-relay"]);
        let allowed = parse_allowed_source_ids(&args.allowed_source_ids)
            .expect("allowlist")
            .expect("configured allowlist");
        assert_eq!(
            allowed.iter().map(SourceId::as_str).collect::<Vec<_>>(),
            vec!["core", "knots", "libre-relay"]
        );

        assert!(parse_allowed_source_ids(&["core".to_owned(), "core".to_owned()]).is_err());
    }

    #[test]
    fn non_loopback_bind_requires_an_exact_source_allowlist() {
        for bind in ["0.0.0.0:3101", "192.0.2.1:3101", "[::]:3101"] {
            let args = serve_args(&["--bind", bind]);
            let allowed = parse_allowed_source_ids(&args.allowed_source_ids).expect("allowlist");
            assert!(validate_serve_exposure(args.bind, allowed.as_ref()).is_err());
        }

        for bind in ["127.0.0.1:3101", "[::1]:3101"] {
            let args = serve_args(&["--bind", bind]);
            let allowed = parse_allowed_source_ids(&args.allowed_source_ids).expect("allowlist");
            validate_serve_exposure(args.bind, allowed.as_ref())
                .expect("loopback development bind");
        }

        let args = serve_args(&[
            "--bind",
            "0.0.0.0:3101",
            "--allowed-source-ids",
            "core,knots",
        ]);
        let allowed = parse_allowed_source_ids(&args.allowed_source_ids)
            .expect("allowlist")
            .expect("exact allowlist");
        validate_serve_exposure(args.bind, Some(&allowed)).expect("restricted external bind");
    }
}
