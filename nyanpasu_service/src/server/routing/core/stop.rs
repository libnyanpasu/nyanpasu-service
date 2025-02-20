use std::borrow::Cow;

use axum::{Json, http::StatusCode};
use nyanpasu_ipc::api::{RBuilder, core::stop::CoreStopRes};

pub async fn stop() -> (StatusCode, Json<CoreStopRes<'static>>) {
    let manager = crate::server::instance::CoreManagerWrapper::global();
    let res = manager.stop().await;
    match res {
        Ok(_) => (StatusCode::OK, Json(RBuilder::success(()))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(RBuilder::other_error(Cow::Owned(e.to_string()))),
        ),
    }
}
