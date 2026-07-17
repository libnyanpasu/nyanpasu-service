//! Immutable launch specifications and manager options.

use std::time::Duration;

use camino::Utf8PathBuf;
use nyanpasu_utils::process::{Backoff, RestartPolicy};
use tokio_util::sync::CancellationToken;

use crate::kind::CoreKind;

#[derive(Debug, Clone)]
pub struct CoreSpec {
    pub kind: CoreKind,
    /// Resolved by the caller (the service keeps `find_binary_path`).
    pub binary_path: Utf8PathBuf,
    /// Display metadata provided by the caller; not interpreted here.
    pub version: Option<String>,
    pub features: Vec<String>,
}

/// Immutable per-epoch launch spec. Changing the config means a new epoch.
#[derive(Debug, Clone)]
pub struct InstanceSpec {
    pub core: CoreSpec,
    pub config_path: Utf8PathBuf,
    pub working_dir: Utf8PathBuf,
    pub pid_file: Option<Utf8PathBuf>,
    pub options: InstanceOptions,
}

#[derive(Debug, Clone)]
pub struct InstanceOptions {
    /// Total limit for the initial start (spawn → version probe success).
    pub startup_timeout: Duration,
    pub probe_interval: Duration,
    pub restart_policy: RestartPolicy,
    pub backoff: Backoff,
}

impl Default for InstanceOptions {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(30),
            probe_interval: Duration::from_millis(250),
            restart_policy: RestartPolicy::OnFailure { max_restarts: 5 },
            backoff: Backoff::exponential(Duration::from_secs(1), Duration::from_secs(30))
                .with_jitter(),
        }
    }
}

/// The probe/control endpoint an instance actually uses.
#[derive(Debug, Clone)]
pub struct ResolvedController {
    pub host: clash_api::Host,
    pub secret: Option<String>,
}

/// How the manager learns and controls the core's external controller.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub enum ControllerMode {
    /// Start the config as-is; extract the probe endpoint from it.
    #[default]
    Passthrough,
}

#[derive(Debug, Clone, Default)]
pub struct ManagerOptions {
    pub controller_mode: ControllerMode,
    pub cancel_token: CancellationToken,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_options_defaults_match_spec() {
        let o = InstanceOptions::default();
        assert_eq!(o.startup_timeout, Duration::from_secs(30));
        assert_eq!(o.probe_interval, Duration::from_millis(250));
        assert_eq!(o.restart_policy, RestartPolicy::OnFailure { max_restarts: 5 });
    }
}
