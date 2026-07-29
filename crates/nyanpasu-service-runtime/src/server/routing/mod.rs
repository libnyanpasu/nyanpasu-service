use std::sync::Arc;

use axum::Router;
use tracing_attributes::instrument;

use super::{CoreManager, EventHub, Logger, consts::RuntimeInfos};

pub mod core;
pub mod logs;
pub mod network;
pub mod status;
pub mod ws;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct AppState {
    pub core_manager: CoreManager,
    pub hub: EventHub,
    pub runtime: Arc<RuntimeInfos>,
    pub logger: Logger<'static>,
}

#[instrument(skip(state))]
pub fn create_router(state: AppState) -> Router {
    tracing::info!("Applying routes...");
    let tracing_layer = tower_http::trace::TraceLayer::new_for_http();
    Router::new()
        .merge(status::setup())
        .merge(core::setup())
        .merge(logs::setup())
        .merge(network::setup())
        .merge(ws::setup())
        .with_state(state)
        .layer(tracing_layer)
}
