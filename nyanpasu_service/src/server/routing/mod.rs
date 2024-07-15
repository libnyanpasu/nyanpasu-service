use axum::{
    routing::{get, post},
    Router,
};

use nyanpasu_ipc::api::{core::start::CORE_START_ENDPOINT, status::STATUS_ENDPOINT};

pub mod core;
pub mod status;

pub fn apply_routes(app: Router) -> Router {
    app.route(STATUS_ENDPOINT, get(status::status))
        .route(CORE_START_ENDPOINT, post(core::start::start))
}
