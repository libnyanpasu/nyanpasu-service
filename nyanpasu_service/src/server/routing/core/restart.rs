use std::borrow::Cow;

use axum::{Json, http::StatusCode};
use nyanpasu_ipc::api::{RBuilder, core::restart::CoreRestartRes};

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
