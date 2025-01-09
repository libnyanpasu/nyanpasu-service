use axum::{routing::post, Router};
use nyanpasu_ipc::api::core::{
    restart::CORE_RESTART_ENDPOINT, start::CORE_START_ENDPOINT, stop::CORE_STOP_ENDPOINT,
};

pub mod restart;
pub mod start;
pub mod stop;

pub fn setup() -> Router {
    Router::new()
        .route(CORE_START_ENDPOINT, post(start::start))
        .route(CORE_STOP_ENDPOINT, post(stop::stop))
        .route(CORE_RESTART_ENDPOINT, post(restart::restart))
}
