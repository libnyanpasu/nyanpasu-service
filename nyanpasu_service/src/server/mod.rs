pub mod consts;
mod instance;
mod routing;

use anyhow::Ok;
use axum::Router;
pub use instance::CoreManagerWrapper as CoreManager;
use nyanpasu_ipc::server::create_server;

use crate::consts::APP_NAME;

pub async fn run() -> Result<(), anyhow::Error> {
    let app = Router::new();
    create_server(APP_NAME, app).await?;
    Ok(())
}
