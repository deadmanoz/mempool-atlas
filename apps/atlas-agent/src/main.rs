use std::num::{NonZeroU8, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use atlas_agent::delivery::DeliveryRetryPolicy;
use atlas_agent::source_replica::{SourceReplica, SourceReplicaLimits, SourceReplicaStoragePolicy};
use atlas_agent::state_runtime::{RpcStateConfig, StateRuntimeConfig};
use atlas_model::SourceId;
use clap::{Args, Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "atlas-agent")]
#[command(about = "Node-local Mempool Atlas RPC state replication agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Migrate {
        #[arg(long, env = "ATLAS_AGENT_DATABASE")]
        database: PathBuf,
    },
    Run(Box<RunArgs>),
}

#[derive(Args)]
struct RunArgs {
    #[arg(long, env = "ATLAS_SOURCE_ID")]
    source_id: String,
    #[arg(long, env = "ATLAS_AGENT_DATABASE")]
    database: PathBuf,
    #[arg(long, env = "ATLAS_SERVER_URL")]
    atlas_server_url: String,
    #[arg(long, env = "ATLAS_RPC_URL", default_value = "http://127.0.0.1:8332")]
    rpc_url: String,
    #[arg(long, env = "ATLAS_RPC_USERNAME")]
    rpc_username: String,
    #[arg(long, env = "ATLAS_RPC_PASSWORD")]
    rpc_password: String,
    #[arg(long, env = "ATLAS_RPC_POLL_SECONDS", default_value = "5")]
    rpc_poll_seconds: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_DELIVERY_RETRY_MILLISECONDS",
        default_value = "1000"
    )]
    delivery_retry_milliseconds: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_DELIVERY_RETRY_MAX_MILLISECONDS",
        default_value = "60000"
    )]
    delivery_retry_max_milliseconds: NonZeroU64,
    #[arg(long, env = "ATLAS_MAX_MEMPOOL_ENTRIES", default_value = "200000")]
    max_mempool_entries: NonZeroU64,
    #[arg(long, env = "ATLAS_MAX_DIRTY_MUTATIONS", default_value = "4096")]
    max_dirty_mutations: NonZeroUsize,
    #[arg(long, env = "ATLAS_MAX_DIRTY_BYTES", default_value = "1048576")]
    max_dirty_bytes: NonZeroU64,
    #[arg(long, env = "ATLAS_CHECKPOINT_CHUNK_ENTRIES", default_value = "512")]
    checkpoint_chunk_entries: NonZeroUsize,
    #[arg(long, env = "ATLAS_AGENT_DB_MAX_BYTES", default_value = "1073741824")]
    agent_db_max_bytes: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_AGENT_STORAGE_MAX_BYTES",
        default_value = "1879048192"
    )]
    agent_storage_max_bytes: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_FILESYSTEM_RESERVE_BYTES",
        default_value = "134217728"
    )]
    filesystem_reserve_bytes: NonZeroU64,
    #[arg(long, env = "ATLAS_FILESYSTEM_RESERVE_PERCENT", default_value = "5")]
    filesystem_reserve_percent: NonZeroU8,
    #[arg(
        long,
        env = "ATLAS_AGENT_WAL_RETAINED_BYTES",
        default_value = "67108864"
    )]
    agent_wal_retained_bytes: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_AGENT_WAL_AUTOCHECKPOINT_PAGES",
        default_value = "1000"
    )]
    agent_wal_autocheckpoint_pages: NonZeroU32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // corepc-client logs complete raw JSON-RPC results at trace level. A
    // verbose mempool response is intentionally large, so never let a broad
    // RUST_LOG=trace turn every state poll into an unbounded log write.
    let log_filter = EnvFilter::from_default_env().add_directive(
        "corepc=debug"
            .parse()
            .expect("static corepc logging directive"),
    );
    tracing_subscriber::fmt().with_env_filter(log_filter).init();

    match Cli::parse().command {
        Command::Migrate { database } => {
            atlas_agent::schema::migrate(&database)
                .with_context(|| format!("migrating agent database {}", database.display()))?;
            info!(database = %database.display(), "agent database migrated");
        }
        Command::Run(args) => run(*args).await?,
    }
    Ok(())
}

