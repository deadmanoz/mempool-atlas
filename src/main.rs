use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use mempool_atlas::model::{validate_source_id, validate_source_label};
use mempool_atlas::{
    AtlasRuntime, AtlasSource, ClassificationLimits, ClassificationPipeline,
    MAX_CONFIGURED_SOURCES, MembershipTimeouts, RpcClient, SourceRegistry, SourceRuntime, router,
};
use serde::Deserialize;
use tracing::info;
use tracing_subscriber::EnvFilter;

const MAX_SOURCE_CONFIG_BYTES: usize = 64 * 1024;
const MAX_TOTAL_CLASSIFICATION_CACHE_MIB: usize = 512;

#[derive(Debug, Parser)]
#[command(name = "mempool-atlas")]
#[command(about = "Periodically snapshot Bitcoin nodes for the Mempool Atlas website")]
struct Cli {
    #[arg(long, env = "ATLAS_BIND", default_value = "127.0.0.1:3101")]
    bind: SocketAddr,
    #[arg(long, env = "ATLAS_WEB_ROOT", default_value = "web/dist")]
    web_root: PathBuf,
    #[arg(long, env = "ATLAS_SOURCES_FILE")]
    sources_file: PathBuf,
    #[arg(long, env = "ATLAS_CREDENTIALS_DIRECTORY")]
    credentials_directory: PathBuf,
    #[arg(long, env = "ATLAS_POLL_SECONDS", default_value = "900")]
    poll_seconds: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_MEMBERSHIP_RPC_TIMEOUT_SECONDS",
        default_value = "120"
    )]
    membership_rpc_timeout_seconds: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_MEMBERSHIP_COLLECTION_BUDGET_SECONDS",
        default_value = "300"
    )]
    membership_collection_budget_seconds: NonZeroU64,
    #[arg(long, env = "ATLAS_MAX_MEMPOOL_ENTRIES", default_value = "200000")]
    max_mempool_entries: NonZeroU64,
    #[arg(
        long,
        env = "ATLAS_CLASSIFICATION_SLICE_ENTRIES",
        default_value = "2048"
    )]
    classification_slice_entries: NonZeroUsize,
    #[arg(long, env = "ATLAS_CLASSIFICATION_RPC_LANES", default_value = "4")]
    classification_rpc_lanes: NonZeroUsize,
    #[arg(
        long,
        env = "ATLAS_CLASSIFICATION_TOTAL_CACHE_MIB",
        default_value = "256"
    )]
    classification_total_cache_mib: NonZeroUsize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcesFile {
    sources: Vec<SourceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceConfig {
    source_id: String,
    source_label: String,
    rpc_url: String,
    rpc_username: String,
    rpc_password_credential: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    validate_bind(cli.bind)?;
    let source_configs = read_sources_file(&cli.sources_file)
        .with_context(|| format!("reading {}", cli.sources_file.display()))?;
    let poll_interval = Duration::from_secs(cli.poll_seconds.get());
    let membership_timeouts = MembershipTimeouts::new(
        Duration::from_secs(cli.membership_rpc_timeout_seconds.get()),
        Duration::from_secs(cli.membership_collection_budget_seconds.get()),
    )
    .context("validating membership RPC time budgets")?;
    let total_cache_mib = cli.classification_total_cache_mib.get();
    if total_cache_mib > MAX_TOTAL_CLASSIFICATION_CACHE_MIB {
        bail!(
            "classification total cache size {total_cache_mib} MiB exceeds the supported maximum {MAX_TOTAL_CLASSIFICATION_CACHE_MIB} MiB"
        );
    }
    let total_cache_bytes = total_cache_mib
        .checked_mul(1024 * 1024)
        .context("classification cache size overflows this platform")?;
    let per_source_cache_bytes = total_cache_bytes
        .checked_div(source_configs.len())
        .context("at least one source is required")?;
    let mut configured_sources = Vec::with_capacity(source_configs.len());
    for source_config in source_configs {
        let credential_path = cli
            .credentials_directory
            .join(&source_config.rpc_password_credential);
        let password = read_secret(&credential_path)
            .with_context(|| format!("reading {}", credential_path.display()))?;
        let runtime = Arc::new(
            SourceRuntime::new(
                source_config.source_id,
                source_config.source_label,
                poll_interval,
            )
            .context("validating source configuration")?,
        );
        let classification_limits = ClassificationLimits::new(
            cli.classification_slice_entries.get(),
            cli.classification_rpc_lanes.get(),
            per_source_cache_bytes,
        )
        .context("validating classification limits")?;
        let rpc = RpcClient::with_timeouts(
            &source_config.rpc_url,
            source_config.rpc_username.clone(),
            password.clone(),
            cli.max_mempool_entries.get(),
            membership_timeouts,
        )
        .context("creating Bitcoin RPC client")?;
        let classification = ClassificationPipeline::new(
            &source_config.rpc_url,
            source_config.rpc_username,
            password,
            classification_limits,
        )
        .context("creating classification pipeline")?;
        configured_sources.push(AtlasSource::new(runtime, rpc, classification));
    }
    let atlas = Arc::new(
        AtlasRuntime::new(configured_sources, poll_interval)
            .context("creating bounded source coordinator")?,
    );
    let registry =
        SourceRegistry::new(atlas.source_runtimes()).context("creating source registry")?;
    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;

    info!(
        bind = %cli.bind,
        source_count = atlas.source_runtimes().len(),
        poll_seconds = cli.poll_seconds.get(),
        membership_rpc_timeout_seconds = cli.membership_rpc_timeout_seconds.get(),
        membership_collection_budget_seconds = cli.membership_collection_budget_seconds.get(),
        max_mempool_entries = cli.max_mempool_entries.get(),
        classification_slice_entries = cli.classification_slice_entries.get(),
        classification_rpc_lanes = cli.classification_rpc_lanes.get(),
        classification_total_cache_mib = total_cache_mib,
        classification_cache_bytes_per_source = per_source_cache_bytes,
        "Mempool Atlas snapshot service listening"
    );

    let runtime_task = tokio::spawn(Arc::clone(&atlas).run());
    let result = axum::serve(listener, router(registry, cli.web_root))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    runtime_task.abort();
    result.context("serving Mempool Atlas API")
}

fn read_sources_file(path: &Path) -> anyhow::Result<Vec<SourceConfig>> {
    let contents = std::fs::read(path)?;
    if contents.len() > MAX_SOURCE_CONFIG_BYTES {
        bail!(
            "source configuration is {} bytes, exceeding the {}-byte limit",
            contents.len(),
            MAX_SOURCE_CONFIG_BYTES
        );
    }
    parse_sources_file(&contents)
}

fn parse_sources_file(contents: &[u8]) -> anyhow::Result<Vec<SourceConfig>> {
    let parsed: SourcesFile = serde_json::from_slice(contents)?;
    if parsed.sources.is_empty() {
        bail!("source configuration must contain at least one source");
    }
    if parsed.sources.len() > MAX_CONFIGURED_SOURCES {
        bail!(
            "source configuration contains {} sources, exceeding the supported maximum {}",
            parsed.sources.len(),
            MAX_CONFIGURED_SOURCES
        );
    }
    let mut source_ids = BTreeSet::new();
    for source in &parsed.sources {
        validate_source_id(&source.source_id)?;
        validate_source_label(&source.source_label)?;
        if !source_ids.insert(source.source_id.clone()) {
            bail!("source {:?} is configured more than once", source.source_id);
        }
        if source.rpc_username.is_empty()
            || source.rpc_username.len() > 128
            || source.rpc_username.contains(['\r', '\n'])
        {
            bail!("RPC username must contain 1 to 128 characters on one line");
        }
        validate_credential_name(&source.rpc_password_credential)?;
    }
    Ok(parsed.sources)
}

fn validate_credential_name(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("RPC password credential {value:?} must be a safe 1 to 128 character file name");
    }
    Ok(())
}

