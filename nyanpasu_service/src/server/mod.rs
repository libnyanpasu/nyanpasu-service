pub mod consts;
mod instance;
mod routing;

use anyhow::Ok;
use axum::Router;
pub use instance::CoreManagerWrapper as CoreManager;
use nyanpasu_ipc::{server::create_server, SERVICE_PLACEHOLDER};
use routing::apply_routes;
use tracing_attributes::instrument;

#[instrument]
pub async fn run() -> Result<(), anyhow::Error> {
    let app = apply_routes(Router::new());
    tracing::info!("Starting server...");
    create_server(SERVICE_PLACEHOLDER, app).await?;
    Ok(())
}
