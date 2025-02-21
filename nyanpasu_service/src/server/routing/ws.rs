use std::sync::Arc;

use super::AppState;
use axum::{
    Router,
    extract::{
        FromRef, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
    routing::any,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use nyanpasu_ipc::api::ws::events::{EVENT_URI, Event};
use tokio::sync::mpsc::Sender as MpscSender;

type SocketId = usize;

#[derive(Default, Clone)]
pub struct WsState {
    pub events_subscribers: Arc<DashMap<SocketId, MpscSender<Event>>>,
}

impl WsState {
    pub async fn event_broadcast(&self, event: Event) {
        futures_util::future::join_all(self.events_subscribers.iter().map(|entry| {
            let tx = entry.value().clone();
            let event = event.clone();
            async move {
                if let Err(e) = tx.send(event).await {
                    tracing::error!("Failed to send event: {:?}", e);
                }
            }
        }))
        .await;
    }
}

impl FromRef<AppState> for WsState {
    fn from_ref(state: &AppState) -> Self {
        state.ws_state.clone()
    }
}

pub fn setup() -> Router<AppState> {
    let router = Router::new();
    router.route(EVENT_URI, any(ws_handler))
}

async fn ws_handler(State(state): State<WsState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: WsState) {
    let socket_id = state.events_subscribers.len() + 1;
    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    state.events_subscribers.insert(socket_id, tx);

    let (mut sink, mut stream) = socket.split();

    let handler = async {
        while let Some(Ok(_)) = stream.next().await {}
        state.events_subscribers.remove(&socket_id);
    };

    let sender = async {
        while let Some(event) = rx.recv().await {
            let Ok(event) = simd_json::to_vec(&event) else {
                tracing::error!("Failed to serialize event: {:?}", event);
                continue;
            };
            let msg = Message::binary(event);
            if let Err(e) = sink.send(msg).await {
                tracing::error!("Failed to send event: {:?}", e);
            }
        }
    };
    tokio::select! {
        _ = handler => (),
        _ = sender => (),
    }
}
