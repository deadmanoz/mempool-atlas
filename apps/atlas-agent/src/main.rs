use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use atlas_agent::delivery::DeliveryRetryPolicy;
use atlas_agent::outbox::{AgentIdentity, Outbox};
use atlas_agent::runtime::{NatsConfig, P2pPolicy, RpcConfig, RuntimeConfig};
use atlas_model::{SourceId, SourceSessionId};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "atlas-agent")]
#[command(about = "Node-local Mempool Atlas capture and reconciliation agent")]
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
    #[arg(long, env = "ATLAS_AGENT_MODE", value_enum, default_value_t = AgentMode::Full)]
    mode: AgentMode,
    #[arg(long, env = "ATLAS_NATS_ADDRESS", default_value = "127.0.0.1:4222")]
    nats_address: String,
    #[arg(long, env = "ATLAS_NATS_USERNAME")]
    nats_username: Option<String>,
    #[arg(long, env = "ATLAS_NATS_PASSWORD")]
    nats_password: Option<String>,
    #[arg(
        long,
        env = "ATLAS_P2P_POLICY",
        value_enum,
        default_value_t = P2pPolicy::Inbound
    )]
    p2p_policy: P2pPolicy,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum AgentMode {
    Full,
    RpcOnly,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Migrate { database } => {
            Outbox::migrate(&database)
                .with_context(|| format!("migrating agent outbox {}", database.display()))?;
            info!(database = %database.display(), "agent outbox migrated");
        }
        Command::Run(args) => run(*args).await?,
    }
    Ok(())
}

async fn run(args: RunArgs) -> anyhow::Result<()> {
    let delivery_retry = delivery_retry_policy(&args)?;
    let source_id = SourceId::new(args.source_id).context("validating source ID")?;
    let source_session_id =
        SourceSessionId::new(Uuid::new_v4().to_string()).context("creating source session ID")?;
    let identity = AgentIdentity::new(source_id.clone(), source_session_id);
    let outbox = Outbox::open(&args.database, source_id)
        .with_context(|| format!("opening agent outbox {}", args.database.display()))?;
    let atlas_ingest_endpoint = http_url("Atlas server", &args.atlas_server_url)?
        .join("/api/v1/events")
        .context("building Atlas ingest endpoint")?
        .to_string();
    let nats = match args.mode {
        AgentMode::Full => Some(NatsConfig {
            address: args.nats_address,
            username: args.nats_username,
            password: args.nats_password,
            p2p_policy: args.p2p_policy,
        }),
        AgentMode::RpcOnly => None,
    };
    let config = RuntimeConfig {
        identity,
        atlas_ingest_endpoint,
        delivery_retry,
        nats,
        rpc: RpcConfig {
            url: http_url("Bitcoin RPC", &args.rpc_url)?.to_string(),
            username: args.rpc_username,
            password: args.rpc_password,
            poll_interval: Duration::from_secs(args.rpc_poll_seconds.get()),
        },
    };
    atlas_agent::runtime::run(config, outbox).await
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
}
