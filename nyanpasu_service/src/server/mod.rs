pub mod consts;
mod instance;
mod logger;
mod routing;

use axum::Router;
pub use instance::CoreManagerHandle as CoreManager;
pub use logger::Logger;
use nyanpasu_ipc::{SERVICE_PLACEHOLDER, server::create_server};
use routing::create_router;
use tokio_util::sync::CancellationToken;
use tracing_attributes::instrument;

#[instrument]
// TODO: impl axum graceful shutdown, and wrap inner stream into axum trait
pub async fn run(token: CancellationToken) -> Result<(), anyhow::Error> {
    let app = create_router();
    tracing::info!("Starting server...");
    create_server(
        SERVICE_PLACEHOLDER,
        app,
        Some(async move {
            token.cancelled().await;
        }),
    )
    .await?;
    Ok(())
}
