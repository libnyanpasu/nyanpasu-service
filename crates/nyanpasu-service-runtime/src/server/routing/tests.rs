use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header::HeaderName},
    response::Response,
};
use camino::Utf8PathBuf;
use nyanpasu_ipc::api::{
    ResponseCode,
    contract::{
        CoreRestart, CoreStart, CoreStop, IpcOperation, LogsInspect, LogsRetrieve, NetworkSetDns,
        Status as StatusOp,
    },
    core::stop::{CORE_STOP_ENDPOINT, CoreStopRes},
    status::{CoreState, STATUS_ENDPOINT, StatusRes},
};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tower::ServiceExt;

use super::{AppState, create_router};
use crate::server::{CoreManager, EventHub, Logger, consts::RuntimeInfos};

struct TestEnv {
    state: AppState,
    _dir: TempDir,
}

impl TestEnv {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let runtime_dir =
            Utf8PathBuf::from_path_buf(root.join("core-runtime")).expect("temp path is UTF-8");
        let core_manager = CoreManager::new(runtime_dir).await.unwrap();
        let runtime = Arc::new(RuntimeInfos {
            service_data_dir: root.join("service-data"),
            service_config_dir: root.join("service-config"),
            nyanpasu_config_dir: root.join("nyanpasu-config"),
            nyanpasu_data_dir: root.join("nyanpasu-data"),
            nyanpasu_app_dir: root.join("nyanpasu-app"),
        });
        let state = AppState {
            core_manager,
            hub: EventHub::new(),
            runtime,
            logger: Logger::new(),
        };
        Self { state, _dir: dir }
    }
}

async fn body_of<T: DeserializeOwned>(response: Response) -> T {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn status_reports_a_stopped_core_and_echoes_the_injected_runtime_dirs() {
    let env = TestEnv::new().await;
    let runtime = env.state.runtime.clone();
    let response = create_router(env.state.clone())
        .oneshot(
            Request::builder()
                .uri(STATUS_ENDPOINT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let envelope: StatusRes<'static> = body_of(response).await;
    assert_eq!(envelope.code, ResponseCode::Ok);
    assert_eq!(envelope.msg, ResponseCode::Ok.msg());
    let body = envelope.data.unwrap();
    assert_eq!(body.version, crate::consts::APP_VERSION);
    assert!(matches!(body.core_infos.state, CoreState::Stopped(None)));
    assert!(body.core_infos.r#type.is_none());
    assert!(body.core_infos.config_path.is_none());
    assert_eq!(
        body.runtime_infos.service_data_dir.as_ref(),
        &runtime.service_data_dir
    );
    assert_eq!(
        body.runtime_infos.service_config_dir.as_ref(),
        &runtime.service_config_dir
    );
    assert_eq!(
        body.runtime_infos.nyanpasu_config_dir.as_ref(),
        &runtime.nyanpasu_config_dir
    );
    assert_eq!(
        body.runtime_infos.nyanpasu_data_dir.as_ref(),
        &runtime.nyanpasu_data_dir
    );
}

#[tokio::test]
async fn stopping_an_idle_core_keeps_the_legacy_error_envelope() {
    let env = TestEnv::new().await;
    let response = create_router(env.state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(CORE_STOP_ENDPOINT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let envelope: CoreStopRes<'static> = body_of(response).await;
    assert_eq!(envelope.code, ResponseCode::OtherError);
    assert_eq!(envelope.msg, "core is already stopped");
    assert!(envelope.data.is_none());
}

#[tokio::test]
async fn restart_before_any_start_reports_the_legacy_error() {
    let env = TestEnv::new().await;
    let response = create_router(env.state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(CoreRestart::PATH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let envelope: CoreStopRes<'static> = body_of(response).await;
    assert_eq!(envelope.code, ResponseCode::OtherError);
    assert_eq!(envelope.msg, "core have not been started yet");
    assert!(envelope.data.is_none());
}

#[tokio::test]
async fn two_states_are_independent() {
    let first = TestEnv::new().await;
    let second = TestEnv::new().await;

    assert_ne!(
        first.state.runtime.service_data_dir,
        second.state.runtime.service_data_dir
    );

    let first_response = create_router(first.state.clone())
        .oneshot(
            Request::builder()
                .uri(STATUS_ENDPOINT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let second_response = create_router(second.state.clone())
        .oneshot(
            Request::builder()
                .uri(STATUS_ENDPOINT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(second_response.status(), StatusCode::OK);
    let first_envelope: StatusRes<'static> = body_of(first_response).await;
    let second_envelope: StatusRes<'static> = body_of(second_response).await;
    let first_body = first_envelope.data.unwrap();
    let second_body = second_envelope.data.unwrap();
    assert_eq!(
        first_body.runtime_infos.service_data_dir.as_ref(),
        &first.state.runtime.service_data_dir
    );
    assert_eq!(
        second_body.runtime_infos.service_data_dir.as_ref(),
        &second.state.runtime.service_data_dir
    );
}

/// Ask the router for `Op`'s address and report only whether it is mounted.
/// A body-less POST to `/core/start` is answered 4xx by the extractor, which
/// still proves the route exists — 404/405 are the only failures here.
async fn probe(state: AppState, method: Method, path: &str) -> StatusCode {
    create_router(state)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn every_operation_is_mounted_where_its_contract_says() {
    let env = TestEnv::new().await;
    let addresses = [
        (StatusOp::METHOD, StatusOp::PATH),
        (CoreStart::METHOD, CoreStart::PATH),
        (CoreStop::METHOD, CoreStop::PATH),
        (CoreRestart::METHOD, CoreRestart::PATH),
        (LogsRetrieve::METHOD, LogsRetrieve::PATH),
        (LogsInspect::METHOD, LogsInspect::PATH),
        (NetworkSetDns::METHOD, NetworkSetDns::PATH),
    ];
    for (method, path) in addresses {
        let status = probe(env.state.clone(), method, path).await;
        assert_ne!(status, StatusCode::NOT_FOUND, "{path} is not mounted");
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{path} is mounted with the wrong method"
        );
    }
}

#[tokio::test]
async fn an_unknown_path_answers_with_the_envelope() {
    let env = TestEnv::new().await;
    let response = create_router(env.state.clone())
        .oneshot(
            Request::builder()
                .uri("/does/not/exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let envelope: CoreStopRes<'static> = body_of(response).await;
    assert_eq!(envelope.code, ResponseCode::OtherError);
    assert_eq!(envelope.msg, "not found");
}

#[tokio::test]
async fn a_wrong_method_answers_with_the_envelope() {
    let env = TestEnv::new().await;
    let response = create_router(env.state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(STATUS_ENDPOINT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let envelope: CoreStopRes<'static> = body_of(response).await;
    assert_eq!(envelope.code, ResponseCode::OtherError);
    assert_eq!(envelope.msg, "method not allowed");
}

#[tokio::test]
async fn responses_carry_a_request_id() {
    let env = TestEnv::new().await;
    let header = HeaderName::from_static("x-request-id");

    let generated = create_router(env.state.clone())
        .oneshot(
            Request::builder()
                .uri(STATUS_ENDPOINT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(generated.headers().contains_key(&header));

    let echoed = create_router(env.state.clone())
        .oneshot(
            Request::builder()
                .uri(STATUS_ENDPOINT)
                .header(&header, "caller-supplied")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(echoed.headers().get(&header).unwrap(), "caller-supplied");
}
