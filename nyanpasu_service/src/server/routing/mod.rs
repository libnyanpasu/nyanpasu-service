use axum::Router;
use tracing_attributes::instrument;

pub mod core;
pub mod logs;
pub mod network;
pub mod status;

#[instrument]
pub fn apply_routes(app: Router) -> Router {
    tracing::info!("Applying routes...");
    let tracing_layer = tower_http::trace::TraceLayer::new_for_http();
    app.nest("/", status::setup())
        .nest("/", core::setup())
        .nest("/", logs::setup())
        .nest("/", network::setup())
        .layer(tracing_layer)
}
