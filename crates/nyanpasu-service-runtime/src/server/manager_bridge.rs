use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
};

use camino::{Utf8Path, Utf8PathBuf};
use nyanpasu_core_manager::{
    ApplyOutcome, ConfigRevision, ControllerMode, CoreKind, CoreManager as Manager, CoreSpec,
    CoreState as ManagerCoreState, CoreStatus, Error as ManagerError, HealthState, HealthStatus,
    Host, InstanceOptions, InstanceSpec, LogFrame, LogStream, ManagerOptions, RevisionId,
};
use nyanpasu_ipc::api::{
    R, RBuilder,
    core::apply::{ApplyOutcomeKind, CoreApplyData},
    error_kind,
    status::{
        ConfigRevisionInfo, CoreControllerInfo, CoreHealthInfo, CoreHealthState, CoreInfos,
        CoreState, CoreStateDetail, RevisionIdInfo,
    },
    ws::events::Event as WsEvent,
};
use nyanpasu_utils::core::{ClashCoreType, CoreType};
use parking_lot::RwLock;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::broadcast::error::RecvError;
use tracing::instrument;

use super::{consts::RuntimeInfos, events::EventHub};

const CORE_LOG_TARGET: &str = "nyanpasu_service::core";

/// Legacy wire strings the GUI branches on. These are protocol, not
/// diagnostics: changing any of them is a breaking change to clash-nyanpasu.
pub(crate) const MSG_CORE_ALREADY_RUNNING: &str = "core is already running";
pub(crate) const MSG_CORE_ALREADY_STOPPED: &str = "core is already stopped";
pub(crate) const MSG_CORE_NOT_STARTED: &str = "core have not been started yet";

/// A failed operation, carrying its wire `error_kind` next to its message.
///
/// `start`/`stop`/`restart` predate `error_kind` and keep returning
/// `anyhow::Error`; the S8 operations need the classification, and the only
/// place it can be derived without downcasting is where the `ManagerError` is
/// still typed.
pub(crate) struct OpError {
    kind: Option<&'static str>,
    message: String,
}

impl OpError {
    /// A failure the service cannot classify. Omitting the kind is correct
    /// here: a guessed one is worse than none.
    fn plain(message: impl Into<String>) -> Self {
        Self {
            kind: None,
            message: message.into(),
        }
    }

    /// The error envelope for this failure, `error_kind` included.
    pub(crate) fn into_envelope<T>(self) -> R<'static, T>
    where
        T: Serialize + DeserializeOwned + std::fmt::Debug,
    {
        RBuilder::other_error_with_kind(Cow::Owned(self.message), self.kind.map(Cow::Borrowed))
    }
}

impl From<ManagerError> for OpError {
    fn from(error: ManagerError) -> Self {
        Self {
            kind: map_error_kind(&error),
            message: match &error {
                // The legacy wire string the GUI already branches on, so
                // `apply` on a stopped core reads exactly like `restart` on
                // one instead of inventing a second phrasing.
                ManagerError::NotStarted => MSG_CORE_NOT_STARTED.to_owned(),
                _ => error.to_string(),
            },
        }
    }
}

impl From<anyhow::Error> for OpError {
    fn from(error: anyhow::Error) -> Self {
        Self::plain(error.to_string())
    }
}

struct Inner {
    manager: Manager,
    /// Wire-type echo: the manager knows nothing about the alpha variants.
    /// Behind an `Arc` so the state bridge can read it without capturing the
    /// whole adapter — an `Arc<Inner>` inside that task would keep the manager
    /// alive, so its watch channel would never close and the task would never
    /// exit.
    requested_core: Arc<RwLock<Option<CoreType>>>,
    /// Serializes adapter-level control ops and carries the closing latch.
    control: tokio::sync::Mutex<ControlState>,
}

struct ControlState {
    closing: bool,
}

#[derive(Clone)]
pub struct CoreManagerService {
    inner: Arc<Inner>,
}

impl CoreManagerService {
    pub async fn new(runtime_dir: Utf8PathBuf) -> Result<Self, anyhow::Error> {
        let manager = Manager::new(ManagerOptions {
            controller_mode: ControllerMode::Passthrough,
            runtime_dir: Some(runtime_dir),
            ..ManagerOptions::default()
        })
        .await?;
        Ok(Self {
            inner: Arc::new(Inner {
                manager,
                requested_core: Arc::new(RwLock::new(None)),
                control: tokio::sync::Mutex::new(ControlState { closing: false }),
            }),
        })
    }

