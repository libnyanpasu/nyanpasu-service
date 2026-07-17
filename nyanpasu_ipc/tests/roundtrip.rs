//! Round-trip integration tests covering every interface of the IPC client,
//! against an axum server bound to the real local transport: a named pipe on
//! Windows, a unix socket elsewhere.
//!
//! The tests bind their own listener instead of `create_server`, which is
//! meant for the privileged service account: its security descriptor owner
//! only applies with elevation (error 1307 otherwise), and its unix socket
//! permission steps require root and the `nyanpasu` group.
//!
//! On unix the client hardcodes `/var/run/{name}.sock`, a root-owned
//! directory, so the tests skip with a message when it is not writable.
//! CI makes it writable instead (see `.github/workflows/integration.yml`);
//! locally, run as root for unix socket coverage.
#![cfg(any(windows, unix))]

use std::{
    borrow::Cow,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use futures_util::StreamExt;
use indexmap::IndexMap;
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, ToFsName,
    tokio::{Listener, Stream as IpcStream, prelude::*},
};
use nyanpasu_ipc::{
    api::{
        RBuilder, ResponseCode,
        core::{
            restart::{CORE_RESTART_ENDPOINT, CoreRestartRes},
            start::{CORE_START_ENDPOINT, CoreStartReq, CoreStartRes},
            stop::{CORE_STOP_ENDPOINT, CoreStopRes},
        },
        log::{LOGS_INSPECT_ENDPOINT, LOGS_RETRIEVE_ENDPOINT, LogsRes, LogsResBody},
        network::set_dns::{NETWORK_SET_DNS_ENDPOINT, NetworkSetDnsReq, NetworkSetDnsRes},
        status::{CoreInfos, CoreState, RuntimeInfos, STATUS_ENDPOINT, StatusRes, StatusResBody},
        ws::events::{EVENT_URI, Event, TraceLog},
    },
    client::{Client, ClientError},
};
use nyanpasu_utils::core::{ClashCoreType, CoreType};

const TEST_VERSION: &str = "9.9.9-roundtrip";

// ---------------------------------------------------------------------------
// Transport glue
// ---------------------------------------------------------------------------

/// Must match the client's path resolution (`utils::get_name_string`).
fn socket_path(placeholder: &str) -> String {
    if cfg!(windows) {
        format!("\\\\.\\pipe\\{placeholder}")
    } else {
        format!("/var/run/{placeholder}.sock")
    }
}

/// Whether the tests can bind the local transport on this machine.
#[cfg(windows)]
fn transport_available() -> bool {
    true
}

/// The unix socket lives in the root-owned `/var/run`; probe once.
#[cfg(unix)]
fn transport_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let probe = format!("/var/run/.nyanpasu-ipc-probe-{}", std::process::id());
        match std::fs::File::create(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(error) => {
                eprintln!(
                    "skipping unix socket tests: /var/run is not writable ({error}); \
                     run as root for unix socket coverage"
                );
                false
            }
        }
    })
}

/// Minimal `axum::serve::Listener` over the local transport with the default
/// ACL, mirroring `nyanpasu_ipc::server::InterProcessListener`.
struct IpcListener(Listener, String);

impl axum::serve::Listener for IpcListener {
    type Io = IpcStream;
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

fn spawn_server(placeholder: &str, router: Router) -> Option<tokio::sync::oneshot::Sender<()>> {
    if !transport_available() {
        return None;
    }
    let path = socket_path(placeholder);
    #[cfg(unix)]
    let _ = std::fs::remove_file(&path);
    let name = path
        .as_str()
        .to_fs_name::<GenericFilePath>()
        .expect("socket name should be valid");
    let listener = ListenerOptions::new()
        .name(name)
        .nonblocking(ListenerNonblockingMode::Both)
        .create_tokio()
        .expect("listener should bind");
    let listener = IpcListener(listener, path);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await
            .expect("test server should run");
    });
    Some(tx)
}

fn cleanup(placeholder: &str) {
    #[cfg(unix)]
    let _ = std::fs::remove_file(socket_path(placeholder));
    let _ = placeholder;
}

