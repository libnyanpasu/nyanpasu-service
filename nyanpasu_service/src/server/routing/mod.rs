use axum::{Router, routing::get};

use nyanpasu_ipc::api::status::STATUS_ENDPOINT;

pub mod status;

pub fn apply_routes(app: Router) -> Router {
    app.route(STATUS_ENDPOINT, get(status::status))
}
