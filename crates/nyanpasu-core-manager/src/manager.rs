//! Cross-epoch orchestration: start/stop/switch and status publication.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::sync::watch;

use crate::{
    config,
    error::Error,
    instance::Instance,
    kind::CoreKind,
    spec::{ControllerMode, InstanceSpec, ManagerOptions, ResolvedController},
    state::{CoreState, CoreStatus, InstanceState, SpecSummary, StopReason, now_ms},
};

/// Why a switch was executed as a hard stop→start instead of gracefully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    NotRunning,
    PassthroughMode,
    UnsupportedKind,
    DnsListen,
    /// Graceful overlap succeeded but the listener-restore PATCH kept failing;
    /// converged via a hard restart on the full config.
    PatchFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    Graceful,
    Hard { reason: DegradeReason },
}

/// Spec §6.3 degradation matrix. `None` means graceful-eligible.
fn decide(managed: bool, kind: CoreKind, has_dns_listen: bool) -> Option<DegradeReason> {
    if !managed {
        return Some(DegradeReason::PassthroughMode);
    }
    if !matches!(kind, CoreKind::Mihomo) {
        return Some(DegradeReason::UnsupportedKind);
    }
    if has_dns_listen {
        return Some(DegradeReason::DnsListen);
    }
    None
}

pub struct CoreManager {
    inner: Arc<Inner>,
}

struct Inner {
    options: ManagerOptions,
    ctrl: tokio::sync::Mutex<Ctrl>,
    status_tx: watch::Sender<CoreStatus>,
    epoch: AtomicU64,
}

#[derive(Default)]
struct Ctrl {
    current: Option<Active>,
    last_spec: Option<InstanceSpec>,
}

struct Active {
    instance: Instance,
    forwarder: tokio::task::JoinHandle<()>,
    derived_path: Option<camino::Utf8PathBuf>,
}

impl Inner {
    fn publish_state(&self, state: CoreState) {
        self.status_tx.send_modify(|status| {
            status.state = state;
            status.changed_at = now_ms();
        });
    }

    fn publish_context(&self, spec: Option<SpecSummary>, controller: Option<clash_api::Host>) {
        self.status_tx.send_modify(|status| {
            status.spec = spec;
            status.controller = controller;
        });
    }
}

