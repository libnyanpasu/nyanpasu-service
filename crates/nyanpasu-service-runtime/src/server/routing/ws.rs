use axum::{
    Router,
    extract::{
        RawQuery, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
    routing::any,
};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use nyanpasu_ipc::api::ws::events::{EVENT_URI, EVENT_VERSION_PARAM, EVENT_VERSION_V2, Event};
use tokio::sync::broadcast::error::RecvError;

use super::AppState;
use crate::server::{
    CoreManager,
    events::{EventHub, WS_LAG_LOG_TARGET},
};

/// The event protocol one connection speaks, negotiated once at upgrade time.
///
/// Per-connection rather than per-hub: the hub carries every variant and each
/// socket filters, which is the only arrangement in which one v2 subscriber
/// cannot expose a new variant to a v1 subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventProtocol {
    V1,
    V2,
}

impl EventProtocol {
    /// Read the version out of a raw query string. Never fails.
    ///
    /// Only the exact token `v=2` opts in; absent, empty, unknown, misspelled
    /// and unparseable all answer v1, the stream every shipped client can
    /// decode. Parsed by hand rather than with `Query`: `Query` deserialises
    /// with `serde_urlencoded`, which **rejects** a repeated key with 400 — a
    /// duplicated `?v=` would kill the upgrade instead of falling back. The
    /// first `v` wins, and percent-encoding is not decoded: the token is
    /// compared literally.
    fn from_query(query: Option<&str>) -> Self {
        let requested = query.unwrap_or_default().split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == EVENT_VERSION_PARAM).then_some(value)
        });
        match requested {
            Some(EVENT_VERSION_V2) => Self::V2,
            _ => Self::V1,
        }
    }

    /// Whether a connection at this version may see `event`.
    ///
    /// Consulted before serialization, so a v1 socket never carries even the
    /// bytes of a variant its decoder does not know.
    fn relays(self, event: &Event) -> bool {
        match self {
            Self::V1 => event.is_protocol_v1(),
            Self::V2 => true,
        }
    }
}

pub fn setup() -> Router<AppState> {
    let router = Router::new();
    router.route(EVENT_URI, any(ws_handler))
}

async fn ws_handler(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> Response {
    let protocol = EventProtocol::from_query(query.as_deref());
    ws.on_upgrade(move |socket| handle_socket(socket, state.hub, state.core_manager, protocol))
}

async fn handle_socket(
    socket: WebSocket,
    hub: EventHub,
    core_manager: CoreManager,
    protocol: EventProtocol,
) {
    // The subscription lives and dies with this task; there is no registry to
    // insert into and no id to collide with. Subscribing *before* the snapshot
    // is read is deliberate: a transition landing in between is then delivered
    // twice rather than lost.
    let mut events = hub.subscribe();
    let (mut sink, mut stream) = socket.split();

    let handler = async { while let Some(Ok(_)) = stream.next().await {} };

    let sender = async {
        // Snapshot-on-connect, v2 only: a v1 client cannot decode the variant,
        // and sending it the legacy state instead would give it a frame it has
        // never received on connect before.
        if protocol == EventProtocol::V2 && !send_snapshot(&mut sink, &core_manager).await {
            return;
        }
        loop {
            match events.recv().await {
                Ok(event) => {
                    if protocol.relays(&event) && !send_event(&mut sink, &event).await {
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
                    // The gap may have swallowed a transition, so a v2 client
                    // is resynchronised exactly as it was on connect. This is
                    // the whole point of the snapshot variant: v1 clients still
                    // have to poll `/status` after a lag.
                    if protocol == EventProtocol::V2
                        && !send_snapshot(&mut sink, &core_manager).await
                    {
                        break;
                    }
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

/// Push the current status as one frame. `false` means the socket is gone.
async fn send_snapshot(
    sink: &mut SplitSink<WebSocket, Message>,
    core_manager: &CoreManager,
) -> bool {
    let event = Event::new_core_status_changed(core_manager.status().await);
    send_event(sink, &event).await
}

/// Frame and write one event. `false` means the socket is gone and the sender
/// must stop; a payload this service cannot serialize is a bug in the payload,
/// not a broken socket, so it is logged and skipped exactly as before.
async fn send_event(sink: &mut SplitSink<WebSocket, Message>, event: &Event) -> bool {
    let Ok(payload) = simd_json::to_vec(event) else {
        tracing::error!("Failed to serialize event: {:?}", event);
        return true;
    };
    match sink.send(Message::binary(payload)).await {
        Ok(()) => true,
        Err(error) => {
            tracing::error!("Failed to send event: {:?}", error);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use nyanpasu_ipc::api::{
        status::{CoreInfos, CoreState, CoreStateDetail},
        ws::events::TraceLog,
    };

    use super::*;

    fn snapshot_event() -> Event {
        Event::new_core_status_changed(CoreInfos {
            r#type: None,
            state: CoreState::Stopped(None),
            state_changed_at: 42,
            config_path: None,
            controller: None,
            health: None,
            revision: None,
            detail: Some(CoreStateDetail::Stopped { reason: None }),
        })
    }

    /// Negotiation is fail-closed and cannot reject: a typo, an empty value, a
    /// future version, a duplicated parameter and a garbled query all answer
    /// v1, which is the only stream every shipped client can decode.
    #[test]
    fn the_version_query_only_opts_in_on_the_exact_token() {
        for query in [
            Some("v=2"),
            Some("foo=bar&v=2"),
            Some("&v=2"),
            Some("v=2&"),
            // the first `v` wins
            Some("v=2&v=1"),
        ] {
            assert_eq!(
                EventProtocol::from_query(query),
                EventProtocol::V2,
                "{query:?}"
            );
        }
        for query in [
            None,
            Some(""),
            Some("v=1"),
            Some("v="),
            Some("v=abc"),
            Some("v=3"),
            Some("v=02"),
            Some("V=2"),
            Some("vv=2"),
            Some("v"),
            // percent-encoding is not decoded: the token is compared literally
            Some("v=%32"),
            Some("%zz"),
            Some("v=1&v=2"),
        ] {
            assert_eq!(
                EventProtocol::from_query(query),
                EventProtocol::V1,
                "{query:?}"
            );
        }
    }

    /// The filter is what makes the new variant safe to broadcast: it runs on
    /// the decoded event, before serialization, so a v1 socket never carries
    /// even the bytes of a variant its decoder does not know (report §7 R1).
    #[test]
    fn a_v1_connection_is_never_offered_the_v2_variant() {
        let log = Event::new_log(TraceLog {
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            level: "INFO".to_owned(),
            message: "hello".to_owned(),
            target: "nyanpasu_service::core".to_owned(),
            fields: Default::default(),
        });
        let legacy_state = Event::new_core_state_changed(CoreState::Running);
        let snapshot = snapshot_event();

        assert!(EventProtocol::V1.relays(&log));
        assert!(EventProtocol::V1.relays(&legacy_state));
        assert!(!EventProtocol::V1.relays(&snapshot));

        assert!(EventProtocol::V2.relays(&log));
        assert!(EventProtocol::V2.relays(&legacy_state));
        assert!(EventProtocol::V2.relays(&snapshot));
    }
}
