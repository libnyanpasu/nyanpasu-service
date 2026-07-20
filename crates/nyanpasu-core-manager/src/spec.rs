//! Immutable launch specifications and manager options.

use std::time::Duration;

use camino::Utf8PathBuf;
use nyanpasu_utils::process::{Backoff, RestartPolicy};
use tokio_util::sync::CancellationToken;

use crate::{health::HealthPolicy, kind::CoreKind};

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
    /// Total limit for the initial start (spawn → readiness threshold).
    pub startup_timeout: Duration,
    pub health: HealthPolicy,
    pub restart_policy: RestartPolicy,
    pub backoff: Backoff,
}

impl Default for InstanceOptions {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(30),
            health: HealthPolicy::default(),
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
    /// Rewrite the config to a manager-owned, epoch-parameterized local
    /// transport endpoint. Prerequisite for graceful switching.
    Managed {
        /// Where derived configs (and default unix sockets) live.
        derived_dir: camino::Utf8PathBuf,
        /// Endpoint template containing `{epoch}`; platform default when `None`.
        controller_template: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ManagerOptions {
    pub controller_mode: ControllerMode,
    /// Manager-owned runtime artifact directory. Required for Passthrough;
    /// Managed mode falls back to `derived_dir` for compatibility.
    pub runtime_dir: Option<Utf8PathBuf>,
    pub control_timeout: Duration,
    pub reconcile_timeout: Duration,
    pub stop_timeout: Duration,
    pub cancel_token: CancellationToken,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            controller_mode: ControllerMode::default(),
            runtime_dir: None,
            control_timeout: Duration::from_secs(10),
            reconcile_timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(10),
            cancel_token: CancellationToken::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_options_defaults_match_spec() {
        let o = InstanceOptions::default();
        assert_eq!(o.startup_timeout, Duration::from_secs(30));
        assert_eq!(o.health.interval(), Duration::from_millis(250));
        assert_eq!(o.health.timeout(), Duration::from_secs(1));
        assert_eq!(o.health.failure_threshold().get(), 3);
        assert_eq!(o.health.success_threshold().get(), 1);
        assert_eq!(o.health.start_period(), Duration::ZERO);
        assert_eq!(
            o.restart_policy,
            RestartPolicy::OnFailure { max_restarts: 5 }
        );
    }
}