impl CoreManager {
    pub fn new(options: ManagerOptions) -> Self {
        if let ControllerMode::Managed { derived_dir, .. } = &options.controller_mode {
            sweep_derived_dir(derived_dir);
        }
        let (status_tx, _) = watch::channel(CoreStatus::initial());
        Self {
            inner: Arc::new(Inner {
                options,
                ctrl: tokio::sync::Mutex::default(),
                status_tx,
                epoch: AtomicU64::new(0),
            }),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<CoreStatus> {
        self.inner.status_tx.subscribe()
    }

    pub fn status(&self) -> CoreStatus {
        self.inner.status_tx.borrow().clone()
    }

    fn next_epoch(&self) -> u64 {
        self.inner.epoch.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Mode-dependent launch preparation: the effective spec (Managed mode may
    /// swap the config path for a derived one), the probe controller, and the
    /// publicly advertised controller endpoint (Managed only).
    async fn prepare(
        &self,
        spec: &InstanceSpec,
        epoch: u64,
    ) -> Result<
        (
            InstanceSpec,
            ResolvedController,
            Option<clash_api::Host>,
            Option<camino::Utf8PathBuf>,
        ),
        Error,
    > {
        match &self.inner.options.controller_mode {
            ControllerMode::Passthrough => {
                let info = config::inspect(&spec.config_path).await?;
                let controller = config::resolve_controller(&info)?;
                Ok((spec.clone(), controller, None, None))
            }
            ControllerMode::Managed {
                derived_dir,
                controller_template,
            } => {
                let derived = config::derive(
                    &spec.config_path,
                    derived_dir,
                    controller_template.as_deref(),
                    epoch,
                    config::DeriveMode::ControllerOnly,
                )
                .await?;
                let mut effective = spec.clone();
                effective.config_path = derived.path.clone();
                let advertised = Some(derived.controller.host.clone());
                Ok((
                    effective,
                    derived.controller,
                    advertised,
                    Some(derived.path),
                ))
            }
        }
    }

    pub async fn start(&self, spec: InstanceSpec) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        let running = ctrl
            .current
            .as_ref()
            .is_some_and(|active| !active.instance.state().borrow().is_terminal());
        if running {
            return Err(Error::AlreadyRunning);
        }
        if let Some(stale) = ctrl.current.take() {
            stale.forwarder.abort();
        }
        self.start_locked(&mut ctrl, spec).await
    }

    async fn start_locked(&self, ctrl: &mut Ctrl, spec: InstanceSpec) -> Result<(), Error> {
        self.start_locked_with_epoch(ctrl, spec, None).await
    }

    async fn start_locked_with_epoch(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
        epoch_override: Option<u64>,
    ) -> Result<(), Error> {
        let epoch = epoch_override.unwrap_or_else(|| self.next_epoch());
        let (effective_spec, controller, advertised, derived_path) =
            self.prepare(&spec, epoch).await?;
        self.inner.publish_context(
            Some(SpecSummary {
                kind: spec.core.kind,
                config_path: spec.config_path.clone(),
            }),
            advertised,
        );
        self.inner.publish_state(CoreState::Starting { epoch });

        let instance = match Instance::spawn(
            effective_spec,
            epoch,
            controller,
            self.inner.options.cancel_token.clone(),
        )
        .await
        {
            Ok(instance) => instance,
            Err(error) => {
                self.inner.publish_state(CoreState::Stopped {
                    reason: Some(StopReason::Error(error.to_string())),
                });
                return Err(error);
            }
        };

        match instance.wait_ready().await {
            Ok(()) => {
                let pid = instance.pid().unwrap_or_default();
                self.inner.publish_state(CoreState::Running { epoch, pid });
                let forwarder = spawn_forwarder(self.inner.clone(), instance.state(), epoch);
                ctrl.current = Some(Active {
                    instance,
                    forwarder,
                    derived_path,
                });
                ctrl.last_spec = Some(spec);
                Ok(())
            }
            Err(error) => {
                self.inner.publish_state(CoreState::Stopped {
                    reason: Some(StopReason::Error(error.to_string())),
                });
                cleanup_derived(derived_path).await;
                Err(error)
            }
        }
    }

    pub async fn restart(&self) -> Result<SwitchOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        let spec = ctrl.last_spec.clone().ok_or(Error::NotStarted)?;
        self.switch_locked(&mut ctrl, spec).await
    }

    pub async fn switch(&self, spec: InstanceSpec) -> Result<SwitchOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        self.switch_locked(&mut ctrl, spec).await
    }

    async fn switch_locked(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
    ) -> Result<SwitchOutcome, Error> {
        let running = ctrl
            .current
            .as_ref()
            .is_some_and(|active| !active.instance.state().borrow().is_terminal());
        if !running {
            if let Some(stale) = ctrl.current.take() {
                stale.forwarder.abort();
            }
            self.start_locked(ctrl, spec).await?;
            return Ok(SwitchOutcome::Hard {
                reason: DegradeReason::NotRunning,
            });
        }
        let managed = matches!(
            self.inner.options.controller_mode,
            ControllerMode::Managed { .. }
        );
        let info = config::inspect(&spec.config_path).await?;
        match decide(managed, spec.core.kind, info.has_dns_listen) {
            Some(reason) => {
                self.hard_switch(ctrl, spec).await?;
                Ok(SwitchOutcome::Hard { reason })
            }
            None => self.graceful_switch(ctrl, spec).await,
        }
    }

    async fn hard_switch(&self, ctrl: &mut Ctrl, spec: InstanceSpec) -> Result<(), Error> {
        let active = ctrl.current.take().expect("running checked by caller");
        active.forwarder.abort();
        let from = active.instance.epoch();
        // Safe peek: `epoch` only advances under the ctrl lock we hold.
        let to = self.inner.epoch.load(Ordering::Relaxed) + 1;
        self.inner.publish_state(CoreState::Switching {
            from: Some(from),
            to,
        });
        match active.instance.stop().await {
            Ok(()) => {}
            Err(error) => {
                self.inner.publish_state(CoreState::Stopped {
                    reason: Some(StopReason::Error(format!(
                        "switch aborted: failed to stop the old core: {error}"
                    ))),
                });
                return Err(error);
            }
        }
        cleanup_derived(active.derived_path).await;
        self.start_locked(ctrl, spec).await
    }

    async fn graceful_switch(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
    ) -> Result<SwitchOutcome, Error> {
        let ControllerMode::Managed {
            derived_dir,
            controller_template,
        } = self.inner.options.controller_mode.clone()
        else {
            unreachable!("decide() only selects graceful in Managed mode");
        };
        let old_epoch = ctrl.current.as_ref().map(|a| a.instance.epoch());
        let old_pid = ctrl
            .current
            .as_ref()
            .and_then(|a| a.instance.pid())
            .unwrap_or_default();
        let epoch = self.next_epoch();
        self.inner.publish_state(CoreState::Switching {
            from: old_epoch,
            to: epoch,
        });

        // 1. Derive B' (listeners zeroed, epoch endpoint injected) and start it
        //    while the old core keeps serving.
        let derived = config::derive(
            &spec.config_path,
            &derived_dir,
            controller_template.as_deref(),
            epoch,
            config::DeriveMode::ZeroListeners,
        )
        .await?;
        let mut effective = spec.clone();
        effective.config_path = derived.path.clone();
        let started = async {
            let instance = Instance::spawn(
                effective,
                epoch,
                derived.controller.clone(),
                self.inner.options.cancel_token.clone(),
            )
            .await?;
            instance.wait_ready().await?;
            Ok::<Instance, Error>(instance)
        }
        .await;
        let instance = match started {
            Ok(instance) => instance,
            Err(error) => {
                // Safe rollback: the old core was never touched.
                cleanup_derived(Some(derived.path)).await;
                if let Some(from) = old_epoch {
                    self.inner.publish_state(CoreState::Running {
                        epoch: from,
                        pid: old_pid,
                    });
                }
                return Err(error);
            }
        };

        // 2. Point of no return: stop the old core, releasing its listeners.
        let old = ctrl.current.take().expect("running checked by caller");
        old.forwarder.abort();
        let old_derived = old.derived_path.clone();
        match old.instance.stop().await {
            Ok(()) => {}
            Err(error) => {
                instance.stop().await.ok();
                cleanup_derived(Some(derived.path)).await;
                self.inner.publish_state(CoreState::Stopped {
                    reason: Some(StopReason::Error(format!(
                        "switch aborted: failed to stop the old core: {error}"
                    ))),
                });
                return Err(error);
            }
        }
        cleanup_derived(old_derived).await;

        // 3. Restore the original listeners on the new core (3 tries × 500ms).
        let patched = if derived.restore.is_empty() {
            true
        } else {
            let client = match crate::health::build_client(instance.controller()) {
                Ok(client) => client,
                Err(_error) => {
                    instance.stop().await.ok();
                    cleanup_derived(Some(derived.path)).await;
                    self.start_locked_with_epoch(ctrl, spec, Some(epoch))
                        .await?;
                    return Ok(SwitchOutcome::Hard {
                        reason: DegradeReason::PatchFailed,
                    });
                }
            };
            let patch = derived.restore.to_patch();
            let mut ok = false;
            for attempt in 1..=3u32 {
                match client.patch_config(&patch).await {
                    Ok(()) => {
                        ok = true;
                        break;
                    }
                    Err(error) => {
                        tracing::warn!("listener-restore patch attempt {attempt} failed: {error}");
                        if attempt < 3 {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                }
            }
            ok
        };
        if !patched {
            // 4. Fallback: the old core is dead and its ports are free — hard
            //    restart the new instance on the full config, same epoch.
            instance.stop().await.ok();
            cleanup_derived(Some(derived.path)).await;
            self.start_locked_with_epoch(ctrl, spec, Some(epoch))
                .await?;
            return Ok(SwitchOutcome::Hard {
                reason: DegradeReason::PatchFailed,
            });
        }

        // 5. Install the new core.
        let pid = instance.pid().unwrap_or_default();
        self.inner.publish_state(CoreState::Running { epoch, pid });
        let forwarder = spawn_forwarder(self.inner.clone(), instance.state(), epoch);
        ctrl.current = Some(Active {
            instance,
            forwarder,
            derived_path: Some(derived.path),
        });
        ctrl.last_spec = Some(spec);
        Ok(SwitchOutcome::Graceful)
    }

    pub async fn stop(&self) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        let Some(active) = ctrl.current.take() else {
            return Err(Error::NotStarted);
        };
        active.forwarder.abort();
        if active.instance.state().borrow().is_terminal() {
            return Err(Error::NotStarted);
        }
        let epoch = active.instance.epoch();
        self.inner.publish_state(CoreState::Stopping { epoch });
        match active.instance.stop().await {
            Ok(()) => {}
            Err(error) => {
                self.inner.publish_state(CoreState::Stopped {
                    reason: Some(StopReason::Error(format!("stop failed: {error}"))),
                });
                return Err(error);
            }
        }
        cleanup_derived(active.derived_path).await;
        self.inner.publish_state(CoreState::Stopped {
            reason: Some(StopReason::User),
        });
        Ok(())
    }

    /// One-shot `-t` validation of a spec's config (spec §6.1 convenience).
    pub async fn check_config(&self, spec: &InstanceSpec) -> Result<(), Error> {
        crate::kind::check_config(spec).await
    }

    /// Service-shutdown teardown: stop whatever is running, tolerate nothing running.
    pub async fn shutdown(&self) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        if let Some(active) = ctrl.current.take() {
            active.forwarder.abort();
            if !active.instance.state().borrow().is_terminal() {
                let epoch = active.instance.epoch();
                self.inner.publish_state(CoreState::Stopping { epoch });
                match active.instance.stop().await {
                    Ok(()) => {}
                    Err(error) => {
                        self.inner.publish_state(CoreState::Stopped {
                            reason: Some(StopReason::Error(format!("stop failed: {error}"))),
                        });
                        cleanup_derived(active.derived_path).await;
                        return Err(error);
                    }
                }
            }
            cleanup_derived(active.derived_path).await;
            self.inner.publish_state(CoreState::Stopped {
                reason: Some(StopReason::User),
            });
        }
        Ok(())
    }
}

/// Removes runtime artifacts left behind by a previous manager process.
fn sweep_derived_dir(derived_dir: &camino::Utf8Path) {
    let Ok(entries) = std::fs::read_dir(derived_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stale = (name.starts_with("epoch-") && name.ends_with(".yaml"))
            || (name.starts_with("core-") && name.ends_with(".sock"));
        if stale && let Err(error) = std::fs::remove_file(entry.path()) {
            tracing::warn!("failed to sweep stale derived artifact {name}: {error}");
        }
    }
}

async fn cleanup_derived(path: Option<camino::Utf8PathBuf>) {
    if let Some(path) = path {
        let _ = tokio::fs::remove_file(&path).await;
    }
}

/// Steady-state bridge: instance transitions → manager status. Installed only
/// once a start/switch confirmed `Running`; aborted before any control action.
fn spawn_forwarder(
    inner: Arc<Inner>,
    mut state_rx: watch::Receiver<InstanceState>,
    epoch: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if state_rx.changed().await.is_err() {
                break;
            }
            let state = state_rx.borrow_and_update().clone();
            let core_state = match state {
                InstanceState::Starting => CoreState::Starting { epoch },
                InstanceState::Running { pid } => CoreState::Running { epoch, pid },
                InstanceState::Restarting { attempt } => CoreState::Restarting { epoch, attempt },
                InstanceState::Stopping => CoreState::Stopping { epoch },
                InstanceState::Stopped(reason) => {
                    inner.publish_state(CoreState::Stopped {
                        reason: Some(reason),
                    });
                    break;
                }
            };
            inner.publish_state(core_state);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::CoreKind;

    #[test]
    fn switch_matrix_matches_the_spec() {
        assert_eq!(
            decide(false, CoreKind::Mihomo, false),
            Some(DegradeReason::PassthroughMode)
        );
        assert_eq!(
            decide(true, CoreKind::ClashRs, false),
            Some(DegradeReason::UnsupportedKind)
        );
        assert_eq!(
            decide(true, CoreKind::ClashPremium, false),
            Some(DegradeReason::UnsupportedKind)
        );
        assert_eq!(
            decide(true, CoreKind::Mihomo, true),
            Some(DegradeReason::DnsListen)
        );
        assert_eq!(decide(true, CoreKind::Mihomo, false), None);
    }
}