    /// State → ws events and core logs → tracing.
    pub fn spawn_bridges(&self, hub: EventHub) {
        let mut states = self.inner.manager.subscribe();
        // Only the echo is cloned in, never the whole adapter: see `Inner`.
        let requested_core = self.inner.requested_core.clone();
        tokio::spawn(async move {
            let raw = states.borrow_and_update().clone();
            let mut last = map_core_state(&raw.state);
            while states.changed().await.is_ok() {
                let raw = states.borrow_and_update().clone();
                let next = map_core_state(&raw.state);
                // v2 leads and is never suppressed: it carries every manager
                // transition, including the ones the two-valued v1 state cannot
                // express (Starting, Restarting, and the Stopping/Switching
                // arrivals the guards below drop). Emitting it here rather than
                // after the guards is what keeps the v1 path below untouched.
                // Deliberately no `tracing::info!` for it — that would re-enter
                // the hub as an `Event::Log` and change what v1 streams carry.
                let requested = requested_core.read().clone();
                hub.send(WsEvent::new_core_status_changed(project_core_infos(
                    &raw, requested,
                )));
                if matches!(
                    raw.state,
                    ManagerCoreState::Stopping { .. } | ManagerCoreState::Switching { .. }
                ) && matches!(last, CoreState::Stopped(_))
                {
                    continue;
                }
                if same_ipc_state(&last, &next) {
                    continue;
                }
                tracing::info!("State changed: {:?}", next);
                hub.send(WsEvent::new_core_state_changed(next.clone()));
                last = next;
            }
        });

        let mut logs = self.inner.manager.subscribe_logs();
        tokio::spawn(async move {
            loop {
                match logs.recv().await {
                    Ok(frame) => forward_log(frame),
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!("core log bridge dropped {skipped} frames")
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    /// Stop the core and clean runtime artifacts. Idempotent; errors are logged, not returned.
    pub async fn shutdown(&self) {
        let mut control = self.inner.control.lock().await;
        if control.closing {
            return;
        }
        control.closing = true;
        drop(control);
        if let Err(error) = self.inner.manager.shutdown().await {
            tracing::error!("failed to stop the core on shutdown: {error}");
        }
    }

    #[instrument(skip(self, infos))]
    pub async fn start(
        &self,
        infos: &RuntimeInfos,
        core_type: &CoreType,
        config_path: &Utf8Path,
    ) -> Result<(), anyhow::Error> {
        let control = self.inner.control.lock().await;
        if control.closing {
            anyhow::bail!("service is shutting down");
        }
        let config_path = config_path.canonicalize_utf8()?;
        let config_path =
            Utf8PathBuf::from_path_buf(dunce::simplified(config_path.as_std_path()).to_path_buf())
                .expect("a canonical UTF-8 path stays UTF-8 after simplification");
        tokio::fs::metadata(&config_path).await?; // check if the file exists
        if !matches!(
            self.inner.manager.status().state,
            ManagerCoreState::Stopped { .. }
        ) {
            anyhow::bail!(MSG_CORE_ALREADY_RUNNING);
        }
        let spec = self.instance_spec(infos, core_type, config_path)?;
        tracing::info!(
            core_type = %core_type,
            kind = %spec.core.kind,
            working_dir = %spec.working_dir,
            binary_path = %spec.core.binary_path,
            config_path = %spec.config_path,
            "Starting Core"
        );
        self.inner.manager.start(spec).await?;
        *self.inner.requested_core.write() = Some(core_type.clone());
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), anyhow::Error> {
        let control = self.inner.control.lock().await;
        if control.closing {
            anyhow::bail!("service is shutting down");
        }
        match self.inner.manager.stop().await {
            Ok(()) => Ok(()),
            Err(ManagerError::NotStarted) => anyhow::bail!(MSG_CORE_ALREADY_STOPPED),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn restart(&self) -> Result<(), anyhow::Error> {
        let control = self.inner.control.lock().await;
        if control.closing {
            anyhow::bail!("service is shutting down");
        }
        match self.inner.manager.restart().await {
            Ok(_outcome) => Ok(()),
            Err(ManagerError::NotStarted) => anyhow::bail!(MSG_CORE_NOT_STARTED),
            Err(error) => Err(error.into()),
        }
    }

    /// Apply `config_file` to the running core.
    ///
    /// The manager classifies the change and routes it: in-place patch, reload,
    /// same-epoch restart with rollback, or a full core switch when the process
    /// spec changed (which is what a different `core_type` produces). A stopped
    /// core is an error, never an implicit start (report §7 R2).
    #[instrument(skip(self, infos))]
    pub async fn apply(
        &self,
        infos: &RuntimeInfos,
        core_type: &CoreType,
        config_file: &Path,
        expected_revision: Option<&RevisionIdInfo>,
    ) -> Result<CoreApplyData, OpError> {
        let control = self.inner.control.lock().await;
        if control.closing {
            return Err(OpError::plain("service is shutting down"));
        }
        let config_path = canonical_config_path(config_file)?;
        let spec = self.instance_spec(infos, core_type, config_path)?;
        let outcome = self
            .inner
            .manager
            .apply_config(spec, expected_revision.map(map_revision_id))
            .await?;
        let data = map_apply_outcome(&outcome);
        tracing::info!(
            outcome = ?data.outcome,
            epoch = data.revision.epoch,
            generation = data.revision.generation,
            "Applied config"
        );
        // A rollback put the *old* spec back, so the wire echo must not claim
        // the core type that was asked for.
        if data.outcome != ApplyOutcomeKind::RolledBack {
            *self.inner.requested_core.write() = Some(core_type.clone());
        }
        Ok(data)
    }

    /// Dry-run a config against a core binary. Never touches the running core.
    #[instrument(skip(self, infos))]
    pub async fn check(
        &self,
        infos: &RuntimeInfos,
        core_type: &CoreType,
        config_file: &Path,
    ) -> Result<(), OpError> {
        {
            // Released before the check runs: it spawns the core binary, and
            // holding the adapter latch across an external process would block
            // start/stop/restart for as long as that takes.
            let control = self.inner.control.lock().await;
            if control.closing {
                return Err(OpError::plain("service is shutting down"));
            }
        }
        let config_path = canonical_config_path(config_file)?;
        let spec = self.instance_spec(infos, core_type, config_path)?;
        self.inner.manager.check_config(&spec).await?;
        Ok(())
    }

    /// Clear the quarantine latch left by an epoch whose death could not be
    /// confirmed. Idempotent: succeeds when nothing was quarantined.
    #[instrument(skip(self))]
    pub async fn recover(&self) -> Result<(), OpError> {
        let control = self.inner.control.lock().await;
        if control.closing {
            return Err(OpError::plain("service is shutting down"));
        }
        self.inner.manager.recover_quarantine().await?;
        Ok(())
    }

    pub async fn status(&self) -> CoreInfos {
        project_core_infos(
            &self.inner.manager.status(),
            self.inner.requested_core.read().clone(),
        )
    }

    fn instance_spec(
        &self,
        infos: &RuntimeInfos,
        core_type: &CoreType,
        config_path: Utf8PathBuf,
    ) -> Result<InstanceSpec, anyhow::Error> {
        let working_dir =
            Utf8PathBuf::from_path_buf(infos.nyanpasu_data_dir.clone()).map_err(|path| {
                anyhow::anyhow!("nyanpasu data dir is not UTF-8: {}", path.display())
            })?;
        let binary_path = Utf8PathBuf::from_path_buf(find_binary_path(infos, core_type)?)
            .map_err(|path| anyhow::anyhow!("core binary path is not UTF-8: {}", path.display()))?;
        let kind = core_kind(core_type)?;
        Ok(InstanceSpec {
            core: CoreSpec {
                kind,
                binary_path,
                version: None,
                features: Vec::new(),
            },
            config_path,
            working_dir,
            // The manager owns the pid record and points it at its runtime dir.
            pid_file: None,
            options: InstanceOptions::default(),
        })
    }
}

/// One-way wire → manager mapping; the manager has no alpha variants.
fn core_kind(core_type: &CoreType) -> Result<CoreKind, anyhow::Error> {
    match core_type {
        CoreType::Clash(ClashCoreType::Mihomo | ClashCoreType::MihomoAlpha) => Ok(CoreKind::Mihomo),
        CoreType::Clash(ClashCoreType::ClashRust | ClashCoreType::ClashRustAlpha) => {
            Ok(CoreKind::ClashRust)
        }
        CoreType::Clash(ClashCoreType::ClashPremium) => Ok(CoreKind::ClashPremium),
        CoreType::Clash(ClashCoreType::Meow) => Ok(CoreKind::Meow),
        CoreType::SingBox => anyhow::bail!("sing-box is not a supported core"),
    }
}

/// Lossy projection onto the unchanged wire state.
fn map_core_state(state: &ManagerCoreState) -> CoreState {
    match state {
        ManagerCoreState::Running { .. }
        | ManagerCoreState::Switching { .. }
        | ManagerCoreState::Stopping { .. } => CoreState::Running,
        ManagerCoreState::Starting { .. } | ManagerCoreState::Restarting { .. } => {
            CoreState::Stopped(None)
        }
        ManagerCoreState::Stopped { reason } => {
            CoreState::Stopped(reason.as_ref().map(ToString::to_string))
        }
        // `CoreState` is `#[non_exhaustive]`; an unknown state is not proven running.
        _ => CoreState::Stopped(None),
    }
}

/// The controller endpoint, minus anything credential-bearing.
///
/// The manager never publishes the secret (only `ResolvedController.host`
/// reaches the watch channel), but an HTTP endpoint is parsed from the
/// caller's own `external-controller` value, which can carry userinfo — so
/// strip it here rather than trusting the shape of that string.
fn map_controller(host: &Host) -> Option<CoreControllerInfo> {
    match host {
        Host::NamedPipe(path) => Some(CoreControllerInfo::NamedPipe(path.clone())),
        Host::UnixSocket(path) => Some(CoreControllerInfo::UnixSocket(path.clone())),
        Host::Http(url) => {
            let mut url = url.clone();
            let _ = url.set_username("");
            let _ = url.set_password(None);
            Some(CoreControllerInfo::Http(url.to_string()))
        }
        // `Host` is `#[non_exhaustive]`; an unknown transport is reported as
        // absent rather than guessed at.
        _ => None,
    }
}

fn map_health(health: &HealthStatus) -> CoreHealthInfo {
    CoreHealthInfo {
        state: match health.state {
            HealthState::Starting => CoreHealthState::Starting,
            HealthState::Healthy => CoreHealthState::Healthy,
            HealthState::Unhealthy => CoreHealthState::Unhealthy,
            // `HealthState` is `#[non_exhaustive]`; an unknown state is not
            // proven healthy.
            _ => CoreHealthState::Unhealthy,
        },
        changed_at: health.changed_at,
        consecutive_failures: health.consecutive_failures,
        last_error: health.last_error.clone(),
        last_success_at: health.last_success_at,
    }
}

/// `runtime_path` is dropped on purpose: it points inside the manager's
/// 0o700 runtime directory, which the client cannot read.
fn map_revision(revision: &ConfigRevision) -> ConfigRevisionInfo {
    ConfigRevisionInfo {
        epoch: revision.epoch,
        generation: revision.generation,
        source_hash: revision.source_hash.clone(),
        effective_hash: revision.effective_hash.clone(),
    }
}

/// The wire CAS token, as the manager compares it.
fn map_revision_id(info: &RevisionIdInfo) -> RevisionId {
    RevisionId {
        epoch: info.epoch,
        generation: info.generation,
        effective_hash: info.effective_hash.clone(),
    }
}

/// Project an apply result onto the wire.
///
/// `DurabilityUncertain` is a wrapper, not an outcome, and the apply path can
/// wrap twice — a commit warning around a restore warning
/// (`manager/apply.rs:148` around `:445`) — so unwrap to the real outcome and
/// keep every warning. `ApplyOutcome` is not `#[non_exhaustive]`: if the
/// manager grows a variant, this match must fail to compile rather than
/// silently mislabel it.
fn map_apply_outcome(outcome: &ApplyOutcome) -> CoreApplyData {
    let mut warnings = Vec::new();
    let mut current = outcome;
    while let ApplyOutcome::DurabilityUncertain { outcome, warning } = current {
        warnings.push(warning.clone());
        current = &**outcome;
    }
    let (kind, revision, failed_apply) = match current {
        ApplyOutcome::Noop { revision } => (ApplyOutcomeKind::Noop, revision, None),
        ApplyOutcome::Patched { revision } => (ApplyOutcomeKind::Patched, revision, None),
        ApplyOutcome::Reloaded { revision } => (ApplyOutcomeKind::Reloaded, revision, None),
        // The core-switch path reports `Restarted` too (`manager/apply.rs:531`),
        // so `ApplyOutcomeKind::Switched` is never produced here. Do not try to
        // infer it from an epoch change: the pre-call epoch can only be read
        // outside the manager's control lock.
        ApplyOutcome::Restarted { revision } => (ApplyOutcomeKind::Restarted, revision, None),
        ApplyOutcome::RolledBack {
            revision,
            failed_apply,
        } => (
            ApplyOutcomeKind::RolledBack,
            revision,
            Some(failed_apply.clone()),
        ),
        ApplyOutcome::DurabilityUncertain { .. } => {
            unreachable!("unwrapped by the loop above")
        }
    };
    CoreApplyData {
        outcome: kind,
        revision: map_revision(revision),
        warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        failed_apply,
    }
}

/// The wire classification for a manager error, or `None` when this change does
/// not classify it yet (report P3 maps the rest).
///
/// `Error` is `#[non_exhaustive]`, so the wildcard arm is mandatory; it must
/// stay "no kind", never a guess.
fn map_error_kind(error: &ManagerError) -> Option<&'static str> {
    match error {
        ManagerError::NotStarted => Some(error_kind::NOT_STARTED),
        ManagerError::AlreadyRunning => Some(error_kind::ALREADY_RUNNING),
        ManagerError::RevisionConflict { .. } => Some(error_kind::REVISION_CONFLICT),
        ManagerError::ManagerQuarantined { .. } => Some(error_kind::QUARANTINED),
        ManagerError::ConfigCheckFailed(_) => Some(error_kind::CONFIG_CHECK_FAILED),
        ManagerError::ConfigNotFound(_) => Some(error_kind::CONFIG_NOT_FOUND),
        ManagerError::BinaryNotFound(_) => Some(error_kind::BINARY_NOT_FOUND),
        ManagerError::InvalidConfig(_) | ManagerError::Yaml(_) => Some(error_kind::INVALID_CONFIG),
        ManagerError::ControllerMissing => Some(error_kind::CONTROLLER_MISSING),
        ManagerError::ApplyFailed(_) => Some(error_kind::APPLY_FAILED),
        ManagerError::ApplyRollbackFailed { .. } => Some(error_kind::APPLY_ROLLBACK_FAILED),
        ManagerError::StopUnconfirmed(_) => Some(error_kind::STOP_UNCONFIRMED),
        // The durability wrapper is a warning around a real failure; report the
        // failure's kind so a caller can still branch on it.
        ManagerError::DurabilityUncertain { source, .. } => map_error_kind(source),
        _ => None,
    }
}

/// The lossless counterpart to `map_core_state`.
fn map_state_detail(state: &ManagerCoreState) -> Option<CoreStateDetail> {
    match state {
        ManagerCoreState::Stopped { reason } => Some(CoreStateDetail::Stopped {
            reason: reason.as_ref().map(ToString::to_string),
        }),
        ManagerCoreState::Starting { epoch } => Some(CoreStateDetail::Starting { epoch: *epoch }),
        ManagerCoreState::Running { epoch, pid } => Some(CoreStateDetail::Running {
            epoch: *epoch,
            pid: *pid,
        }),
        ManagerCoreState::Restarting { epoch, attempt } => Some(CoreStateDetail::Restarting {
            epoch: *epoch,
            attempt: *attempt,
        }),
        ManagerCoreState::Switching { from, to } => Some(CoreStateDetail::Switching {
            from: *from,
            to: *to,
        }),
        ManagerCoreState::Stopping { epoch } => Some(CoreStateDetail::Stopping { epoch: *epoch }),
        // `CoreState` is `#[non_exhaustive]`; an unknown state has no faithful
        // projection, so it is reported as absent.
        _ => None,
    }
}

/// The wire type carries no `PartialEq`, so equality is spelled out here.
fn same_ipc_state(previous: &CoreState, next: &CoreState) -> bool {
    match (previous, next) {
        (CoreState::Running, CoreState::Running) => true,
        (CoreState::Stopped(previous), CoreState::Stopped(next)) => previous == next,
        _ => false,
    }
}

/// The wire projection of a manager snapshot.
///
/// One function so `/status` and the v2 `CoreStatusChanged` frame cannot drift:
/// the event *is* the snapshot (report §4 P1-A). `state` stays the lossy
/// two-valued field v1 clients depend on; `detail` is the faithful six-state
/// view beside it.
fn project_core_infos(status: &CoreStatus, requested_core: Option<CoreType>) -> CoreInfos {
    CoreInfos {
        r#type: requested_core,
        state: map_core_state(&status.state),
        state_changed_at: status.changed_at,
        config_path: status
            .spec
            .as_ref()
            .map(|spec| spec.config_path.as_std_path().to_path_buf()),
        controller: status.controller.as_ref().and_then(map_controller),
        health: status.health.as_ref().map(map_health),
        revision: status.revision.as_ref().map(map_revision),
        detail: map_state_detail(&status.state),
    }
}

fn forward_log(frame: LogFrame) {
    let kind = frame.kind;
    let epoch = frame.epoch;
    let core_level = frame.level;
    let raw = frame.raw;
    match frame.stream {
        LogStream::Stdout => {
            tracing::info!(target: CORE_LOG_TARGET, ?core_level, %kind, epoch, "{raw}")
        }
        LogStream::Stderr => {
            tracing::error!(target: CORE_LOG_TARGET, ?core_level, %kind, epoch, "{raw}")
        }
    }
}

// TODO: support system path search via a config or flag
/// Search the binary path of the core: Data Dir -> Sidecar Dir
fn find_binary_path(infos: &RuntimeInfos, core_type: &CoreType) -> std::io::Result<PathBuf> {
    let data_dir = &infos.nyanpasu_data_dir;
    let binary_path = data_dir.join(core_type.get_executable_name());
    if binary_path.exists() {
        return Ok(binary_path);
    }
    let app_dir = &infos.nyanpasu_app_dir;
    let binary_path = app_dir.join(core_type.get_executable_name());
    if binary_path.exists() {
        return Ok(binary_path);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{} not found", core_type.get_executable_name()),
    ))
}

/// The absolute, non-verbatim, UTF-8 path the manager is handed for a config.
///
/// Same shape as `start`'s inline version: canonicalizing makes the source path
/// a stable identity, and `dunce::simplified` strips the Windows `\\?\` prefix
/// the core's own command line cannot use. Canonicalization is also the
/// existence check.
fn canonical_config_path(config_file: &Path) -> Result<Utf8PathBuf, OpError> {
    let canonical = config_file.canonicalize().map_err(|error| {
        OpError::plain(format!(
            "failed to resolve config path {}: {error}",
            config_file.display()
        ))
    })?;
    Utf8PathBuf::from_path_buf(dunce::simplified(&canonical).to_path_buf())
        .map_err(|path| OpError::plain(format!("config path is not UTF-8: {}", path.display())))
}

#[cfg(test)]
mod tests {
    use nyanpasu_core_manager::StopReason;

    use super::*;

    fn simulate(states: &[ManagerCoreState]) -> Vec<String> {
        let mut last = CoreState::Stopped(None);
        let mut emitted = Vec::new();
        for raw in states {
            let next = map_core_state(raw);
            if matches!(
                raw,
                ManagerCoreState::Stopping { .. } | ManagerCoreState::Switching { .. }
            ) && matches!(last, CoreState::Stopped(_))
            {
                continue;
            }
            if same_ipc_state(&last, &next) {
                continue;
            }
            emitted.push(format!("{next:?}"));
            last = next;
        }
        emitted
    }

    // covered by S2 route-level tests

    #[test]
    fn manager_states_map_onto_the_wire_states() {
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Stopped { reason: None })
            ),
            "Stopped(None)"
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Starting { epoch: 1 })
            ),
            "Stopped(None)"
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Running { epoch: 1, pid: 42 })
            ),
            "Running"
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Restarting {
                    epoch: 1,
                    attempt: 2,
                })
            ),
            "Stopped(None)"
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Switching {
                    from: Some(1),
                    to: 2,
                })
            ),
            "Running"
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Stopping { epoch: 2 })
            ),
            "Running"
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Stopped {
                    reason: Some(StopReason::Finished),
                })
            ),
            r#"Stopped(Some("core exited"))"#
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Stopped {
                    reason: Some(StopReason::User),
                })
            ),
            r#"Stopped(Some("stopped by user"))"#
        );
        assert_eq!(
            format!(
                "{:?}",
                map_core_state(&ManagerCoreState::Stopped {
                    reason: Some(StopReason::Error("boom".to_owned())),
                })
            ),
            r#"Stopped(Some("boom"))"#
        );
    }

    #[test]
    fn core_types_map_onto_manager_kinds() {
        let cases = [
            (ClashCoreType::Mihomo, CoreKind::Mihomo),
            (ClashCoreType::MihomoAlpha, CoreKind::Mihomo),
            (ClashCoreType::ClashRust, CoreKind::ClashRust),
            (ClashCoreType::ClashRustAlpha, CoreKind::ClashRust),
            (ClashCoreType::ClashPremium, CoreKind::ClashPremium),
            (ClashCoreType::Meow, CoreKind::Meow),
        ];
        for (core_type, expected) in cases {
            assert_eq!(core_kind(&CoreType::Clash(core_type)).unwrap(), expected);
        }
        assert!(core_kind(&CoreType::SingBox).is_err());
    }

    #[test]
    fn the_state_bridge_forwards_only_real_transitions() {
        let states = [
            ManagerCoreState::Starting { epoch: 1 },
            ManagerCoreState::Running { epoch: 1, pid: 42 },
            ManagerCoreState::Switching {
                from: Some(1),
                to: 2,
            },
            ManagerCoreState::Stopping { epoch: 2 },
            ManagerCoreState::Running { epoch: 2, pid: 43 },
            ManagerCoreState::Stopped {
                reason: Some(StopReason::User),
            },
            ManagerCoreState::Stopped {
                reason: Some(StopReason::Error("boom".to_owned())),
            },
        ];

        assert_eq!(
            simulate(&states),
            [
                "Running",
                r#"Stopped(Some("stopped by user"))"#,
                r#"Stopped(Some("boom"))"#,
            ]
        );
    }

    #[test]
    fn terminal_stop_is_not_resurrected_by_shutdown() {
        let states = [
            ManagerCoreState::Running { epoch: 1, pid: 42 },
            ManagerCoreState::Stopped {
                reason: Some(StopReason::Error("boom".to_owned())),
            },
            ManagerCoreState::Stopping { epoch: 1 },
            ManagerCoreState::Stopped {
                reason: Some(StopReason::User),
            },
        ];

        assert_eq!(
            simulate(&states),
            [
                "Running",
                r#"Stopped(Some("boom"))"#,
                r#"Stopped(Some("stopped by user"))"#,
            ]
        );
    }

    #[test]
    fn manager_states_map_onto_the_wire_state_detail() {
        let cases = [
            (
                ManagerCoreState::Stopped { reason: None },
                CoreStateDetail::Stopped { reason: None },
            ),
            (
                ManagerCoreState::Stopped {
                    reason: Some(StopReason::User),
                },
                CoreStateDetail::Stopped {
                    reason: Some("stopped by user".to_owned()),
                },
            ),
            (
                ManagerCoreState::Starting { epoch: 1 },
                CoreStateDetail::Starting { epoch: 1 },
            ),
            (
                ManagerCoreState::Running { epoch: 1, pid: 42 },
                CoreStateDetail::Running { epoch: 1, pid: 42 },
            ),
            (
                ManagerCoreState::Restarting {
                    epoch: 1,
                    attempt: 2,
                },
                CoreStateDetail::Restarting {
                    epoch: 1,
                    attempt: 2,
                },
            ),
            (
                ManagerCoreState::Switching {
                    from: Some(1),
                    to: 2,
                },
                CoreStateDetail::Switching {
                    from: Some(1),
                    to: 2,
                },
            ),
            (
                ManagerCoreState::Stopping { epoch: 2 },
                CoreStateDetail::Stopping { epoch: 2 },
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(map_state_detail(&raw), Some(expected));
        }
    }

    /// The detail projection must disagree with the lossy wire state exactly
    /// where the report says it does: a crash loop and a start-up both look
    /// stopped on the old field.
    #[test]
    fn the_detail_field_recovers_what_the_wire_state_flattens() {
        let restarting = ManagerCoreState::Restarting {
            epoch: 1,
            attempt: 3,
        };
        assert!(matches!(
            map_core_state(&restarting),
            CoreState::Stopped(None)
        ));
        assert_eq!(
            map_state_detail(&restarting),
            Some(CoreStateDetail::Restarting {
                epoch: 1,
                attempt: 3,
            })
        );
    }

    #[test]
    fn controller_hosts_map_onto_the_wire_controller() {
        assert_eq!(
            map_controller(&Host::named_pipe(r"\\.\pipe\nyanpasu\core-1")),
            Some(CoreControllerInfo::NamedPipe(std::path::PathBuf::from(
                r"\\.\pipe\nyanpasu\core-1"
            )))
        );
        assert_eq!(
            map_controller(&Host::unix_socket("/run/nyanpasu/core-1.sock")),
            Some(CoreControllerInfo::UnixSocket(std::path::PathBuf::from(
                "/run/nyanpasu/core-1.sock"
            )))
        );
        assert_eq!(
            map_controller(&Host::http("127.0.0.1:9090").unwrap()),
            Some(CoreControllerInfo::Http(
                "http://127.0.0.1:9090/".to_owned()
            ))
        );
    }

    /// The HTTP endpoint is parsed from the caller's own config value; if that
    /// value carried userinfo, it must not reach the wire.
    #[test]
    fn http_controller_credentials_never_reach_the_wire() {
        for raw in [
            "http://user:pass@127.0.0.1:9090",
            "http://user@127.0.0.1:9090",
        ] {
            assert_eq!(
                map_controller(&Host::http(raw).unwrap()),
                Some(CoreControllerInfo::Http(
                    "http://127.0.0.1:9090/".to_owned()
                )),
                "raw = {raw}"
            );
        }
    }

    #[test]
    fn the_revision_projection_drops_the_runtime_path() {
        let revision = ConfigRevision {
            epoch: 3,
            generation: 7,
            source_hash: "0123456789abcdef".to_owned(),
            effective_hash: "fedcba9876543210".to_owned(),
            runtime_path: "/srv/data/core-runtime/config-3.yaml".into(),
        };
        assert_eq!(
            map_revision(&revision),
            ConfigRevisionInfo {
                epoch: 3,
                generation: 7,
                source_hash: "0123456789abcdef".to_owned(),
                effective_hash: "fedcba9876543210".to_owned(),
            }
        );
    }

    #[test]
    fn health_states_map_onto_the_wire_health() {
        // `HealthStatus::starting()` is `pub(crate)` to the manager
        // (`state.rs:41`) and unreachable from here, but all five fields are
        // `pub` and the struct is not `#[non_exhaustive]`.
        let health = HealthStatus {
            state: HealthState::Unhealthy,
            changed_at: 1_700_000_000_123,
            consecutive_failures: 4,
            last_error: Some("probe timed out".to_owned()),
            last_success_at: Some(1_700_000_000_000),
        };
        let mapped = map_health(&health);
        assert_eq!(mapped.state, CoreHealthState::Unhealthy);
        assert_eq!(mapped.changed_at, 1_700_000_000_123);
        assert_eq!(mapped.consecutive_failures, 4);
        assert_eq!(mapped.last_error.as_deref(), Some("probe timed out"));
        assert_eq!(mapped.last_success_at, Some(1_700_000_000_000));
    }

    fn revision(generation: u64) -> ConfigRevision {
        ConfigRevision {
            epoch: 3,
            generation,
            source_hash: "0123456789abcdef".to_owned(),
            effective_hash: "fedcba9876543210".to_owned(),
            runtime_path: "/srv/data/core-runtime/config-3.yaml".into(),
        }
    }

    #[test]
    fn apply_outcomes_map_onto_the_wire_kinds() {
        let cases = [
            (
                ApplyOutcome::Noop {
                    revision: revision(7),
                },
                ApplyOutcomeKind::Noop,
            ),
            (
                ApplyOutcome::Patched {
                    revision: revision(8),
                },
                ApplyOutcomeKind::Patched,
            ),
            (
                ApplyOutcome::Reloaded {
                    revision: revision(9),
                },
                ApplyOutcomeKind::Reloaded,
            ),
            (
                ApplyOutcome::Restarted {
                    revision: revision(10),
                },
                ApplyOutcomeKind::Restarted,
            ),
        ];
        for (outcome, expected) in cases {
            let data = map_apply_outcome(&outcome);
            assert_eq!(data.outcome, expected);
            assert_eq!(data.revision.effective_hash, "fedcba9876543210");
            assert!(data.warning.is_none());
            assert!(data.failed_apply.is_none());
        }
    }

    /// The report's sharpest requirement: a rollback must not be
    /// indistinguishable from a success. The core is running the OLD config, so
    /// the reported revision is the old one and the rejection reason comes with
    /// it.
    #[test]
    fn a_rolled_back_apply_reports_the_old_revision_and_the_failure() {
        let data = map_apply_outcome(&ApplyOutcome::RolledBack {
            revision: revision(7),
            failed_apply: "core failed to start".to_owned(),
        });
        assert_eq!(data.outcome, ApplyOutcomeKind::RolledBack);
        assert_eq!(data.revision.generation, 7);
        assert_eq!(data.failed_apply.as_deref(), Some("core failed to start"));
    }

    /// `DurabilityUncertain` is a wrapper, and the apply path can wrap twice —
    /// a commit warning around a restore warning. Both must survive, and the
    /// wrapper must never be reported as the outcome.
    #[test]
    fn durability_warnings_unwrap_to_the_real_outcome() {
        let single = ApplyOutcome::DurabilityUncertain {
            outcome: Box::new(ApplyOutcome::Patched {
                revision: revision(8),
            }),
            warning: "directory sync failed".to_owned(),
        };
        let data = map_apply_outcome(&single);
        assert_eq!(data.outcome, ApplyOutcomeKind::Patched);
        assert_eq!(data.warning.as_deref(), Some("directory sync failed"));

        let nested = ApplyOutcome::DurabilityUncertain {
            outcome: Box::new(ApplyOutcome::DurabilityUncertain {
                outcome: Box::new(ApplyOutcome::RolledBack {
                    revision: revision(7),
                    failed_apply: "boom".to_owned(),
                }),
                warning: "restore sync failed".to_owned(),
            }),
            warning: "commit sync failed".to_owned(),
        };
        let data = map_apply_outcome(&nested);
        assert_eq!(data.outcome, ApplyOutcomeKind::RolledBack);
        assert_eq!(
            data.warning.as_deref(),
            Some("commit sync failed; restore sync failed")
        );
        assert_eq!(data.failed_apply.as_deref(), Some("boom"));
    }

    #[test]
    fn manager_errors_map_onto_the_wire_error_kinds() {
        let cases: [(ManagerError, Option<&str>); 8] = [
            (ManagerError::NotStarted, Some("not_started")),
            (ManagerError::AlreadyRunning, Some("already_running")),
            (
                ManagerError::RevisionConflict {
                    expected: RevisionId {
                        epoch: 3,
                        generation: 7,
                        effective_hash: "fedcba9876543210".to_owned(),
                    },
                    actual: None,
                },
                Some("revision_conflict"),
            ),
            (
                ManagerError::ManagerQuarantined {
                    epoch: 3,
                    reason: "death unconfirmed".to_owned(),
                },
                Some("quarantined"),
            ),
            (
                ManagerError::ConfigCheckFailed("unknown field".to_owned()),
                Some("config_check_failed"),
            ),
            (ManagerError::ControllerMissing, Some("controller_missing")),
            // The durability wrapper reports the wrapped failure's kind.
            (
                ManagerError::DurabilityUncertain {
                    source: Box::new(ManagerError::ApplyFailed("boom".to_owned())),
                    warning: "sync failed".to_owned(),
                },
                Some("apply_failed"),
            ),
            // Unmapped until report P3: no kind rather than a guessed one.
            (
                ManagerError::StartupFailed {
                    stderr_tail: String::new(),
                },
                None,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(map_error_kind(&error), expected, "{error}");
        }
    }

    /// `apply` on a stopped core answers with the same string `restart` does,
    /// so a GUI branching on it keeps working, and gains the kind beside it.
    #[test]
    fn the_not_started_failure_keeps_the_legacy_wire_string() {
        let error = OpError::from(ManagerError::NotStarted);
        assert_eq!(error.message, MSG_CORE_NOT_STARTED);
        assert_eq!(error.kind, Some("not_started"));
    }

    #[test]
    fn expected_revisions_cross_the_wire_unchanged() {
        assert_eq!(
            map_revision_id(&RevisionIdInfo {
                epoch: 3,
                generation: 7,
                effective_hash: "fedcba9876543210".to_owned(),
            }),
            RevisionId {
                epoch: 3,
                generation: 7,
                effective_hash: "fedcba9876543210".to_owned(),
            }
        );
    }

    fn status_of(state: ManagerCoreState) -> CoreStatus {
        CoreStatus {
            state,
            changed_at: 42,
            health: None,
            spec: None,
            controller: None,
            revision: None,
        }
    }

    /// Mirrors the restructured bridge loop: what a v1 connection receives
    /// (after the unchanged suppression rules) and what a v2 connection
    /// receives (one snapshot per manager transition, none suppressed).
    fn simulate_both(statuses: &[CoreStatus]) -> (Vec<String>, Vec<Option<CoreStateDetail>>) {
        let mut last = CoreState::Stopped(None);
        let mut v1 = Vec::new();
        let mut v2 = Vec::new();
        for raw in statuses {
            let next = map_core_state(&raw.state);
            v2.push(project_core_infos(raw, None).detail);
            if matches!(
                raw.state,
                ManagerCoreState::Stopping { .. } | ManagerCoreState::Switching { .. }
            ) && matches!(last, CoreState::Stopped(_))
            {
                continue;
            }
            if same_ipc_state(&last, &next) {
                continue;
            }
            v1.push(format!("{next:?}"));
            last = next;
        }
        (v1, v2)
    }

    /// The point of S9: the v1 stream keeps suppressing exactly what it always
    /// suppressed, while the v2 stream carries every transition — including the
    /// crash-loop and start-up ones the two-valued v1 state cannot express
    /// (report §1.2).
    #[test]
    fn the_v2_stream_carries_every_transition_the_v1_stream_suppresses() {
        let states = [
            ManagerCoreState::Starting { epoch: 1 },
            ManagerCoreState::Running { epoch: 1, pid: 42 },
            ManagerCoreState::Restarting {
                epoch: 1,
                attempt: 2,
            },
            ManagerCoreState::Running { epoch: 1, pid: 43 },
            ManagerCoreState::Stopping { epoch: 1 },
            ManagerCoreState::Stopped {
                reason: Some(StopReason::User),
            },
        ];
        let statuses: Vec<CoreStatus> = states.iter().cloned().map(status_of).collect();
        let (v1, v2) = simulate_both(&statuses);

        // The lossy v1 stream, defect included: the restart in the middle is
        // reported as a stop, which is why S9 exists.
        assert_eq!(
            v1,
            [
                "Running",
                "Stopped(None)",
                "Running",
                r#"Stopped(Some("stopped by user"))"#,
            ]
        );
        // …and the mirror above agrees with the untouched v1 helper.
        assert_eq!(v1, simulate(&states));
        // One faithful frame per manager transition, none dropped.
        assert_eq!(
            v2,
            [
                Some(CoreStateDetail::Starting { epoch: 1 }),
                Some(CoreStateDetail::Running { epoch: 1, pid: 42 }),
                Some(CoreStateDetail::Restarting {
                    epoch: 1,
                    attempt: 2,
                }),
                Some(CoreStateDetail::Running { epoch: 1, pid: 43 }),
                Some(CoreStateDetail::Stopping { epoch: 1 }),
                Some(CoreStateDetail::Stopped {
                    reason: Some("stopped by user".to_owned()),
                }),
            ]
        );
    }

    /// The v2 payload is the S7 projection unchanged: `state` stays the lossy
    /// field the v1 wire has always carried, `detail` is the faithful one, and
    /// the type echo comes from the adapter rather than the manager.
    #[test]
    fn the_v2_payload_keeps_the_lossy_state_and_adds_the_faithful_detail() {
        let infos = project_core_infos(
            &status_of(ManagerCoreState::Restarting {
                epoch: 3,
                attempt: 2,
            }),
            Some(CoreType::Clash(ClashCoreType::MihomoAlpha)),
        );
        assert!(matches!(infos.state, CoreState::Stopped(None)));
        assert_eq!(
            infos.detail,
            Some(CoreStateDetail::Restarting {
                epoch: 3,
                attempt: 2
            })
        );
        assert_eq!(
            infos.r#type,
            Some(CoreType::Clash(ClashCoreType::MihomoAlpha))
        );
        assert_eq!(infos.state_changed_at, 42);
        assert!(infos.controller.is_none());
        assert!(infos.health.is_none());
        assert!(infos.revision.is_none());
        assert!(infos.config_path.is_none());
    }
}

#[cfg(test)]
mod wire_strings {
    /// One of these three ("core is already running") cannot be produced by a
    /// route-level unit test — it requires an actually-running core — so the
    /// producer constants themselves are pinned here, and the two siblings'
    /// route tests prove the bail→envelope pipeline delivers them verbatim.
    #[test]
    fn the_legacy_core_error_strings_are_protocol() {
        assert_eq!(super::MSG_CORE_ALREADY_RUNNING, "core is already running");
        assert_eq!(super::MSG_CORE_ALREADY_STOPPED, "core is already stopped");
        assert_eq!(
            super::MSG_CORE_NOT_STARTED,
            "core have not been started yet"
        );
    }
}
