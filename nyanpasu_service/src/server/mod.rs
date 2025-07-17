pub mod consts;
mod instance;
mod logger;
mod routing;

pub use instance::CoreManagerHandle as CoreManager;
pub use logger::Logger;
use nyanpasu_ipc::{
    SERVICE_PLACEHOLDER,
    api::ws::events::{Event as WsEvent, TraceLog},
    server::create_server,
};
use routing::{AppState, create_router};
use tokio_util::sync::CancellationToken;
use tracing_attributes::instrument;

#[instrument]
pub async fn run(
    token: CancellationToken,
    #[cfg(windows)] sids: &[&str],
    #[cfg(not(windows))] sids: (),
) -> Result<(), anyhow::Error> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    let core_manager = CoreManager::new_with_notify(tx);
    let state = AppState {
        core_manager,
        ..Default::default()
    };
    let ws_state = state.ws_state.clone();
    tokio::spawn(async move {
        while let Some(state) = rx.recv().await {
            tracing::info!("State changed: {:?}", state);
            ws_state
                .event_broadcast(WsEvent::new_core_state_changed(state))
                .await;
        }
    });
    let ws_state = state.ws_state.clone();
    Logger::global().set_subscriber(Box::new(move |logging| {
        let ws_state = ws_state.clone();
        tokio::spawn(async move {
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

    let app = create_router(state);
    tracing::info!("Starting server...");
    create_server(
        SERVICE_PLACEHOLDER,
        app,
        Some(async move {
            token.cancelled().await;
        }),
        sids,
    )
    .await?;
    Ok(())
}
