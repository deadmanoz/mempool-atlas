use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use mempool_atlas::{RpcClient, SourceRegistry, SourceRuntime, router};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "mempool-atlas")]
#[command(about = "Periodically snapshot one Bitcoin node for the Mempool Atlas website")]
struct Cli {
    #[arg(long, env = "ATLAS_BIND", default_value = "127.0.0.1:3101")]
    bind: SocketAddr,
    #[arg(long, env = "ATLAS_WEB_ROOT", default_value = "web/dist")]
    web_root: PathBuf,
    #[arg(long, env = "ATLAS_SOURCE_ID")]
    source_id: String,
    #[arg(long, env = "ATLAS_SOURCE_LABEL")]
    source_label: Option<String>,
    #[arg(long, env = "ATLAS_RPC_URL")]
    rpc_url: String,
    #[arg(long, env = "ATLAS_RPC_USERNAME", default_value = "atlas")]
    rpc_username: String,
    #[arg(long, env = "ATLAS_RPC_PASSWORD_FILE")]
    rpc_password_file: PathBuf,
    #[arg(long, env = "ATLAS_POLL_SECONDS", default_value = "300")]
    poll_seconds: NonZeroU64,
    #[arg(long, env = "ATLAS_MAX_MEMPOOL_ENTRIES", default_value = "200000")]
    max_mempool_entries: NonZeroU64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let log_filter = EnvFilter::from_default_env().add_directive(
        "corepc=debug"
            .parse()
            .expect("corepc log directive is valid"),
    );
    tracing_subscriber::fmt().with_env_filter(log_filter).init();

    let cli = Cli::parse();
    validate_bind(cli.bind)?;
    let password = read_secret(&cli.rpc_password_file)
        .with_context(|| format!("reading {}", cli.rpc_password_file.display()))?;
    let source_label = cli.source_label.unwrap_or_else(|| cli.source_id.clone());
    let poll_interval = Duration::from_secs(cli.poll_seconds.get());
    let source = Arc::new(
        SourceRuntime::new(cli.source_id.clone(), source_label, poll_interval)
            .context("validating source configuration")?,
    );
    let rpc = RpcClient::new(
        &cli.rpc_url,
        cli.rpc_username,
        password,
        cli.max_mempool_entries.get(),
    )
    .context("creating Bitcoin RPC client")?;
    let registry =
        SourceRegistry::new(vec![Arc::clone(&source)]).context("creating source registry")?;
    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;

    info!(
        bind = %cli.bind,
        source_id = %cli.source_id,
        rpc_url = %cli.rpc_url,
        poll_seconds = cli.poll_seconds.get(),
        max_mempool_entries = cli.max_mempool_entries.get(),
        "Mempool Atlas snapshot service listening"
    );

    let poll_task = tokio::spawn(Arc::clone(&source).run(rpc));
    let result = axum::serve(listener, router(registry, cli.web_root))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    poll_task.abort();
    result.context("serving Mempool Atlas API")
}

fn validate_bind(bind: SocketAddr) -> anyhow::Result<()> {
    if !bind.ip().is_loopback() {
        bail!("Atlas must bind to loopback behind the webserver reverse proxy; refusing {bind}");
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
            "--source-id",
            "core",
            "--rpc-url",
            "http://127.0.0.1:18443/",
            "--rpc-password-file",
            "/run/credentials/atlas-rpc-password",
        ];
        arguments.extend_from_slice(extra);
        arguments
    }

    #[test]
    fn production_defaults_are_periodic_and_bounded() {
        let cli = Cli::try_parse_from(arguments(&[])).expect("CLI");

        assert_eq!(cli.bind, "127.0.0.1:3101".parse().expect("address"));
        assert_eq!(cli.rpc_username, "atlas");
        assert_eq!(cli.poll_seconds.get(), 300);
        assert_eq!(cli.max_mempool_entries.get(), 200_000);
        validate_bind(cli.bind).expect("loopback bind");
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
