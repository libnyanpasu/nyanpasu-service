use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use nyanpasu_ipc::api::{
    RBuilder,
    log::{LOGS_INSPECT_ENDPOINT, LOGS_RETRIEVE_ENDPOINT, LogsRes, LogsResBody},
};

use super::AppState;

pub fn setup() -> Router<AppState> {
    Router::new()
        .route(LOGS_RETRIEVE_ENDPOINT, get(retrieve_logs))
        .route(LOGS_INSPECT_ENDPOINT, get(inspect_logs))
}

pub async fn retrieve_logs(State(state): State<AppState>) -> (StatusCode, Json<LogsRes<'static>>) {
    let logs = state.logger.retrieve_logs();
    (
        StatusCode::OK,
        Json(RBuilder::success(LogsResBody { logs })),
    )
}

pub async fn inspect_logs(State(state): State<AppState>) -> (StatusCode, Json<LogsRes<'static>>) {
    let logs = state.logger.inspect_logs();
    (
        StatusCode::OK,
        Json(RBuilder::success(LogsResBody { logs })),
    )
}
