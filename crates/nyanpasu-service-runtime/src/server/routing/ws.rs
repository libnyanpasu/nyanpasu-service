use axum::{
    Router,
    extract::{
        FromRef, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
    routing::any,
};
use futures_util::{SinkExt, StreamExt};
use nyanpasu_ipc::api::ws::events::EVENT_URI;
use tokio::sync::broadcast::error::RecvError;

use super::AppState;
use crate::server::events::{EventHub, WS_LAG_LOG_TARGET};

impl FromRef<AppState> for EventHub {
    fn from_ref(state: &AppState) -> Self {
        state.hub.clone()
    }
}

pub fn setup() -> Router<AppState> {
    let router = Router::new();
    router.route(EVENT_URI, any(ws_handler))
}

async fn ws_handler(State(hub): State<EventHub>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, hub))
}

async fn handle_socket(socket: WebSocket, hub: EventHub) {
    // The subscription lives and dies with this task; there is no registry to
    // insert into and no id to collide with.
    let mut events = hub.subscribe();
    let (mut sink, mut stream) = socket.split();

    let handler = async { while let Some(Ok(_)) = stream.next().await {} };

    let sender = async {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let Ok(payload) = simd_json::to_vec(&event) else {
                        tracing::error!("Failed to serialize event: {:?}", event);
                        continue;
                    };
                    if let Err(e) = sink.send(Message::binary(payload)).await {
                        tracing::error!("Failed to send event: {:?}", e);
                        break;
                    }
                }
                // Only this connection pays for being slow. Warn once, then
                // jump to the live tail: the receiver skips the backlog, so a
                // full ring cannot spin us in a Lagged loop. The warn itself
                // is tagged with WS_LAG_LOG_TARGET, which the log-forwarding
                // subscriber filters out before it ever reaches the hub —
                // otherwise many lagging connections could collectively
                // refill the ring and re-lag each other.
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        target: WS_LAG_LOG_TARGET,
                        "ws subscriber dropped {skipped} events"
                    );
                    events = events.resubscribe();
                }
                Err(RecvError::Closed) => break,
            }
        }
    };

    tokio::select! {
        _ = handler => (),
        _ = sender => (),
    }
}
