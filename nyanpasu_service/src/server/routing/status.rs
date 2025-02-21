use std::borrow::Cow;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};

use nyanpasu_ipc::api::{
    RBuilder,
    status::{RuntimeInfos, STATUS_ENDPOINT, StatusRes, StatusResBody},
};

use super::AppState;

pub fn setup() -> Router<AppState> {
    let router = Router::new();
    router.route(STATUS_ENDPOINT, get(status))
}

pub async fn status(State(state): State<AppState>) -> (StatusCode, Json<StatusRes<'static>>) {
    let status = state.core_manager.status();
    let runtime_infos = crate::server::consts::RuntimeInfos::global();
    let res = RBuilder::success(StatusResBody {
        version: Cow::Borrowed(crate::consts::APP_VERSION),
        core_infos: status,
        runtime_infos: RuntimeInfos {
            service_data_dir: Cow::Borrowed(&runtime_infos.service_data_dir),
            service_config_dir: Cow::Borrowed(&runtime_infos.service_config_dir),
            nyanpasu_config_dir: Cow::Borrowed(&runtime_infos.nyanpasu_config_dir),
            nyanpasu_data_dir: Cow::Borrowed(&runtime_infos.nyanpasu_data_dir),
        },
    });

    (StatusCode::OK, Json(res))
}
