use axum::Router;
use nyanpasu_ipc::{
    api::contract::{CoreRestart, CoreStart, CoreStop},
    server::RegisterOperation,
};

use super::AppState;

pub mod restart;
pub mod start;
pub mod stop;

pub fn setup() -> Router<AppState> {
    Router::new()
        .register(CoreStart, start::start)
        .register(CoreStop, stop::stop)
        .register(CoreRestart, restart::restart)
}
