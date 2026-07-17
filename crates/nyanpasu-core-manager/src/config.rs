//! Runtime-config introspection (controller endpoint, secret, DNS listener).

use camino::Utf8Path;
use serde_yaml_ng::{Mapping, Value};

use crate::{error::Error, spec::ResolvedController};

#[derive(Debug)]
pub(crate) struct ConfigInfo {
    pub controller: Option<RawController>,
    pub secret: Option<String>,
    pub has_dns_listen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawController {
    Pipe(String),
    Unix(String),
    Http(String),
}

fn str_value(doc: &Mapping, key: &str) -> Option<String> {
    doc.get(Value::String(key.to_owned()))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

pub(crate) async fn inspect(config_path: &Utf8Path) -> Result<ConfigInfo, Error> {
    let raw = tokio::fs::read_to_string(config_path).await?;
    let doc: Mapping = serde_yaml_ng::from_str(&raw)?;
    Ok(inspect_mapping(&doc))
}

fn inspect_mapping(doc: &Mapping) -> ConfigInfo {
    #[cfg(windows)]
    let local = str_value(doc, "external-controller-pipe").map(RawController::Pipe);
    #[cfg(not(windows))]
    let local = str_value(doc, "external-controller-unix").map(RawController::Unix);

    let controller =
        local.or_else(|| str_value(doc, "external-controller").map(RawController::Http));

    let has_dns_listen = doc
        .get(Value::String("dns".to_owned()))
        .and_then(Value::as_mapping)
        .and_then(|dns| dns.get(Value::String("listen".to_owned())))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());

    ConfigInfo {
        controller,
        secret: str_value(doc, "secret"),
        has_dns_listen,
    }
}

pub(crate) fn resolve_controller(info: &ConfigInfo) -> Result<ResolvedController, Error> {
    let raw = info.controller.as_ref().ok_or(Error::ControllerMissing)?;
    let host = match raw {
        RawController::Pipe(path) => clash_api::Host::named_pipe(path),
        RawController::Unix(path) => clash_api::Host::unix_socket(path),
        RawController::Http(addr) => clash_api::Host::http(probe_address(addr))?,
    };
    Ok(ResolvedController {
        host,
        secret: info.secret.clone(),
    })
}

/// `0.0.0.0:9090`, `:9090`, and `[::]:9090` are bind addresses — probe loopback.
fn probe_address(addr: &str) -> String {
    match addr.rsplit_once(':') {
        Some(("0.0.0.0" | "::" | "[::]" | "", port)) => format!("127.0.0.1:{port}"),
        _ => addr.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_of(yaml: &str) -> ConfigInfo {
        let doc: serde_yaml_ng::Mapping = serde_yaml_ng::from_str(yaml).unwrap();
        inspect_mapping(&doc)
    }

    #[test]
    fn extracts_http_controller_and_secret() {
        let info = info_of("external-controller: 127.0.0.1:9090\nsecret: s3cret\n");
        assert_eq!(
            info.controller,
            Some(RawController::Http("127.0.0.1:9090".into()))
        );
        assert_eq!(info.secret.as_deref(), Some("s3cret"));
        assert!(!info.has_dns_listen);
    }

    #[test]
    fn local_transport_takes_priority_over_http() {
        #[cfg(windows)]
        {
            let info = info_of(
                "external-controller: 127.0.0.1:9090\nexternal-controller-pipe: \\\\.\\pipe\\x\n",
            );
            assert_eq!(
                info.controller,
                Some(RawController::Pipe(r"\\.\pipe\x".into()))
            );
        }
        #[cfg(unix)]
        {
            let info = info_of(
                "external-controller: 127.0.0.1:9090\nexternal-controller-unix: /run/x.sock\n",
            );
            assert_eq!(
                info.controller,
                Some(RawController::Unix("/run/x.sock".into()))
            );
        }
    }

    #[test]
    fn detects_dns_listen() {
        let info = info_of("dns:\n  listen: 0.0.0.0:53\n");
        assert!(info.has_dns_listen);
    }

    #[test]
    fn missing_controller_is_a_strict_error() {
        let info = info_of("mixed-port: 7890\n");
        assert!(matches!(
            resolve_controller(&info),
            Err(crate::Error::ControllerMissing)
        ));
    }

    #[test]
    fn wildcard_bind_addresses_probe_via_loopback() {
        assert_eq!(probe_address("0.0.0.0:9090"), "127.0.0.1:9090");
        assert_eq!(probe_address(":9090"), "127.0.0.1:9090");
        assert_eq!(probe_address("[::]:9090"), "127.0.0.1:9090");
        assert_eq!(probe_address("127.0.0.1:9090"), "127.0.0.1:9090");
    }
}