async fn run(args: RunArgs) -> anyhow::Result<()> {
    let delivery_retry = delivery_retry_policy(&args)?;
    let source_id = SourceId::new(args.source_id).context("validating source ID")?;
    let limits = SourceReplicaLimits {
        max_membership_entries: args.max_mempool_entries.get(),
        max_dirty_mutations: args.max_dirty_mutations.get(),
        max_dirty_bytes: args.max_dirty_bytes.get(),
        checkpoint_chunk_entries: args.checkpoint_chunk_entries.get(),
        max_database_bytes: args.agent_db_max_bytes.get(),
    }
    .validate()
    .context("validating SourceReplica limits")?;
    let storage_policy = SourceReplicaStoragePolicy {
        max_total_sqlite_bytes: args.agent_storage_max_bytes.get(),
        filesystem_reserve_bytes: args.filesystem_reserve_bytes.get(),
        filesystem_reserve_percent: args.filesystem_reserve_percent.get(),
        retained_wal_high_water_bytes: args.agent_wal_retained_bytes.get(),
        wal_autocheckpoint_pages: args.agent_wal_autocheckpoint_pages.get(),
    };
    info!(
        max_membership_entries = limits.max_membership_entries,
        max_database_bytes = limits.max_database_bytes,
        max_total_sqlite_bytes = storage_policy.max_total_sqlite_bytes,
        filesystem_reserve_bytes = storage_policy.filesystem_reserve_bytes,
        filesystem_reserve_percent = storage_policy.filesystem_reserve_percent,
        retained_wal_high_water_bytes = storage_policy.retained_wal_high_water_bytes,
        wal_autocheckpoint_pages = storage_policy.wal_autocheckpoint_pages,
        "configured agent capacity limits"
    );
    let replica =
        SourceReplica::open_with_storage_policy(&args.database, source_id, limits, storage_policy)
            .with_context(|| format!("opening agent database {}", args.database.display()))?;
    let state_endpoint = http_url("Atlas server", &args.atlas_server_url)?
        .join("/api/v1/state")
        .context("building Atlas state endpoint")?
        .to_string();
    let config = StateRuntimeConfig {
        state_endpoint,
        delivery_retry,
        rpc: RpcStateConfig {
            url: http_url("Bitcoin RPC", &args.rpc_url)?.to_string(),
            username: args.rpc_username,
            password: args.rpc_password,
            poll_interval: Duration::from_secs(args.rpc_poll_seconds.get()),
        },
    };
    atlas_agent::state_runtime::run(config, replica).await
}

fn delivery_retry_policy(args: &RunArgs) -> anyhow::Result<DeliveryRetryPolicy> {
    DeliveryRetryPolicy::new(
        Duration::from_millis(args.delivery_retry_milliseconds.get()),
        Duration::from_millis(args.delivery_retry_max_milliseconds.get()),
    )
    .context("validating delivery retry intervals")
}

fn http_url(label: &str, value: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).with_context(|| format!("parsing {label} URL"))?;
    if matches!(url.scheme(), "http" | "https") {
        return Ok(url);
    }
    anyhow::bail!("{label} URL must use http or https")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse_run_args(extra: &[&str]) -> RunArgs {
        let mut arguments = vec![
            "atlas-agent",
            "run",
            "--source-id",
            "core-a",
            "--database",
            "/tmp/atlas-agent-test.db",
            "--atlas-server-url",
            "http://127.0.0.1:8080",
            "--rpc-username",
            "rpc-user",
            "--rpc-password",
            "rpc-password",
        ];
        arguments.extend_from_slice(extra);
        match Cli::try_parse_from(arguments).expect("parse CLI").command {
            Command::Run(args) => *args,
            Command::Migrate { .. } => panic!("expected run command"),
        }
    }

    #[test]
    fn delivery_retry_defaults_to_one_second_initial_and_sixty_second_maximum() {
        let command = Cli::command();
        let run = command.find_subcommand("run").expect("run subcommand");
        let default = |argument_id: &str| {
            run.get_arguments()
                .find(|argument| argument.get_id() == argument_id)
                .and_then(|argument| argument.get_default_values().first())
                .and_then(|value| value.to_str())
                .expect("UTF-8 default")
        };

        assert_eq!(default("delivery_retry_milliseconds"), "1000");
        assert_eq!(default("delivery_retry_max_milliseconds"), "60000");
    }

    #[test]
    fn production_capacity_defaults_are_bounded() {
        let args = parse_run_args(&[]);

        assert_eq!(args.max_mempool_entries.get(), 200_000);
        assert_eq!(args.agent_db_max_bytes.get(), 1024 * 1024 * 1024);
        assert_eq!(args.agent_storage_max_bytes.get(), 1_792 * 1024 * 1024);
        assert_eq!(args.filesystem_reserve_bytes.get(), 128 * 1024 * 1024);
        assert_eq!(args.filesystem_reserve_percent.get(), 5);
        assert_eq!(args.agent_wal_retained_bytes.get(), 64 * 1024 * 1024);
        assert_eq!(args.agent_wal_autocheckpoint_pages.get(), 1_000);
    }

    #[test]
    fn delivery_retry_maximum_must_not_be_below_initial() {
        let args = parse_run_args(&[
            "--delivery-retry-milliseconds",
            "1001",
            "--delivery-retry-max-milliseconds",
            "1000",
        ]);

        let error = delivery_retry_policy(&args).expect_err("reject inverted retry bounds");
        assert!(error.chain().any(|cause| {
            cause
                .to_string()
                .contains("maximum delivery retry interval")
        }));
    }

    #[test]
    fn legacy_capture_mode_flags_are_not_accepted() {
        let result = Cli::try_parse_from([
            "atlas-agent",
            "run",
            "--source-id",
            "core-a",
            "--database",
            "/tmp/atlas-agent-test.db",
            "--atlas-server-url",
            "http://127.0.0.1:8080",
            "--rpc-username",
            "rpc-user",
            "--rpc-password",
            "rpc-password",
            "--mode",
            "full",
        ]);
        assert!(result.is_err());
    }
}
