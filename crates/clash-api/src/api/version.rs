use reqwest::Method;

use crate::{Client, Result, retry::RequestMetadata};

/// Mihomo core version information.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Version {
    pub meta: bool,
    pub version: String,
}

impl Client {
    pub async fn version(&self) -> Result<Version> {
        const OPERATION: &str = "version";
        self.send_json(RequestMetadata::new(OPERATION, Method::GET, true), || {
            self.get("/version")
        })
        .await
    }
}
