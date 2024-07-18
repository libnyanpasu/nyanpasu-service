use axum::{http::StatusCode, Json};
use nyanpasu_ipc::api::log::LogsResBody;

pub async fn retrieve_logs() -> (StatusCode, Json<LogsResBody<'static>>) {
    let logs = crate::server::logger::Logger::global().retrieve_logs();
    let logs = LogsResBody { logs };
    (StatusCode::OK, Json(logs))
}

pub async fn inspect_logs() -> (StatusCode, Json<LogsResBody<'static>>) {
    let logs = crate::server::logger::Logger::global().inspect_logs();
    let logs = LogsResBody { logs };
    (StatusCode::OK, Json(logs))
}
