//! Round-trip tests: the reqwest-based client against an axum server over a
//! Windows named pipe.
//!
//! These tests are Windows-only: they must run unprivileged. `create_server`
//! applies a security descriptor whose owner is the Administrators group,
//! which only works for the privileged service account (error 1307
//! otherwise), and on unix the client hardcodes the root-owned
//! `/var/run/{name}.sock` path. The wire protocol itself is
//! platform-independent, so the test binds its own default-ACL pipe.
#![cfg(windows)]

use std::{borrow::Cow, path::PathBuf, time::Duration};

use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use futures_util::StreamExt;
use indexmap::IndexMap;
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, ToFsName,
    tokio::{Listener, Stream as PipeStream, prelude::*},
};
use nyanpasu_ipc::{
    api::{
        RBuilder, ResponseCode,
        core::stop::{CORE_STOP_ENDPOINT, CoreStopRes},
        status::{CoreInfos, CoreState, RuntimeInfos, STATUS_ENDPOINT, StatusRes, StatusResBody},
        ws::events::{EVENT_URI, Event, TraceLog},
    },
    client::{Client, ClientError},
};

const TEST_VERSION: &str = "9.9.9-roundtrip";

/// Minimal `axum::serve::Listener` over a named pipe with the default ACL,
/// mirroring `nyanpasu_ipc::server::InterProcessListener`.
struct PipeListener(Listener, String);

impl axum::serve::Listener for PipeListener {
    type Io = PipeStream;
    type Addr = String;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok(stream) => return (stream, self.1.clone()),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    fn local_addr(&self) -> tokio::io::Result<Self::Addr> {
        Ok(self.1.clone())
    }
}

fn test_status_body() -> StatusResBody<'static> {
    StatusResBody {
        version: Cow::Borrowed(TEST_VERSION),
        core_infos: CoreInfos {
            r#type: None,
            state: CoreState::Running,
            state_changed_at: 42,
            config_path: None,
        },
        runtime_infos: RuntimeInfos {
            service_data_dir: Cow::Owned(PathBuf::from("/srv/data")),
            service_config_dir: Cow::Owned(PathBuf::from("/srv/config")),
            nyanpasu_config_dir: Cow::Owned(PathBuf::from("/home/config")),
            nyanpasu_data_dir: Cow::Owned(PathBuf::from("/home/data")),
        },
    }
}

async fn status_handler() -> (StatusCode, Json<StatusRes<'static>>) {
    (StatusCode::OK, Json(RBuilder::success(test_status_body())))
}

async fn stop_handler() -> (StatusCode, Json<CoreStopRes<'static>>) {
    (StatusCode::OK, Json(RBuilder::success(())))
}

async fn status_fails_with_500() -> (StatusCode, Json<StatusRes<'static>>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(RBuilder::other_error(Cow::Borrowed("boom-500"))),
    )
}

async fn status_fails_in_envelope() -> (StatusCode, Json<StatusRes<'static>>) {
    (
        StatusCode::OK,
        Json(RBuilder::other_error(Cow::Borrowed("boom-envelope"))),
    )
}

async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|mut socket: WebSocket| async move {
        let event = Event::new_log(TraceLog {
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            level: "INFO".to_owned(),
            message: "hello events".to_owned(),
            target: "roundtrip".to_owned(),
            fields: IndexMap::new(),
        });
        let bytes = serde_json::to_vec(&event).unwrap();
        let _ = socket.send(Message::binary(bytes)).await;
        // keep the socket open until the client goes away
        while let Some(Ok(_)) = socket.recv().await {}
    })
}

fn spawn_server(placeholder: &str, router: Router) -> tokio::sync::oneshot::Sender<()> {
    let name = format!("\\\\.\\pipe\\{placeholder}")
        .to_fs_name::<GenericFilePath>()
        .expect("pipe name should be valid");
    let listener = ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Both)
        .create_tokio()
        .expect("pipe listener should bind");
    let listener = PipeListener(listener, placeholder.to_owned());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await
            .expect("test server should run");
    });
    tx
}

/// Poll the status endpoint until it responds or the error changes,
/// giving the spawned server time to start.
async fn wait_until_up(client: &Client) {
    for _ in 0..100 {
        match client.status().await {
            Err(ClientError::Request { .. }) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => return,
        }
    }
    panic!("server did not start in time");
}

#[tokio::test]
async fn status_stop_and_events_roundtrip() {
    let placeholder = format!("nyanpasu-ipc-roundtrip-{}-ok", std::process::id());
    let router = Router::new()
        .route(STATUS_ENDPOINT, get(status_handler))
        .route(CORE_STOP_ENDPOINT, post(stop_handler))
        .route(EVENT_URI, get(ws_handler));
    let shutdown = spawn_server(&placeholder, router);
    let client = Client::new(&placeholder).expect("client should build");
    wait_until_up(&client).await;

    let status = client.status().await.expect("status should succeed");
    assert_eq!(status.version, TEST_VERSION);
    assert!(matches!(status.core_infos.state, CoreState::Running));
    assert_eq!(status.core_infos.state_changed_at, 42);
    assert_eq!(
        *status.runtime_infos.service_data_dir,
        PathBuf::from("/srv/data")
    );

    client.stop_core().await.expect("stop_core should succeed");

    let mut events = client.events().await.expect("events should connect");
    let event = events
        .next()
        .await
        .expect("stream should yield an event")
        .expect("event should decode");
    match event {
        Event::Log(log) => {
            assert_eq!(log.message, "hello events");
            assert_eq!(log.target, "roundtrip");
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn http_error_envelope_maps_to_server_error() {
    let placeholder = format!("nyanpasu-ipc-roundtrip-{}-500", std::process::id());
    let router = Router::new().route(STATUS_ENDPOINT, get(status_fails_with_500));
    let shutdown = spawn_server(&placeholder, router);
    let client = Client::new(&placeholder).expect("client should build");
    wait_until_up(&client).await;

    match client.status().await {
        Err(ClientError::Server { code, msg, .. }) => {
            assert_eq!(code, ResponseCode::OtherError);
            assert_eq!(msg, "boom-500");
        }
        other => panic!("expected a server error, got: {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn ok_status_with_error_code_maps_to_server_error() {
    let placeholder = format!("nyanpasu-ipc-roundtrip-{}-env", std::process::id());
    let router = Router::new().route(STATUS_ENDPOINT, get(status_fails_in_envelope));
    let shutdown = spawn_server(&placeholder, router);
    let client = Client::new(&placeholder).expect("client should build");
    wait_until_up(&client).await;

    match client.status().await {
        Err(ClientError::Server { code, msg, .. }) => {
            assert_eq!(code, ResponseCode::OtherError);
            assert_eq!(msg, "boom-envelope");
        }
        other => panic!("expected a server error, got: {other:?}"),
    }

    let _ = shutdown.send(());
}
