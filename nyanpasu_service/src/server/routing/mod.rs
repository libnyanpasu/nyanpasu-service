use axum::{
    routing::{get, post},
    Router,
};

use nyanpasu_ipc::api::{
    core::{restart::CORE_RESTART_ENDPOINT, start::CORE_START_ENDPOINT, stop::CORE_STOP_ENDPOINT},
    status::STATUS_ENDPOINT,
};
use tracing_attributes::instrument;

pub mod core;
pub mod status;

#[instrument]
pub fn apply_routes(app: Router) -> Router {
    tracing::info!("Applying routes...");
    app.route(STATUS_ENDPOINT, get(status::status))
        .route(CORE_START_ENDPOINT, post(core::start::start))
        .route(CORE_STOP_ENDPOINT, post(core::stop::stop))
        .route(CORE_RESTART_ENDPOINT, post(core::restart::restart))
}
