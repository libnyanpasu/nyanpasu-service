pub mod consts;
mod instance;
mod logger;
mod routing;

pub use instance::CoreManagerWrapper as CoreManager;
pub use logger::Logger;
use nyanpasu_ipc::{SERVICE_PLACEHOLDER, server::create_server};
use routing::apply_routes;
use tokio_util::sync::CancellationToken;
use tracing_attributes::instrument;
use axum::Router;

#[instrument]
// TODO: impl axum graceful shutdown, and wrap inner stream into axum trait
pub async fn run(token: CancellationToken) -> Result<(), anyhow::Error> {
    let app = apply_routes(Router::new());
    tracing::info!("Starting server...");
    create_server(SERVICE_PLACEHOLDER, app).await?;
    Ok(())
}
