pub mod consts;
mod instance;
mod routing;

use anyhow::Ok;
use axum::Router;
pub use instance::CoreManagerWrapper as CoreManager;
use nyanpasu_ipc::server::create_server;
use routing::apply_routes;

use crate::consts::APP_NAME;

pub async fn run() -> Result<(), anyhow::Error> {
    let app = apply_routes(Router::new());
    create_server(APP_NAME, app).await?;
    Ok(())
}
