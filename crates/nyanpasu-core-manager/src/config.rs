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
    #[cfg_attr(windows, allow(dead_code))]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeriveMode {
    /// Rewrite only the controller keys (Managed-mode normal start).
    ControllerOnly,
    /// Also zero every listener so the new core can start beside the old one.
    ZeroListeners,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RestorePlan {
    pub port: Option<i64>,
    pub socks_port: Option<i64>,
    pub redir_port: Option<i64>,
    pub tproxy_port: Option<i64>,
    pub mixed_port: Option<i64>,
    pub tun_enabled: bool,
}

impl RestorePlan {
    pub(crate) fn to_patch(&self) -> clash_api::ConfigPatch {
        clash_api::ConfigPatch {
            port: self.port,
            socks_port: self.socks_port,
            redir_port: self.redir_port,
            tproxy_port: self.tproxy_port,
            mixed_port: self.mixed_port,
            tun: self.tun_enabled.then(|| clash_api::TunPatch {
                enable: true,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

pub(crate) struct DerivedConfig {
    pub path: camino::Utf8PathBuf,
    pub controller: ResolvedController,
    pub restore: RestorePlan,
}

pub(crate) fn managed_endpoint_path(
    derived_dir: &Utf8Path,
    template: Option<&str>,
    epoch: u64,
) -> String {
    if let Some(template) = template {
        return template.replace("{epoch}", &epoch.to_string());
    }
    #[cfg(windows)]
    {
        let _ = derived_dir;
        format!(r"\\.\pipe\nyanpasu\core-{epoch}")
    }
    #[cfg(not(windows))]
    {
        derived_dir.join(format!("core-{epoch}.sock")).to_string()
    }
}

fn zero_listener(doc: &mut Mapping, key: &str, slot: &mut Option<i64>) {
    let key = Value::String(key.to_owned());
    if let Some(value) = doc.get(&key).and_then(Value::as_i64).filter(|v| *v != 0) {
        *slot = Some(value);
        doc.insert(key, Value::from(0));
    }
}

/// Writes `derived_dir/epoch-{epoch}.yaml` — the caller's config with the
/// controller swapped for the managed epoch endpoint (and, for
/// [`DeriveMode::ZeroListeners`], every listener disabled and recorded).
pub(crate) async fn derive(
    config_path: &Utf8Path,
    derived_dir: &Utf8Path,
    template: Option<&str>,
    epoch: u64,
    mode: DeriveMode,
) -> Result<DerivedConfig, Error> {
    let raw = tokio::fs::read_to_string(config_path).await?;
    let mut doc: Mapping = serde_yaml_ng::from_str(&raw)?;

    let mut restore = RestorePlan::default();
    if mode == DeriveMode::ZeroListeners {
        zero_listener(&mut doc, "port", &mut restore.port);
        zero_listener(&mut doc, "socks-port", &mut restore.socks_port);
        zero_listener(&mut doc, "redir-port", &mut restore.redir_port);
        zero_listener(&mut doc, "tproxy-port", &mut restore.tproxy_port);
        zero_listener(&mut doc, "mixed-port", &mut restore.mixed_port);
        if let Some(tun) = doc
            .get_mut(Value::String("tun".to_owned()))
            .and_then(Value::as_mapping_mut)
        {
            let enable = Value::String("enable".to_owned());
            if tun.get(&enable).and_then(Value::as_bool) == Some(true) {
                restore.tun_enabled = true;
                tun.insert(enable, Value::from(false));
            }
        }
    }

    doc.remove(Value::String("external-controller".to_owned()));
    doc.remove(Value::String("external-controller-pipe".to_owned()));
    doc.remove(Value::String("external-controller-unix".to_owned()));
    let endpoint = managed_endpoint_path(derived_dir, template, epoch);
    #[cfg(windows)]
    doc.insert(
        Value::String("external-controller-pipe".to_owned()),
        Value::String(endpoint.clone()),
    );
    #[cfg(not(windows))]
    doc.insert(
        Value::String("external-controller-unix".to_owned()),
        Value::String(endpoint.clone()),
    );
    let secret = str_value(&doc, "secret");

    tokio::fs::create_dir_all(derived_dir).await?;
    let path = derived_dir.join(format!("epoch-{epoch}.yaml"));
    tokio::fs::write(&path, serde_yaml_ng::to_string(&doc)?).await?;

    #[cfg(windows)]
    let host = clash_api::Host::named_pipe(&endpoint);
    #[cfg(not(windows))]
    let host = clash_api::Host::unix_socket(&endpoint);
    Ok(DerivedConfig {
        path,
        controller: ResolvedController { host, secret },
        restore,
    })
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

    #[tokio::test]
    async fn derive_zeroes_listeners_and_records_the_restore_plan() {
        let dir = tempfile::tempdir().unwrap();
        let dir = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let src = dir.join("config.yaml");
        std::fs::write(
            &src,
            "mixed-port: 7890\nsocks-port: 0\nexternal-controller: 127.0.0.1:9090\nsecret: sc\nmode: rule\ntun:\n  enable: true\n  stack: system\n",
        )
        .unwrap();

        let derived = derive(&src, &dir, None, 7, DeriveMode::ZeroListeners)
            .await
            .unwrap();
        assert_eq!(derived.restore.mixed_port, Some(7890));
        assert_eq!(
            derived.restore.socks_port, None,
            "0 means disabled — not restored"
        );
        assert!(derived.restore.tun_enabled);
        assert_eq!(derived.controller.secret.as_deref(), Some("sc"));

        let out: serde_yaml_ng::Mapping =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&derived.path).unwrap()).unwrap();
        let get = |k: &str| out.get(Value::String(k.to_owned())).cloned();
        assert_eq!(get("mixed-port"), Some(Value::from(0)));
        assert_eq!(
            get("mode"),
            Some(Value::from("rule")),
            "unrelated keys survive"
        );
        assert_eq!(get("external-controller"), None, "HTTP controller stripped");
        let tun = get("tun").unwrap();
        assert_eq!(
            tun.as_mapping()
                .unwrap()
                .get(Value::String("enable".into())),
            Some(&Value::from(false))
        );
        #[cfg(windows)]
        assert!(get("external-controller-pipe").is_some());
        #[cfg(not(windows))]
        assert!(get("external-controller-unix").is_some());
    }

    #[tokio::test]
    async fn derive_controller_only_keeps_listeners() {
        let dir = tempfile::tempdir().unwrap();
        let dir = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let src = dir.join("config.yaml");
        std::fs::write(&src, "mixed-port: 7890\n").unwrap();
        let derived = derive(&src, &dir, None, 3, DeriveMode::ControllerOnly)
            .await
            .unwrap();
        assert!(derived.restore.is_empty());
        let out: serde_yaml_ng::Mapping =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&derived.path).unwrap()).unwrap();
        assert_eq!(
            out.get(Value::String("mixed-port".into())),
            Some(&Value::from(7890))
        );
    }

    #[test]
    fn endpoint_template_substitutes_epoch() {
        let dir = camino::Utf8Path::new("/tmp/x");
        assert_eq!(
            managed_endpoint_path(dir, Some(r"\\.\pipe\ny-{epoch}"), 42),
            r"\\.\pipe\ny-42"
        );
        let default_path = managed_endpoint_path(dir, None, 42);
        assert!(default_path.contains("42"), "got {default_path}");
    }

    #[test]
    fn restore_plan_maps_to_config_patch() {
        let plan = RestorePlan {
            mixed_port: Some(7890),
            tun_enabled: true,
            ..Default::default()
        };
        let patch = plan.to_patch();
        assert_eq!(patch.mixed_port, Some(7890));
        assert_eq!(patch.tun.as_ref().map(|t| t.enable), Some(true));
        assert_eq!(patch.port, None);
    }
}