fn validate_bind(bind: SocketAddr) -> anyhow::Result<()> {
    if !bind.ip().is_loopback() {
        bail!("Atlas only supports loopback binding; refusing {bind}");
    }
    Ok(())
}

fn read_secret(path: &Path) -> anyhow::Result<String> {
    let contents = std::fs::read_to_string(path)?;
    parse_secret(contents)
}

fn parse_secret(mut contents: String) -> anyhow::Result<String> {
    while contents.ends_with(['\r', '\n']) {
        contents.pop();
    }
    if contents.is_empty() {
        bail!("RPC password file is empty");
    }
    if contents.contains(['\r', '\n']) {
        bail!("RPC password file must contain exactly one line");
    }
    Ok(contents)
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c()
            .await
            .expect("installing Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing terminate handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments<'a>(extra: &'a [&'a str]) -> Vec<&'a str> {
        let mut arguments = vec![
            "mempool-atlas",
            "--sources-file",
            "/etc/mempool-atlas/sources.json",
            "--credentials-directory",
            "/run/credentials/mempool-atlas.service",
        ];
        arguments.extend_from_slice(extra);
        arguments
    }

    #[test]
    fn production_defaults_are_periodic_and_bounded() {
        let cli = Cli::try_parse_from(arguments(&[])).expect("CLI");

        assert_eq!(cli.bind, "127.0.0.1:3101".parse().expect("address"));
        assert_eq!(cli.poll_seconds.get(), 900);
        assert_eq!(cli.membership_rpc_timeout_seconds.get(), 120);
        assert_eq!(cli.membership_collection_budget_seconds.get(), 300);
        assert_eq!(cli.max_mempool_entries.get(), 200_000);
        assert_eq!(cli.classification_slice_entries.get(), 2_048);
        assert_eq!(cli.classification_rpc_lanes.get(), 4);
        assert_eq!(cli.classification_total_cache_mib.get(), 256);
        validate_bind(cli.bind).expect("loopback bind");
    }

    #[test]
    fn source_file_preserves_declared_order_and_per_source_credentials() {
        let sources = parse_sources_file(
            br#"{
                "sources": [
                    {
                        "source_id": "core",
                        "source_label": "Bitcoin Core",
                        "rpc_url": "http://127.0.0.1:18443/",
                        "rpc_username": "atlas-core",
                        "rpc_password_credential": "core-password"
                    },
                    {
                        "source_id": "knots",
                        "source_label": "Bitcoin Knots",
                        "rpc_url": "http://127.0.0.1:28443/",
                        "rpc_username": "atlas-knots",
                        "rpc_password_credential": "knots-password"
                    }
                ]
            }"#,
        )
        .expect("source configuration");

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source_id, "core");
        assert_eq!(sources[1].source_id, "knots");
        assert_eq!(sources[1].rpc_password_credential, "knots-password");
    }

    #[test]
    fn source_file_rejects_empty_duplicate_excess_and_unknown_configuration() {
        assert!(parse_sources_file(br#"{"sources": []}"#).is_err());
        assert!(
            parse_sources_file(
                br#"{"sources": [
                    {"source_id":"core","source_label":"Core","rpc_url":"http://core/","rpc_username":"atlas","rpc_password_credential":"password"},
                    {"source_id":"core","source_label":"Again","rpc_url":"http://other/","rpc_username":"atlas","rpc_password_credential":"password"}
                ]}"#,
            )
            .is_err()
        );
        let five_sources = format!(
            "{{\"sources\":[{}]}}",
            (0..=MAX_CONFIGURED_SOURCES)
                .map(|index| format!(
                    "{{\"source_id\":\"source-{index}\",\"source_label\":\"Source {index}\",\"rpc_url\":\"http://source-{index}/\",\"rpc_username\":\"atlas\",\"rpc_password_credential\":\"password\"}}"
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(parse_sources_file(five_sources.as_bytes()).is_err());
        assert!(parse_sources_file(br#"{"sources": [], "unexpected": true}"#,).is_err());
    }

    #[test]
    fn source_file_rejects_credential_path_traversal() {
        for credential in [
            "",
            ".",
            "..",
            "../password",
            "nested/password",
            "line\nbreak",
        ] {
            assert!(
                validate_credential_name(credential).is_err(),
                "{credential:?}"
            );
        }
        validate_credential_name("mempool-atlas-core.password").expect("safe credential");
    }

    #[test]
    fn public_bind_is_rejected() {
        for bind in ["0.0.0.0:3101", "192.0.2.1:3101", "[::]:3101"] {
            let cli = Cli::try_parse_from(arguments(&["--bind", bind])).expect("CLI");
            assert!(validate_bind(cli.bind).is_err(), "{bind}");
        }
    }

    #[test]
    fn password_file_accepts_one_optional_trailing_newline() {
        assert_eq!(
            parse_secret("secret\n".to_owned()).expect("secret"),
            "secret"
        );
        assert_eq!(
            parse_secret("secret\r\n".to_owned()).expect("secret"),
            "secret"
        );
        assert!(parse_secret(String::new()).is_err());
        assert!(parse_secret("one\ntwo".to_owned()).is_err());
    }
}
