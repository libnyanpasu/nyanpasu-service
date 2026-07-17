use std::{fmt::Debug, sync::OnceLock};

use reqwest::{Method, RequestBuilder, StatusCode, Url};
use serde::de::DeserializeOwned;

use crate::{
    SERVICE_PLACEHOLDER,
    api::{R, ResponseCode},
};

pub mod shortcuts;

pub type Result<T> = std::result::Result<T, ClientError>;

/// Synthetic base URL for requests over the local IPC transport.
///
/// Requests travel over a named pipe or unix socket; the HTTP authority is
/// only there to satisfy the protocol, nothing is routed by it.
const LOCAL_TRANSPORT_BASE_URL: &str = "http://localhost/";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    #[error("failed to build the IPC client: {0}")]
    BuildClient(#[source] reqwest::Error),
    #[error("IPC request `{operation}` failed: {source}")]
    Request {
        operation: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("IPC request `{operation}` returned HTTP {status}")]
    HttpStatus {
        operation: &'static str,
        status: StatusCode,
        body: Option<String>,
    },
    #[error("failed to decode the IPC response for `{operation}`: {source}")]
    Decode {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("IPC request `{operation}` failed with {code:?}: {msg}")]
    Server {
        operation: &'static str,
        code: ResponseCode,
        msg: String,
    },
    #[error("IPC request `{operation}` succeeded but carried no data")]
    EmptyData { operation: &'static str },
    #[error("IPC WebSocket `{operation}` failed: {source}")]
    WebSocket {
        operation: &'static str,
        #[source]
        source: reqwest_websocket::Error,
    },
}

/// An IPC client sharing one underlying HTTP client across all operations.
#[derive(Clone, Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: Url,
}

impl Client {
    /// Create a client for the IPC endpoint named by `placeholder`.
    ///
    /// The placeholder maps to `\\.\pipe\{placeholder}` on Windows and
    /// `/var/run/{placeholder}.sock` on Unix.
    pub fn new(placeholder: &str) -> Result<Self> {
        let path = crate::utils::get_name_string(placeholder);
        let builder = reqwest::Client::builder().no_proxy().http1_only();
        #[cfg(windows)]
        let builder = builder.windows_named_pipe(std::path::Path::new(&path));
        #[cfg(unix)]
        let builder = builder.unix_socket(std::path::Path::new(&path));
        let client = builder.build().map_err(ClientError::BuildClient)?;
        Ok(Self {
            client,
            base_url: Url::parse(LOCAL_TRANSPORT_BASE_URL)
                .expect("the local transport base URL must be valid"),
        })
    }

    /// The client for the nyanpasu service's default IPC endpoint.
    pub fn service_default() -> &'static Self {
        static CLIENT: OnceLock<Client> = OnceLock::new();
        CLIENT.get_or_init(|| {
            Self::new(SERVICE_PLACEHOLDER).expect("failed to build the default IPC client")
        })
    }

    /// Access the shared reqwest client for advanced integrations.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    pub(crate) fn request(&self, method: Method, endpoint: &str) -> RequestBuilder {
        let url = self
            .base_url
            .join(endpoint.trim_start_matches('/'))
            .expect("IPC endpoint must be a valid relative URL");
        self.client.request(method, url)
    }

    pub(crate) fn get(&self, endpoint: &str) -> RequestBuilder {
        self.request(Method::GET, endpoint)
    }

    pub(crate) fn post(&self, endpoint: &str) -> RequestBuilder {
        self.request(Method::POST, endpoint)
    }

    pub(crate) async fn send(
        &self,
        operation: &'static str,
        request: RequestBuilder,
    ) -> Result<reqwest::Response> {
        let response = request
            .send()
            .await
            .map_err(|source| ClientError::Request { operation, source })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|source| ClientError::Request { operation, source })?;
        // The service reports failures with the usual response envelope.
        if let Ok(envelope) = serde_json::from_slice::<R<'_, Option<()>>>(&bytes)
            && envelope.code != ResponseCode::Ok
        {
            return Err(ClientError::Server {
                operation,
                code: envelope.code,
                msg: envelope.msg.into_owned(),
            });
        }
        let body =
            Some(String::from_utf8_lossy(&bytes).into_owned()).filter(|body| !body.is_empty());
        Err(ClientError::HttpStatus {
            operation,
            status,
            body,
        })
    }

    /// Send a request and unwrap the data of the response envelope.
    pub(crate) async fn send_data<T>(
        &self,
        operation: &'static str,
        request: RequestBuilder,
    ) -> Result<T>
    where
        T: serde::Serialize + DeserializeOwned + Debug,
    {
        let response = self.send(operation, request).await?;
        let bytes = response
            .bytes()
            .await
            .map_err(|source| ClientError::Request { operation, source })?;
        let envelope = serde_json::from_slice::<R<'static, T>>(&bytes)
            .map_err(|source| ClientError::Decode { operation, source })?;
        if envelope.code != ResponseCode::Ok {
            return Err(ClientError::Server {
                operation,
                code: envelope.code,
                msg: envelope.msg.into_owned(),
            });
        }
        envelope.data.ok_or(ClientError::EmptyData { operation })
    }

    /// Send a request and only check the code of the response envelope.
    pub(crate) async fn send_unit(
        &self,
        operation: &'static str,
        request: RequestBuilder,
    ) -> Result<()> {
        let response = self.send(operation, request).await?;
        let bytes = response
            .bytes()
            .await
            .map_err(|source| ClientError::Request { operation, source })?;
        let envelope = serde_json::from_slice::<R<'static, serde_json::Value>>(&bytes)
            .map_err(|source| ClientError::Decode { operation, source })?;
        if envelope.code != ResponseCode::Ok {
            return Err(ClientError::Server {
                operation,
                code: envelope.code,
                msg: envelope.msg.into_owned(),
            });
        }
        Ok(())
    }
}
