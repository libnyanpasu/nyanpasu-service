use axum::Router;
use tracing_attributes::instrument;
use ws::WsState;

use super::CoreManager;

pub mod core;
pub mod logs;
pub mod network;
pub mod status;
pub mod ws;

#[derive(Default, Clone)]
struct AppState {
    core_manager: CoreManager,
    ws_state: WsState,
}

#[instrument]
pub fn create_router() -> Router {
    tracing::info!("Applying routes...");
    let tracing_layer = tower_http::trace::TraceLayer::new_for_http();
    Router::new()
        .merge(status::setup())
        .merge(core::setup())
        .merge(logs::setup())
        .merge(network::setup())
        .merge(ws::setup())
        .with_state(AppState::default())
        .layer(tracing_layer)
}
