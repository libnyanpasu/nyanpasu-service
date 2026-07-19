//! Cross-epoch orchestration: manager-owned artifacts, start/stop/switch, and
//! atomic status publication.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use nyanpasu_utils::process::reap_epoch_pid_file;
use tokio::sync::watch;

use crate::{
    config::{self, ConfigSnapshot, DeriveMode},
    error::Error,
    instance::Instance,
    kind::CoreKind,
    runtime_store::RuntimeConfigStore,
    spec::{ControllerMode, InstanceSpec, ManagerOptions, ResolvedController},
    state::{
        ConfigRevision, CoreState, CoreStatus, InstanceState, SpecSummary, StopReason, now_ms,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    NotRunning,
    PassthroughMode,
    UnsupportedKind,
    DnsListen,
    PatchFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    Graceful,
    Hard { reason: DegradeReason },
}

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
    store: RuntimeConfigStore,
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
    source_spec: InstanceSpec,
    revision: ConfigRevision,
}

struct PreparedLaunch {
    source_spec: InstanceSpec,
    effective_spec: InstanceSpec,
    controller: ResolvedController,
    revision: ConfigRevision,
    restore: config::RestorePlan,
}

impl Inner {
    fn publish(
        &self,
        state: CoreState,
        spec: Option<SpecSummary>,
        controller: Option<clash_api::Host>,
        revision: Option<ConfigRevision>,
    ) {
        self.status_tx.send_modify(|status| {
            status.state = state;
            status.spec = spec;
            status.controller = controller;
            status.revision = revision;
            status.changed_at = now_ms();
        });
    }

    fn publish_active(&self, active: &Active, state: CoreState) {
        self.publish(
            state,
            Some(spec_summary(&active.source_spec)),
            Some(active.instance.controller().host.clone()),
            Some(active.revision.clone()),
        );
    }

    fn publish_epoch_state(&self, epoch: u64, state: CoreState) {
        self.status_tx.send_modify(|status| {
            if apply_epoch_state(status, epoch, state.clone()) {
                status.changed_at = now_ms();
            }
        });
    }
}

fn apply_epoch_state(status: &mut CoreStatus, epoch: u64, state: CoreState) -> bool {
    if status.revision.as_ref().map(|revision| revision.epoch) != Some(epoch) {
        return false;
    }
    status.state = state;
    true
}

fn spec_summary(spec: &InstanceSpec) -> SpecSummary {
    SpecSummary {
        kind: spec.core.kind,
        config_path: spec.config_path.clone(),
    }
}

