use axum::{http::StatusCode, Json};
use nyanpasu_ipc::api::{
    log::{LogsRes, LogsResBody},
    RBuilder,
};

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
