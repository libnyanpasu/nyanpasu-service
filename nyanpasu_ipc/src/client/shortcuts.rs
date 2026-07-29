use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::{Stream, StreamExt};
use reqwest_websocket::{Message, Upgrade};

use crate::api::{
    self,
    contract::{
        CoreRestart, CoreStart, CoreStop, LogsInspect, LogsRetrieve, NetworkSetDns, Status,
    },
    log::{LOGS_INSPECT_ENDPOINT, LOGS_RETRIEVE_ENDPOINT},
    status::STATUS_ENDPOINT,
    ws::events::{EVENT_URI, Event},
};

use super::{ClientError, Result};

pub use super::Client;

impl Client {
    pub async fn status(&self) -> Result<api::status::StatusResBody<'static>> {
        self.call::<Status>(None)
            .await?
            .data
            .ok_or(ClientError::EmptyData {
                operation: STATUS_ENDPOINT,
            })
    }

    pub async fn start_core(&self, payload: &api::core::start::CoreStartReq<'_>) -> Result<()> {
        self.call::<CoreStart>(Some(payload)).await.map(|_| ())
    }

    pub async fn stop_core(&self) -> Result<()> {
        self.call::<CoreStop>(None).await.map(|_| ())
    }

    pub async fn restart_core(&self) -> Result<()> {
        self.call::<CoreRestart>(None).await.map(|_| ())
    }

    pub async fn inspect_logs(&self) -> Result<api::log::LogsResBody<'static>> {
        self.call::<LogsInspect>(None)
            .await?
            .data
            .ok_or(ClientError::EmptyData {
                operation: LOGS_INSPECT_ENDPOINT,
            })
    }

    pub async fn retrieve_logs(&self) -> Result<api::log::LogsResBody<'static>> {
        self.call::<LogsRetrieve>(None)
            .await?
            .data
            .ok_or(ClientError::EmptyData {
                operation: LOGS_RETRIEVE_ENDPOINT,
            })
    }

    pub async fn set_dns(
        &self,
        payload: &api::network::set_dns::NetworkSetDnsReq<'_>,
    ) -> Result<()> {
        self.call::<NetworkSetDns>(Some(payload)).await.map(|_| ())
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
