use std::borrow::Cow;

use axum::{http::StatusCode, Json};
use nyanpasu_ipc::api::{core::restart::CoreRestartRes, RBuilder};

pub async fn restart() -> (StatusCode, Json<CoreRestartRes<'static>>) {
    let manager = crate::server::instance::CoreManagerWrapper::global();
    let res = manager.restart().await;
    match res {
        Ok(_) => (StatusCode::OK, Json(RBuilder::success(()))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(RBuilder::other_error(Cow::Owned(e.to_string()))),
        ),
    }
}
