use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::{Stream, StreamExt};
use reqwest_websocket::{Message, Upgrade};

use crate::api::{
    self,
    core::{restart::CORE_RESTART_ENDPOINT, start::CORE_START_ENDPOINT, stop::CORE_STOP_ENDPOINT},
    log::{LOGS_INSPECT_ENDPOINT, LOGS_RETRIEVE_ENDPOINT},
    network::set_dns::NETWORK_SET_DNS_ENDPOINT,
    status::STATUS_ENDPOINT,
    ws::events::{EVENT_URI, Event},
};

use super::{ClientError, Result};

pub use super::Client;

impl Client {
    pub async fn status(&self) -> Result<api::status::StatusResBody<'static>> {
        self.send_data(STATUS_ENDPOINT, self.get(STATUS_ENDPOINT))
            .await
    }

    pub async fn start_core(&self, payload: &api::core::start::CoreStartReq<'_>) -> Result<()> {
        self.send_unit(
            CORE_START_ENDPOINT,
            self.post(CORE_START_ENDPOINT).json(payload),
        )
        .await
    }

    pub async fn stop_core(&self) -> Result<()> {
        self.send_unit(CORE_STOP_ENDPOINT, self.post(CORE_STOP_ENDPOINT))
            .await
    }

    pub async fn restart_core(&self) -> Result<()> {
        self.send_unit(CORE_RESTART_ENDPOINT, self.post(CORE_RESTART_ENDPOINT))
            .await
    }

    pub async fn inspect_logs(&self) -> Result<api::log::LogsResBody<'static>> {
        self.send_data(LOGS_INSPECT_ENDPOINT, self.get(LOGS_INSPECT_ENDPOINT))
            .await
    }

    pub async fn retrieve_logs(&self) -> Result<api::log::LogsResBody<'static>> {
        self.send_data(LOGS_RETRIEVE_ENDPOINT, self.get(LOGS_RETRIEVE_ENDPOINT))
            .await
    }

    pub async fn set_dns(
        &self,
        payload: &api::network::set_dns::NetworkSetDnsReq<'_>,
    ) -> Result<()> {
        self.send_unit(
            NETWORK_SET_DNS_ENDPOINT,
            self.post(NETWORK_SET_DNS_ENDPOINT).json(payload),
        )
        .await
    }

    /// Subscribe to the events pushed by the service over `/ws/events`.
    pub async fn events(&self) -> Result<EventStream> {
        let response = self
            .get(EVENT_URI)
            .upgrade()
            .send()
            .await
            .map_err(|source| ClientError::WebSocket {
                operation: EVENT_URI,
                source,
            })?;
        let websocket =
            response
                .into_websocket()
                .await
                .map_err(|source| ClientError::WebSocket {
                    operation: EVENT_URI,
                    source,
                })?;
        let stream = websocket.filter_map(|message| async move {
            let bytes = match message {
                Ok(Message::Binary(bytes)) => bytes,
                Ok(Message::Text(text)) => text.into(),
                // pings are answered internally, everything else is not an event
                Ok(_) => return None,
                Err(source) => {
                    return Some(Err(ClientError::WebSocket {
                        operation: EVENT_URI,
                        source,
                    }));
                }
            };
            Some(
                serde_json::from_slice(&bytes).map_err(|source| ClientError::Decode {
                    operation: EVENT_URI,
                    source,
                }),
            )
        });
        Ok(EventStream {
            inner: Box::pin(stream),
        })
    }
}

/// A stream of [`Event`]s pushed by the service.
pub struct EventStream {
    inner: Pin<Box<dyn Stream<Item = Result<Event>> + Send>>,
}

impl Stream for EventStream {
    type Item = Result<Event>;

    #[inline]
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx)
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").finish_non_exhaustive()
    }
}
