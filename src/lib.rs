mod bip110;

pub mod api;
pub mod classification;
mod classification_rpc;
mod classifiers;
pub(crate) mod conflict_facts;
pub mod model;
#[cfg(feature = "perf-fixtures")]
pub mod perf_fixtures;
pub mod rpc;
pub mod rpc_transport;
pub mod runtime;
pub mod staged_snapshot;

#[cfg(test)]
pub(crate) async fn spawn_test_server(
    application: axum::Router,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let server = tokio::spawn(async move {
        axum::serve(listener, application)
            .await
            .expect("fixture server");
    });
    (address, server)
}

pub use api::router;
pub use classification::{ClassificationError, ClassificationLimits, ClassificationPipeline};
pub use model::{
    Bip110Assessment, Bip110RuleDetail, Bip110RuleId, Bip110RuleVerdict, Bip110Scope, Bip110Status,
    Bip110Summary, ChainTip, MempoolEntry, MempoolObservation, MempoolSnapshot, SourceAvailability,
    SourceSummary, SourcesResponse, TransactionDetailResponse,
};
pub use rpc::{RpcClient, RpcError};
pub use runtime::{
    AtlasRuntime, AtlasSource, MAX_CONFIGURED_SOURCES, SourceRegistry, SourceRuntime,
    TransactionLookup,
};
