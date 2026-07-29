pub mod consts;
mod logger;
mod manager_bridge;
mod routing;

pub use logger::Logger;
pub use manager_bridge::CoreManagerService as CoreManager;
use nyanpasu_ipc::{
    SERVICE_PLACEHOLDER,
    api::ws::events::{Event as WsEvent, TraceLog},
    server::create_server,
};
use routing::{AppState, create_router};
use tokio_util::sync::CancellationToken;
use tracing_attributes::instrument;

use crate::server::routing::ws::WsState;

const SERVER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[instrument]
pub async fn run(
    token: CancellationToken,
    #[cfg(windows)] sids: &[&str],
    #[cfg(not(windows))] sids: (),
) -> Result<(), anyhow::Error> {
    let runtime_dir =
        camino::Utf8PathBuf::from_path_buf(crate::utils::dirs::service_core_runtime_dir())
            .map_err(|path| anyhow::anyhow!("core runtime dir is not UTF-8: {}", path.display()))?;
    let core_manager = CoreManager::new(runtime_dir).await?;
    let state = AppState {
        core_manager,
        ws_state: WsState::default(),
    };
    state.core_manager.spawn_bridges(state.ws_state.clone());
    let ws_state = state.ws_state.clone();
    let tokio_handle = tokio::runtime::Handle::current();
    Logger::global().set_subscriber(Box::new(move |logging| {
        let ws_state = ws_state.clone();
        tokio_handle.spawn(async move {
            ws_state
                .event_broadcast(WsEvent::new_log(TraceLog {
                    timestamp: logging.timestamp,
                    level: logging.level,
                    message: logging
                        .fields
                        .get("message")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or("".to_string()),
                    target: logging
                        .fields
                        .get("target")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or("".to_string()),
                    fields: logging.fields,
                }))
                .await;
        });
    }));

    let core_manager = state.core_manager.clone();
    let app = create_router(state);
    tracing::info!("Starting server...");
    let shutdown_token = token.clone();
    let server = create_server(
        SERVICE_PLACEHOLDER,
        app,
        Some(async move {
            shutdown_token.cancelled().await;
        }),
        sids,
    );
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => {
            core_manager.shutdown().await;
            result?;
        }
        _ = token.cancelled() => {
            core_manager.shutdown().await;
            match tokio::time::timeout(SERVER_DRAIN_TIMEOUT, &mut server).await {
                Ok(result) => result?,
                Err(_) => tracing::warn!(
                    "pipe server did not drain within {SERVER_DRAIN_TIMEOUT:?}; abandoning open connections"
                ),
            }
        }
    }
    Ok(())
}