/// Poll the status endpoint until the server responds, then return the client.
async fn run_server(
    placeholder: &str,
    router: Router,
) -> Option<(tokio::sync::oneshot::Sender<()>, Client)> {
    let shutdown = spawn_server(placeholder, router)?;
    let client = Client::new(placeholder).expect("client should build");
    for _ in 0..100 {
        match client.status().await {
            Err(ClientError::Request { .. }) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => return Some((shutdown, client)),
        }
    }
    panic!("server did not start in time");
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// What the server received, for asserting request payloads.
#[derive(Default)]
struct Received {
    start_core: Option<(CoreType, PathBuf)>,
    stop_core_calls: usize,
    restart_core_calls: usize,
    set_dns_calls: Vec<Option<Vec<IpAddr>>>,
}

type Shared = Arc<Mutex<Received>>;

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

async fn start_core_handler(
    State(state): State<Shared>,
    Json(req): Json<CoreStartReq<'static>>,
) -> (StatusCode, Json<CoreStartRes<'static>>) {
    state.lock().unwrap().start_core =
        Some((req.core_type.into_owned(), req.config_file.into_owned()));
    (StatusCode::OK, Json(RBuilder::success(())))
}

async fn stop_core_handler(
    State(state): State<Shared>,
) -> (StatusCode, Json<CoreStopRes<'static>>) {
    state.lock().unwrap().stop_core_calls += 1;
    (StatusCode::OK, Json(RBuilder::success(())))
}

async fn restart_core_handler(
    State(state): State<Shared>,
) -> (StatusCode, Json<CoreRestartRes<'static>>) {
    state.lock().unwrap().restart_core_calls += 1;
    (StatusCode::OK, Json(RBuilder::success(())))
}

async fn inspect_logs_handler() -> (StatusCode, Json<LogsRes<'static>>) {
    let logs = LogsResBody {
        logs: vec![Cow::Borrowed("inspect-1"), Cow::Borrowed("inspect-2")],
    };
    (StatusCode::OK, Json(RBuilder::success(logs)))
}

async fn retrieve_logs_handler() -> (StatusCode, Json<LogsRes<'static>>) {
    let logs = LogsResBody {
        logs: vec![Cow::Borrowed("retrieve-1")],
    };
    (StatusCode::OK, Json(RBuilder::success(logs)))
}

async fn set_dns_handler(
    State(state): State<Shared>,
    Json(req): Json<NetworkSetDnsReq<'static>>,
) -> (StatusCode, Json<NetworkSetDnsRes<'static>>) {
    let dns_servers = req
        .dns_servers
        .map(|servers| servers.into_iter().map(Cow::into_owned).collect());
    state.lock().unwrap().set_dns_calls.push(dns_servers);
    (StatusCode::OK, Json(RBuilder::success(())))
}

async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|mut socket: WebSocket| async move {
        let events = [
            Event::new_log(TraceLog {
                timestamp: "2026-01-01T00:00:00Z".to_owned(),
                level: "INFO".to_owned(),
                message: "hello events".to_owned(),
                target: "roundtrip".to_owned(),
                fields: IndexMap::new(),
            }),
            Event::new_core_state_changed(CoreState::Stopped(Some("bye".to_owned()))),
        ];
        for event in events {
            let bytes = serde_json::to_vec(&event).unwrap();
            if socket.send(Message::binary(bytes)).await.is_err() {
                return;
            }
        }
        // keep the socket open until the client goes away
        while let Some(Ok(_)) = socket.recv().await {}
    })
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