impl CoreManager {
    pub async fn new(options: ManagerOptions) -> Result<Self, Error> {
        let runtime_dir = match (&options.runtime_dir, &options.controller_mode) {
            (Some(runtime_dir), _) => runtime_dir.clone(),
            (None, ControllerMode::Managed { derived_dir, .. }) => derived_dir.clone(),
            (None, ControllerMode::Passthrough) => {
                return Err(Error::InvalidManagerOptions(
                    "Passthrough mode requires runtime_dir".into(),
                ));
            }
        };
        let store = RuntimeConfigStore::new(runtime_dir).await?;
        let max_epoch = sweep_orphans(&store).await?;

        if let ControllerMode::Managed {
            controller_template,
            ..
        } = &options.controller_mode
        {
            config::validate_controller_template(controller_template.as_deref())?;
        }
        for (name, timeout) in [
            ("control_timeout", options.control_timeout),
            ("reconcile_timeout", options.reconcile_timeout),
            ("stop_timeout", options.stop_timeout),
        ] {
            if timeout.is_zero() {
                return Err(Error::InvalidManagerOptions(format!(
                    "{name} must be greater than zero"
                )));
            }
        }
        let (status_tx, _) = watch::channel(CoreStatus::initial());
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                store,
                ctrl: tokio::sync::Mutex::default(),
                status_tx,
                epoch: AtomicU64::new(max_epoch),
            }),
        })
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

    async fn prepare_launch(
        &self,
        spec: &InstanceSpec,
        epoch: u64,
        snapshot: &ConfigSnapshot,
        derive_mode: DeriveMode,
    ) -> Result<PreparedLaunch, Error> {
        debug_assert_eq!(snapshot.source_path(), spec.config_path);
        if tokio::fs::metadata(&spec.core.binary_path).await.is_err() {
            return Err(Error::BinaryNotFound(spec.core.binary_path.clone()));
        }
        spec.core
            .kind
            .run_args(&spec.working_dir, &spec.config_path)?;
        let prepared = snapshot.prepare(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
            derive_mode,
        )?;
        let staged = self.inner.store.stage(epoch, &prepared.bytes).await?;

        let mut check_spec = spec.clone();
        check_spec.config_path = staged.path().to_owned();
        crate::kind::check_config(&check_spec).await?;

        let runtime_path = self.inner.store.commit_new(staged, epoch).await?;
        let mut effective_spec = spec.clone();
        effective_spec.config_path = runtime_path.clone();
        effective_spec.pid_file = Some(self.inner.store.pid_path(epoch));
        Ok(PreparedLaunch {
            source_spec: spec.clone(),
            effective_spec,
            controller: prepared.controller,
            revision: ConfigRevision {
                epoch,
                generation: 1,
                source_hash: prepared.source_hash,
                effective_hash: prepared.effective_hash,
                runtime_path,
            },
            restore: prepared.restore,
        })
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
            abort_and_await(stale.forwarder).await;
            let epoch = stale.instance.epoch();
            stale
                .instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await?;
            self.inner.store.cleanup_epoch(epoch).await?;
        }
        self.start_locked(&mut ctrl, spec).await
    }

    async fn start_locked(&self, ctrl: &mut Ctrl, spec: InstanceSpec) -> Result<(), Error> {
        let epoch = self.next_epoch();
        let snapshot = match ConfigSnapshot::load(&spec.config_path).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.publish_terminal_error(&error);
                return Err(error);
            }
        };
        let prepared = match self
            .prepare_launch(&spec, epoch, &snapshot, DeriveMode::ControllerOnly)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.publish_terminal_error(&error);
                return Err(error);
            }
        };
        self.start_prepared(ctrl, prepared).await
    }

    async fn start_prepared(&self, ctrl: &mut Ctrl, prepared: PreparedLaunch) -> Result<(), Error> {
        let epoch = prepared.revision.epoch;
        self.inner.publish(
            CoreState::Starting { epoch },
            Some(spec_summary(&prepared.source_spec)),
            Some(prepared.controller.host.clone()),
            Some(prepared.revision.clone()),
        );
        let instance = match Instance::spawn(
            prepared.effective_spec,
            epoch,
            prepared.controller,
            self.inner.options.cancel_token.clone(),
        )
        .await
        {
            Ok(instance) => instance,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.publish_terminal_error(&error);
                return Err(error);
            }
        };

        if let Err(error) = instance.wait_ready().await {
            let stopped = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await;
            if stopped.is_ok() {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
            }
            self.publish_terminal_error(&error);
            return Err(error);
        }

        let pid = instance.pid().unwrap_or_default();
        self.inner.publish(
            CoreState::Running { epoch, pid },
            Some(spec_summary(&prepared.source_spec)),
            Some(instance.controller().host.clone()),
            Some(prepared.revision.clone()),
        );
        let forwarder = spawn_forwarder(self.inner.clone(), instance.state(), epoch);
        ctrl.last_spec = Some(prepared.source_spec.clone());
        ctrl.current = Some(Active {
            instance,
            forwarder,
            source_spec: prepared.source_spec,
            revision: prepared.revision,
        });
        Ok(())
    }

    fn publish_terminal_error(&self, error: &Error) {
        self.inner.publish(
            CoreState::Stopped {
                reason: Some(StopReason::Error(error.to_string())),
            },
            None,
            None,
            None,
        );
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
                abort_and_await(stale.forwarder).await;
                let epoch = stale.instance.epoch();
                stale
                    .instance
                    .stop_and_confirm_dead(self.inner.options.stop_timeout)
                    .await?;
                self.inner.store.cleanup_epoch(epoch).await?;
            }
            self.start_locked(ctrl, spec).await?;
            return Ok(SwitchOutcome::Hard {
                reason: DegradeReason::NotRunning,
            });
        }

        let snapshot = ConfigSnapshot::load(&spec.config_path).await?;
        let managed = matches!(
            self.inner.options.controller_mode,
            ControllerMode::Managed { .. }
        );
        match decide(managed, spec.core.kind, snapshot.info().has_dns_listen) {
            Some(reason) => {
                self.hard_switch(ctrl, spec, snapshot).await?;
                Ok(SwitchOutcome::Hard { reason })
            }
            None => self.graceful_switch(ctrl, spec, snapshot).await,
        }
    }

    async fn hard_switch(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
        snapshot: ConfigSnapshot,
    ) -> Result<(), Error> {
        let epoch = self.next_epoch();
        let prepared = match self
            .prepare_launch(&spec, epoch, &snapshot, DeriveMode::ControllerOnly)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        let old_epoch = ctrl.current.as_ref().map(|active| active.instance.epoch());
        self.inner.publish(
            CoreState::Switching {
                from: old_epoch,
                to: epoch,
            },
            Some(spec_summary(&prepared.source_spec)),
            Some(prepared.controller.host.clone()),
            Some(prepared.revision.clone()),
        );

        let old = ctrl.current.take().expect("running checked by caller");
        abort_and_await(old.forwarder).await;
        let old_epoch = old.instance.epoch();
        if let Err(error) = old
            .instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            let _ = self.inner.store.cleanup_epoch(epoch).await;
            self.publish_terminal_error(&error);
            return Err(error);
        }
        if let Err(error) = self.inner.store.cleanup_epoch(old_epoch).await {
            self.publish_terminal_error(&error);
            return Err(error);
        }
        self.start_prepared(ctrl, prepared).await
    }

    fn republish_retained(&self, ctrl: &Ctrl) {
        let Some(active) = ctrl.current.as_ref() else {
            return;
        };
        let state = instance_core_state(active.instance.epoch(), &active.instance.state().borrow());
        self.inner.publish_active(active, state);
    }

    async fn graceful_switch(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
        snapshot: ConfigSnapshot,
    ) -> Result<SwitchOutcome, Error> {
        let old_epoch = ctrl.current.as_ref().map(|active| active.instance.epoch());
        let epoch = self.next_epoch();
        let prepared = match self
            .prepare_launch(&spec, epoch, &snapshot, DeriveMode::ZeroListeners)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        self.inner.publish(
            CoreState::Switching {
                from: old_epoch,
                to: epoch,
            },
            Some(spec_summary(&prepared.source_spec)),
            Some(prepared.controller.host.clone()),
            Some(prepared.revision.clone()),
        );

        let instance = match Instance::spawn(
            prepared.effective_spec.clone(),
            epoch,
            prepared.controller.clone(),
            self.inner.options.cancel_token.clone(),
        )
        .await
        {
            Ok(instance) => instance,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        if let Err(error) = instance.wait_ready().await {
            if instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
                .is_ok()
            {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
            }
            self.republish_retained(ctrl);
            return Err(error);
        }

        let old = ctrl.current.take().expect("running checked by caller");
        abort_and_await(old.forwarder).await;
        let old_epoch = old.instance.epoch();
        if let Err(error) = old
            .instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            if instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
                .is_ok()
            {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
            }
            self.publish_terminal_error(&error);
            return Err(error);
        }
        if let Err(error) = self.inner.store.cleanup_epoch(old_epoch).await {
            if instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
                .is_ok()
            {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
            }
            self.publish_terminal_error(&error);
            return Err(error);
        }

        let patched = if prepared.restore.is_empty() {
            true
        } else {
            let client = crate::health::build_control_client(
                instance.controller(),
                self.inner.options.control_timeout,
            );
            match client {
                Ok(client) => {
                    let patch = prepared.restore.to_patch();
                    let mut patched = false;
                    for attempt in 1..=3_u32 {
                        match client.patch_config(&patch).await {
                            Ok(()) => {
                                patched = true;
                                break;
                            }
                            Err(error) => {
                                tracing::warn!(
                                    "listener-restore patch attempt {attempt} failed: {error}"
                                );
                                if attempt < 3 {
                                    tokio::time::sleep(Duration::from_millis(500)).await;
                                }
                            }
                        }
                    }
                    patched
                }
                Err(error) => {
                    tracing::warn!("failed to build listener-restore client: {error}");
                    false
                }
            }
        };
        if !patched {
            if let Err(error) = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                self.publish_terminal_error(&error);
                return Err(error);
            }
            if let Err(error) = self.inner.store.cleanup_epoch(epoch).await {
                self.publish_terminal_error(&error);
                return Err(error);
            }
            let full = match self
                .prepare_launch(&spec, epoch, &snapshot, DeriveMode::ControllerOnly)
                .await
            {
                Ok(full) => full,
                Err(error) => {
                    self.publish_terminal_error(&error);
                    return Err(error);
                }
            };
            self.start_prepared(ctrl, full).await?;
            return Ok(SwitchOutcome::Hard {
                reason: DegradeReason::PatchFailed,
            });
        }

        let pid = instance.pid().unwrap_or_default();
        self.inner.publish(
            CoreState::Running { epoch, pid },
            Some(spec_summary(&prepared.source_spec)),
            Some(instance.controller().host.clone()),
            Some(prepared.revision.clone()),
        );
        let forwarder = spawn_forwarder(self.inner.clone(), instance.state(), epoch);
        ctrl.last_spec = Some(prepared.source_spec.clone());
        ctrl.current = Some(Active {
            instance,
            forwarder,
            source_spec: prepared.source_spec,
            revision: prepared.revision,
        });
        Ok(SwitchOutcome::Graceful)
    }

    pub async fn stop(&self) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        let Some(active) = ctrl.current.take() else {
            return Err(Error::NotStarted);
        };
        let Active {
            instance,
            forwarder,
            source_spec,
            revision,
        } = active;
        abort_and_await(forwarder).await;
        if instance.state().borrow().is_terminal() {
            let epoch = instance.epoch();
            instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await?;
            if let Err(error) = self.inner.store.cleanup_epoch(epoch).await {
                self.publish_terminal_error(&error);
                return Err(error);
            }
            return Err(Error::NotStarted);
        }
        let epoch = instance.epoch();
        self.inner.publish(
            CoreState::Stopping { epoch },
            Some(spec_summary(&source_spec)),
            Some(instance.controller().host.clone()),
            Some(revision),
        );
        if let Err(error) = instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            self.publish_terminal_error(&error);
            return Err(error);
        }
        if let Err(error) = self.inner.store.cleanup_epoch(epoch).await {
            self.publish_terminal_error(&error);
            return Err(error);
        }
        self.inner.publish(
            CoreState::Stopped {
                reason: Some(StopReason::User),
            },
            None,
            None,
            None,
        );
        Ok(())
    }

    pub async fn check_config(&self, spec: &InstanceSpec) -> Result<(), Error> {
        crate::kind::check_config(spec).await
    }

    pub async fn shutdown(&self) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        if let Some(active) = ctrl.current.take() {
            let Active {
                instance,
                forwarder,
                source_spec,
                revision,
            } = active;
            abort_and_await(forwarder).await;
            let epoch = instance.epoch();
            self.inner.publish(
                CoreState::Stopping { epoch },
                Some(spec_summary(&source_spec)),
                Some(instance.controller().host.clone()),
                Some(revision),
            );
            if let Err(error) = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                self.publish_terminal_error(&error);
                return Err(error);
            }
            if let Err(error) = self.inner.store.cleanup_epoch(epoch).await {
                self.publish_terminal_error(&error);
                return Err(error);
            }
            self.inner.publish(
                CoreState::Stopped {
                    reason: Some(StopReason::User),
                },
                None,
                None,
                None,
            );
        }
        Ok(())
    }
}

