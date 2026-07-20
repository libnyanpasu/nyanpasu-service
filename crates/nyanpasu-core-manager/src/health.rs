//! Version-probe health checking against the core's external controller.

use std::time::Duration;

use crate::{error::Error, spec::ResolvedController};

/// Builds a probe client with the fixed one-second health timeout.
pub(crate) fn build_health_client(
    controller: &ResolvedController,
) -> Result<clash_api::Client, Error> {
    let mut builder = clash_api::Client::builder(controller.host.clone())
        .configure_reqwest(|b| b.timeout(Duration::from_secs(1)));
    if let Some(secret) = &controller.secret {
        builder = builder.secret(secret.as_str());
    }
    Ok(builder.build()?)
}

pub(crate) fn build_control_client(
    controller: &ResolvedController,
    timeout: Duration,
) -> Result<clash_api::Client, Error> {
    let mut builder = clash_api::Client::builder(controller.host.clone())
        .configure_reqwest(|builder| builder.timeout(timeout));
    if let Some(secret) = &controller.secret {
        builder = builder.secret(secret.as_str());
    }
    Ok(builder.build()?)
}

pub(crate) struct HealthCheck {
    client: clash_api::Client,
}

impl HealthCheck {
    pub(crate) fn new(controller: &ResolvedController) -> Result<Self, Error> {
        Ok(Self {
            client: build_health_client(controller)?,
        })
    }

    /// One probe attempt: healthy iff `GET /version` succeeds.
    pub(crate) async fn probe_once(&self) -> bool {
        self.client.version().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(port: u16) -> ResolvedController {
        ResolvedController {
            host: clash_api::Host::http(format!("127.0.0.1:{port}")).unwrap(),
            secret: None,
        }
    }

    #[tokio::test]
    async fn probe_fails_against_closed_port_and_succeeds_against_version_server() {
        // Closed port → probe false.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let probe = HealthCheck::new(&controller(port)).unwrap();
        assert!(!probe.probe_once().await);

        // Minimal /version responder → probe true.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let body = r#"{"meta":true,"version":"t"}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });
        let probe = HealthCheck::new(&controller(port)).unwrap();
        assert!(probe.probe_once().await);
    }
}