fn test_router(state: Shared) -> Router {
    Router::new()
        .route(STATUS_ENDPOINT, get(status_handler))
        .route(CORE_START_ENDPOINT, post(start_core_handler))
        .route(CORE_STOP_ENDPOINT, post(stop_core_handler))
        .route(CORE_RESTART_ENDPOINT, post(restart_core_handler))
        .route(LOGS_INSPECT_ENDPOINT, get(inspect_logs_handler))
        .route(LOGS_RETRIEVE_ENDPOINT, get(retrieve_logs_handler))
        .route(NETWORK_SET_DNS_ENDPOINT, post(set_dns_handler))
        .route(EVENT_URI, get(ws_handler))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rest_roundtrip() {
    let placeholder = format!("nyanpasu-ipc-test-{}-rest", std::process::id());
    let state = Shared::default();
    let Some((shutdown, client)) = run_server(&placeholder, test_router(state.clone())).await
    else {
        return;
    };

    let status = client.status().await.expect("status should succeed");
    assert_eq!(status.version, TEST_VERSION);
    assert!(matches!(status.core_infos.state, CoreState::Running));
    assert_eq!(status.core_infos.state_changed_at, 42);
    assert_eq!(
        *status.runtime_infos.service_data_dir,
        PathBuf::from("/srv/data")
    );
    assert_eq!(
        *status.runtime_infos.nyanpasu_data_dir,
        PathBuf::from("/home/data")
    );

    client
        .start_core(&CoreStartReq {
            core_type: Cow::Owned(CoreType::Clash(ClashCoreType::Mihomo)),
            config_file: Cow::Owned(PathBuf::from("/etc/nyanpasu/config.yaml")),
        })
        .await
        .expect("start_core should succeed");
    client.stop_core().await.expect("stop_core should succeed");
    client
        .restart_core()
        .await
        .expect("restart_core should succeed");

    let inspect = client.inspect_logs().await.expect("inspect_logs");
    assert_eq!(
        inspect
            .logs
            .iter()
            .map(|log| log.as_ref())
            .collect::<Vec<_>>(),
        ["inspect-1", "inspect-2"]
    );
    let retrieve = client.retrieve_logs().await.expect("retrieve_logs");
    assert_eq!(
        retrieve
            .logs
            .iter()
            .map(|log| log.as_ref())
            .collect::<Vec<_>>(),
        ["retrieve-1"]
    );

    let servers = [
        IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
    ];
    client
        .set_dns(&NetworkSetDnsReq {
            dns_servers: Some(servers.iter().copied().map(Cow::Owned).collect()),
        })
        .await
        .expect("set_dns with servers should succeed");
    client
        .set_dns(&NetworkSetDnsReq { dns_servers: None })
        .await
        .expect("set_dns without servers should succeed");

    let received = state.lock().unwrap();
    assert_eq!(
        received.start_core,
        Some((
            CoreType::Clash(ClashCoreType::Mihomo),
            PathBuf::from("/etc/nyanpasu/config.yaml")
        ))
    );
    assert_eq!(received.stop_core_calls, 1);
    assert_eq!(received.restart_core_calls, 1);
    assert_eq!(received.set_dns_calls, [Some(servers.to_vec()), None]);

    let _ = shutdown.send(());
    cleanup(&placeholder);
}

#[tokio::test]
async fn events_roundtrip() {
    let placeholder = format!("nyanpasu-ipc-test-{}-events", std::process::id());
    let Some((shutdown, client)) = run_server(&placeholder, test_router(Shared::default())).await
    else {
        return;
    };

    let mut events = client.events().await.expect("events should connect");

    let event = events
        .next()
        .await
        .expect("stream should yield a first event")
        .expect("first event should decode");
    match event {
        Event::Log(log) => {
            assert_eq!(log.message, "hello events");
            assert_eq!(log.target, "roundtrip");
            assert_eq!(log.level, "INFO");
        }
        other => panic!("expected a log event, got: {other:?}"),
    }

    let event = events
        .next()
        .await
        .expect("stream should yield a second event")
        .expect("second event should decode");
    match event {
        Event::CoreStateChanged(CoreState::Stopped(Some(reason))) => {
            assert_eq!(reason, "bye");
        }
        other => panic!("expected a core state changed event, got: {other:?}"),
    }

    let _ = shutdown.send(());
    cleanup(&placeholder);
}

#[tokio::test]
async fn http_error_envelope_maps_to_server_error() {
    let placeholder = format!("nyanpasu-ipc-test-{}-500", std::process::id());
    let router = Router::new().route(STATUS_ENDPOINT, get(status_fails_with_500));
    let Some((shutdown, client)) = run_server(&placeholder, router).await else {
        return;
    };

    match client.status().await {
        Err(ClientError::Server { code, msg, .. }) => {
            assert_eq!(code, ResponseCode::OtherError);
            assert_eq!(msg, "boom-500");
        }
        other => panic!("expected a server error, got: {other:?}"),
    }

    let _ = shutdown.send(());
    cleanup(&placeholder);
}

#[tokio::test]
async fn ok_status_with_error_code_maps_to_server_error() {
    let placeholder = format!("nyanpasu-ipc-test-{}-env", std::process::id());
    let router = Router::new().route(STATUS_ENDPOINT, get(status_fails_in_envelope));
    let Some((shutdown, client)) = run_server(&placeholder, router).await else {
        return;
    };

    match client.status().await {
        Err(ClientError::Server { code, msg, .. }) => {
            assert_eq!(code, ResponseCode::OtherError);
            assert_eq!(msg, "boom-envelope");
        }
        other => panic!("expected a server error, got: {other:?}"),
    }

    let _ = shutdown.send(());
    cleanup(&placeholder);
}