async fn sweep_orphans(store: &RuntimeConfigStore) -> Result<u64, Error> {
    let epochs = store.artifact_epochs().await?;
    let max_epoch = epochs.iter().copied().max().unwrap_or(0);
    for epoch in epochs {
        let pid_path = store.pid_path(epoch);
        if tokio::fs::try_exists(&pid_path).await? {
            reap_epoch_pid_file(pid_path.as_std_path(), store.dir().as_std_path()).await?;
        }
        store.cleanup_epoch(epoch).await?;
    }
    Ok(max_epoch)
}

async fn abort_and_await(mut forwarder: tokio::task::JoinHandle<()>) {
    forwarder.abort();
    let _ = (&mut forwarder).await;
}

fn instance_core_state(epoch: u64, state: &InstanceState) -> CoreState {
    match state {
        InstanceState::Starting => CoreState::Starting { epoch },
        InstanceState::Running { pid } => CoreState::Running { epoch, pid: *pid },
        InstanceState::Restarting { attempt } => CoreState::Restarting {
            epoch,
            attempt: *attempt,
        },
        InstanceState::Stopping => CoreState::Stopping { epoch },
        InstanceState::Stopped(reason) => CoreState::Stopped {
            reason: Some(reason.clone()),
        },
    }
}

fn spawn_forwarder(
    inner: Arc<Inner>,
    mut state_rx: watch::Receiver<InstanceState>,
    epoch: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while state_rx.changed().await.is_ok() {
            let state = state_rx.borrow_and_update().clone();
            let terminal = state.is_terminal();
            inner.publish_epoch_state(epoch, instance_core_state(epoch, &state));
            if terminal {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            decide(true, CoreKind::Mihomo, true),
            Some(DegradeReason::DnsListen)
        );
        assert_eq!(decide(true, CoreKind::Mihomo, false), None);
    }

    #[test]
    fn old_epoch_events_cannot_overwrite_new_epoch_status() {
        let mut status = CoreStatus::initial();
        status.revision = Some(ConfigRevision {
            epoch: 9,
            generation: 1,
            source_hash: "source".into(),
            effective_hash: "effective".into(),
            runtime_path: "config-9.yaml".into(),
        });
        status.state = CoreState::Running { epoch: 9, pid: 90 };
        for stale in [
            CoreState::Running { epoch: 8, pid: 80 },
            CoreState::Restarting {
                epoch: 8,
                attempt: 2,
            },
            CoreState::Stopped {
                reason: Some(StopReason::Finished),
            },
        ] {
            assert!(!apply_epoch_state(&mut status, 8, stale));
            assert!(matches!(
                status.state,
                CoreState::Running { epoch: 9, pid: 90 }
            ));
        }
    }
}
