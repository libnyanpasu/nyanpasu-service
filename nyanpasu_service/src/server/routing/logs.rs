use axum::{Json, Router, http::StatusCode, routing::get};
use nyanpasu_ipc::api::{
    RBuilder,
    log::{LOGS_INSPECT_ENDPOINT, LOGS_RETRIEVE_ENDPOINT, LogsRes, LogsResBody},
};

pub fn setup() -> Router {
    Router::new()
        .route(LOGS_RETRIEVE_ENDPOINT, get(retrieve_logs))
        .route(LOGS_INSPECT_ENDPOINT, get(inspect_logs))
}

pub async fn retrieve_logs() -> (StatusCode, Json<LogsRes<'static>>) {
    let logs = crate::server::logger::Logger::global().retrieve_logs();
    let res = RBuilder::success(LogsResBody { logs });
    (StatusCode::OK, Json(res))
}

pub async fn inspect_logs() -> (StatusCode, Json<LogsRes<'static>>) {
    let logs = crate::server::logger::Logger::global().inspect_logs();
    let res = RBuilder::success(LogsResBody { logs });
    (StatusCode::OK, Json(res))
}
