//! Cross-epoch orchestration: manager-owned artifacts, start/stop/switch, and
//! atomic status publication.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use nyanpasu_utils::process::{OrphanReapOutcome, reap_epoch_pid_file};
use serde_yaml_ng::Mapping;
use tokio::sync::watch;

use crate::{
    config::{self, ConfigSnapshot},
    config_diff::{self, ConfigChange, OverlapBlock},
    error::Error,
    instance::Instance,
    kind::CoreKind,
    probe::{ProbeHandle, ProbePhase},
    runtime_store::{RuntimeConfigStore, RuntimeDirectoryLock, StagedRuntimeConfig},
    spec::{ControllerMode, InstanceSpec, ManagerOptions, ResolvedController},
    state::{
        ConfigRevision, CoreState, CoreStatus, HealthStatus, InstanceState, InstanceStatus,
        RevisionId, SpecSummary, StopReason, now_ms,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    NotRunning,
    PassthroughMode,
    UnsupportedKind,
    DnsListen,
    InboundConflict,
    PatchFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchOutcome {
    Graceful,
    Hard {
        reason: DegradeReason,
    },
    DurabilityUncertain {
        outcome: Box<SwitchOutcome>,
        warning: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Noop {
        revision: ConfigRevision,
    },
    Patched {
        revision: ConfigRevision,
    },
    Reloaded {
        revision: ConfigRevision,
    },
    Restarted {
        revision: ConfigRevision,
    },
    RolledBack {
        revision: ConfigRevision,
        failed_apply: String,
    },
    DurabilityUncertain {
        outcome: Box<ApplyOutcome>,
        warning: String,
    },
}

fn graceful_degrade_reason(
    managed: bool,
    kind: CoreKind,
    overlap_block: Option<OverlapBlock>,
) -> Option<DegradeReason> {
    if !managed {
        return Some(DegradeReason::PassthroughMode);
    }
    if !matches!(kind, CoreKind::Mihomo) {
        return Some(DegradeReason::UnsupportedKind);
    }
    if let Some(block) = overlap_block {
        return Some(match block {
            OverlapBlock::DnsListen => DegradeReason::DnsListen,
            OverlapBlock::InboundSurface => DegradeReason::InboundConflict,
        });
    }
    None
}

pub struct CoreManager {
    inner: Arc<Inner>,
}

pub struct CoreManagerBuilder {
    options: ManagerOptions,
    probes: ProbePlan,
}

#[derive(Clone, Default)]
struct ProbePlan {
    readiness: Option<ProbeHandle>,
    liveness: Option<ProbeHandle>,
    liveness_with_readiness: bool,
}

struct Inner {
    options: ManagerOptions,
    probes: ProbePlan,
    store: RuntimeConfigStore,
    ctrl: tokio::sync::Mutex<Ctrl>,
    status_tx: watch::Sender<CoreStatus>,
    epoch: AtomicU64,
    // Declared last so ordinary Inner destruction drops instances/tasks before
    // releasing directory ownership.
    _runtime_lock: RuntimeDirectoryLock,
}

#[derive(Default)]
struct Ctrl {
    current: Option<Active>,
    last_spec: Option<InstanceSpec>,
    quarantine: Vec<QuarantinedEpoch>,
}

#[derive(Debug, Clone)]
struct QuarantinedEpoch {
    epoch: u64,
    reason: String,
    death_proven: bool,
}

struct Active {
    instance: Instance,
    forwarder: tokio::task::JoinHandle<()>,
    source_spec: InstanceSpec,
    revision: ConfigRevision,
    source_document: Mapping,
    effective_document: Mapping,
}

struct PreparedLaunch {
    source_spec: InstanceSpec,
    effective_spec: InstanceSpec,
    controller: ResolvedController,
    revision: ConfigRevision,
    source_document: Mapping,
    effective_document: Mapping,
}

struct PreparedGraceful {
    launch: PreparedLaunch,
    full_staged: StagedRuntimeConfig,
    restoration: Option<(Box<clash_api::ConfigPatch>, config_diff::RuntimeProjection)>,
}

struct PreparedApply {
    source_spec: InstanceSpec,
    effective_spec: InstanceSpec,
    controller: ResolvedController,
    revision: ConfigRevision,
    source_document: Mapping,
    effective_document: Mapping,
    staged: StagedRuntimeConfig,
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
            let lifecycle_changed = status.state != state;
            let health = default_health_for_state(status.health.as_ref(), &state);
            status.state = state;
            status.health = health;
            status.spec = spec;
            status.controller = controller;
            status.revision = revision;
            if lifecycle_changed {
                status.changed_at = now_ms();
            }
        });
    }

    fn publish_active(&self, active: &Active, state: CoreState) {
        self.publish_instance(
            &active.instance,
            state,
            &active.source_spec,
            &active.revision,
        );
    }

    fn publish_instance(
        &self,
        instance: &Instance,
        state: CoreState,
        source_spec: &InstanceSpec,
        revision: &ConfigRevision,
    ) {
        let health = instance.state().borrow().health.clone();
        self.status_tx.send_modify(|status| {
            let lifecycle_changed = status.state != state;
            status.state = state;
            status.health = health;
            status.spec = Some(spec_summary(source_spec));
            status.controller = Some(instance.controller().host.clone());
            status.revision = Some(revision.clone());
            if lifecycle_changed {
                status.changed_at = now_ms();
            }
        });
    }

    fn publish_epoch_status(&self, epoch: u64, instance: InstanceStatus) {
        self.status_tx
            .send_if_modified(|status| apply_epoch_status(status, epoch, &instance));
    }
}

fn default_health_for_state(
    previous: Option<&HealthStatus>,
    state: &CoreState,
) -> Option<HealthStatus> {
    let target = match state {
        CoreState::Starting { .. } | CoreState::Restarting { .. } | CoreState::Switching { .. } => {
            crate::state::HealthState::Starting
        }
        CoreState::Running { .. } => crate::state::HealthState::Healthy,
        CoreState::Stopping { .. } | CoreState::Stopped { .. } => return None,
    };
    let mut health = HealthStatus::starting();
    health.state = target;
    if let Some(previous) = previous.filter(|status| status.state == target) {
        health.changed_at = previous.changed_at;
        health.consecutive_failures = previous.consecutive_failures;
        health.last_error.clone_from(&previous.last_error);
        health.last_success_at = previous.last_success_at;
    }
    Some(health)
}

fn apply_epoch_status(status: &mut CoreStatus, epoch: u64, instance: &InstanceStatus) -> bool {
    if status.revision.as_ref().map(|revision| revision.epoch) != Some(epoch) {
        return false;
    }
    let state = instance_core_state(epoch, &instance.state);
    let lifecycle_changed = status.state != state;
    let health_changed = status.health != instance.health;
    if !lifecycle_changed && !health_changed {
        return false;
    }
    status.state = state;
    status.health = instance.health.clone();
    if lifecycle_changed {
        status.changed_at = now_ms();
    }
    true
}

fn spec_summary(spec: &InstanceSpec) -> SpecSummary {
    SpecSummary {
        kind: spec.core.kind,
        config_path: spec.config_path.clone(),
    }
}

impl CoreManagerBuilder {
    pub fn readiness_probe(mut self, probe: ProbeHandle) -> Self {
        self.probes.readiness = Some(probe);
        self
    }

    pub fn liveness_probe(mut self, probe: ProbeHandle) -> Self {
        self.probes.liveness = Some(probe);
        self.probes.liveness_with_readiness = false;
        self
    }

    pub fn liveness_with_readiness_probe(mut self) -> Self {
        self.probes.liveness = None;
        self.probes.liveness_with_readiness = true;
        self
    }

    pub async fn build(self) -> Result<CoreManager, Error> {
        CoreManager::build_configured(self).await
    }
}

impl CoreManager {
    pub fn builder(options: ManagerOptions) -> CoreManagerBuilder {
        CoreManagerBuilder {
            options,
            probes: ProbePlan::default(),
        }
    }

    pub async fn new(options: ManagerOptions) -> Result<Self, Error> {
        Self::builder(options).build().await
    }

    async fn build_configured(builder: CoreManagerBuilder) -> Result<Self, Error> {
        let CoreManagerBuilder { options, probes } = builder;
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
        let runtime_lock = store.acquire_ownership().await?;

        if let ControllerMode::Managed {
            controller_template,
            ..
        } = &options.controller_mode
        {
            config::managed_endpoint_path(store.dir(), controller_template.as_deref(), 0)?;
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
        let max_epoch = sweep_orphans(&store).await?;
        let (status_tx, _) = watch::channel(CoreStatus::initial());
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                probes,
                store,
                ctrl: tokio::sync::Mutex::default(),
                status_tx,
                epoch: AtomicU64::new(max_epoch),
                _runtime_lock: runtime_lock,
            }),
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<CoreStatus> {
        self.inner.status_tx.subscribe()
    }

    pub fn status(&self) -> CoreStatus {
        self.inner.status_tx.borrow().clone()
    }

    /// Test-only fault hook for the installed-but-parent-sync-failed branch.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn inject_runtime_parent_sync_failure_once_for_test(&self) {
        self.inner.store.inject_replace_parent_sync_failure_once();
    }

    pub async fn apply_config(
        &self,
        input: InstanceSpec,
        expected_revision: Option<RevisionId>,
    ) -> Result<ApplyOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        reject_quarantine(&ctrl)?;
        let current = ctrl.current.as_ref().ok_or(Error::NotStarted)?;
        if current.instance.state().borrow().state.is_terminal() {
            return Err(Error::NotStarted);
        }
        let actual_revision = current.revision.id();
        if let Some(expected) = expected_revision
            && expected != actual_revision
        {
            return Err(Error::RevisionConflict {
                expected,
                actual: Some(actual_revision),
            });
        }

        let snapshot = ConfigSnapshot::load(&input.config_path).await?;
        let prepared = self
            .prepare_apply(current, input.clone(), &snapshot)
            .await?;
        let change = config_diff::classify(
            &current.source_document,
            &current.effective_document,
            &current.source_spec,
            &prepared.source_document,
            &prepared.effective_document,
            &prepared.source_spec,
        )?;
        if matches!(change, ConfigChange::Noop) {
            return Ok(ApplyOutcome::Noop {
                revision: current.revision.clone(),
            });
        }
        if matches!(change, ConfigChange::Switch) {
            drop(prepared);
            return self
                .switch_with_compensation(&mut ctrl, input, snapshot)
                .await;
        }

        let backup = self
            .inner
            .store
            .backup(current.revision.epoch, prepared.revision.generation)
            .await?;
        let PreparedApply {
            source_spec,
            effective_spec,
            controller,
            revision,
            source_document,
            effective_document,
            staged,
        } = prepared;
        let commit = match self
            .inner
            .store
            .commit_replace(staged, revision.epoch)
            .await
        {
            Ok(commit) => commit,
            Err(error) => {
                let _ = self.inner.store.remove_backup(backup).await;
                return Err(error);
            }
        };
        let durability_warning = commit.durability_warning().map(str::to_owned);
        let desired = PreparedLaunch {
            source_spec,
            effective_spec,
            controller,
            revision,
            source_document,
            effective_document,
        };

        let reconciled = tokio::time::timeout(
            self.inner.options.reconcile_timeout,
            self.reconcile_in_place(current, &change, &desired),
        )
        .await
        .unwrap_or(false);
        if reconciled {
            let revision = desired.revision.clone();
            let outcome = match change {
                ConfigChange::Patch { .. } => ApplyOutcome::Patched {
                    revision: revision.clone(),
                },
                ConfigChange::Reload => ApplyOutcome::Reloaded {
                    revision: revision.clone(),
                },
                ConfigChange::Noop | ConfigChange::Switch => unreachable!(),
            };
            let source_spec = {
                let active = ctrl.current.as_mut().expect("current held by control lock");
                active.source_spec = desired.source_spec;
                active.revision = desired.revision;
                active.source_document = desired.source_document;
                active.effective_document = desired.effective_document;
                self.inner.publish_active(
                    active,
                    CoreState::Running {
                        epoch: revision.epoch,
                        pid: active.instance.pid().unwrap_or_default(),
                    },
                );
                active.source_spec.clone()
            };
            ctrl.last_spec = Some(source_spec);
            if let Err(error) = self.inner.store.remove_backup(backup).await {
                tracing::warn!("failed to remove successful apply backup: {error}");
            }
            return Ok(with_durability_warning(outcome, durability_warning));
        }

        let result = self
            .restart_with_compensation(&mut ctrl, desired, backup)
            .await;
        with_durability_result(result, durability_warning)
    }

    async fn prepare_apply(
        &self,
        current: &Active,
        input: InstanceSpec,
        snapshot: &ConfigSnapshot,
    ) -> Result<PreparedApply, Error> {
        if tokio::fs::metadata(&input.core.binary_path).await.is_err() {
            return Err(Error::BinaryNotFound(input.core.binary_path.clone()));
        }
        input
            .core
            .kind
            .run_args(&input.working_dir, &input.config_path)?;
        let epoch = current.revision.epoch;
        let prepared = snapshot.prepare_full(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
        )?;
        let staged = self.inner.store.stage(epoch, &prepared.bytes).await?;
        let mut check_spec = input.clone();
        check_spec.config_path = staged.path().to_owned();
        crate::kind::check_config(&check_spec).await?;

        let runtime_path = current.revision.runtime_path.clone();
        let mut effective_spec = input.clone();
        effective_spec.config_path = runtime_path.clone();
        effective_spec.pid_file = Some(self.inner.store.pid_path(epoch));
        Ok(PreparedApply {
            source_spec: input,
            effective_spec,
            controller: prepared.controller,
            revision: ConfigRevision {
                epoch,
                generation: current.revision.generation + 1,
                source_hash: prepared.source_hash,
                effective_hash: prepared.effective_hash,
                runtime_path,
            },
            source_document: snapshot.document().clone(),
            effective_document: prepared.document,
            staged,
        })
    }

    async fn reconcile_in_place(
        &self,
        current: &Active,
        change: &ConfigChange,
        desired: &PreparedLaunch,
    ) -> bool {
        if let ConfigChange::Patch { patch, projection } = change {
            return self
                .patch_and_verify(&current.instance, patch, projection)
                .await;
        }
        if matches!(change, ConfigChange::Switch) {
            return false;
        }
        if matches!(change, ConfigChange::Noop) {
            return true;
        }
        let client = match crate::health::build_control_client(
            current.instance.controller(),
            self.inner.options.control_timeout,
        ) {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!("failed to build config control client: {error}");
                return false;
            }
        };
        match change {
            ConfigChange::Reload => {
                let request = clash_api::UpdateConfigRequest::from_path(
                    desired.revision.runtime_path.to_string(),
                );
                if let Err(error) = client
                    .update_config(&request, clash_api::UpdateConfigOptions { force: true })
                    .await
                {
                    tracing::warn!("config PUT failed: {error}");
                    return false;
                }
            }
            ConfigChange::Patch { .. } | ConfigChange::Switch | ConfigChange::Noop => {
                unreachable!()
            }
        }
        current
            .instance
            .probe_now(ProbePhase::Reconcile)
            .await
            .is_healthy()
    }

    async fn patch_and_verify(
        &self,
        instance: &Instance,
        patch: &clash_api::ConfigPatch,
        projection: &config_diff::RuntimeProjection,
    ) -> bool {
        let client = match crate::health::build_control_client(
            instance.controller(),
            self.inner.options.control_timeout,
        ) {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!("failed to build config control client: {error}");
                return false;
            }
        };
        if let Err(error) = client.patch_config(patch).await {
            tracing::warn!("config PATCH returned an uncertain result: {error}");
        }
        match client.configs().await {
            Ok(runtime) => match projection.verify(&runtime) {
                Ok(true) => {}
                Ok(false) => return false,
                Err(error) => {
                    tracing::warn!("failed to verify config projection: {error}");
                    return false;
                }
            },
            Err(error) => {
                tracing::warn!("GET /configs verification failed: {error}");
                return false;
            }
        }
        instance.probe_now(ProbePhase::Reconcile).await.is_healthy()
    }

    async fn restart_with_compensation(
        &self,
        ctrl: &mut Ctrl,
        desired: PreparedLaunch,
        backup: crate::RuntimeConfigBackup,
    ) -> Result<ApplyOutcome, Error> {
        let old = ctrl.current.take().expect("current held by control lock");
        let old_effective_spec = old.instance.spec().clone();
        let old_controller = old.instance.controller().clone();
        let old_source_spec = old.source_spec.clone();
        let old_revision = old.revision.clone();
        let old_source_document = old.source_document.clone();
        let old_effective_document = old.effective_document.clone();
        abort_and_await(old.forwarder).await;
        if let Err(error) = old
            .instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            if matches!(error, Error::StopUnconfirmed(_)) {
                return Err(self.latch_quarantine(ctrl, old_revision.epoch, error));
            }
            let message = format!("failed to stop current epoch for reconcile: {error}");
            self.publish_terminal_error(&Error::ApplyFailed(message.clone()));
            return Err(Error::ApplyFailed(message));
        }

        self.inner.publish(
            CoreState::Restarting {
                epoch: desired.revision.epoch,
                attempt: 0,
            },
            Some(spec_summary(&desired.source_spec)),
            Some(desired.controller.host.clone()),
            Some(desired.revision.clone()),
        );

        match self
            .spawn_replacement(
                desired.effective_spec.clone(),
                desired.revision.epoch,
                desired.controller.clone(),
            )
            .await
        {
            Ok(instance) => {
                let revision = desired.revision.clone();
                let pid = instance.pid().unwrap_or_default();
                let forwarder = spawn_forwarder(&self.inner, instance.state(), revision.epoch);
                ctrl.last_spec = Some(desired.source_spec.clone());
                ctrl.current = Some(Active {
                    instance,
                    forwarder,
                    source_spec: desired.source_spec,
                    revision: desired.revision,
                    source_document: desired.source_document,
                    effective_document: desired.effective_document,
                });
                let active = ctrl.current.as_ref().expect("just installed");
                self.inner.publish_active(
                    active,
                    CoreState::Running {
                        epoch: revision.epoch,
                        pid,
                    },
                );
                if let Err(error) = self.inner.store.remove_backup(backup).await {
                    tracing::warn!("failed to remove successful restart backup: {error}");
                }
                Ok(ApplyOutcome::Restarted { revision })
            }
            Err(error @ Error::StopUnconfirmed(_)) => {
                Err(self.latch_quarantine(ctrl, desired.revision.epoch, error))
            }
            Err(apply_error) => {
                let apply_text = apply_error.to_string();
                let restore = match self.inner.store.restore(&backup).await {
                    Ok(restore) => restore,
                    Err(restore_error) => {
                        let error = Error::ApplyRollbackFailed {
                            apply: apply_text,
                            rollback: format!("runtime restore failed: {restore_error}"),
                        };
                        self.publish_terminal_error(&error);
                        return Err(error);
                    }
                };
                let restore_warning = restore.durability_warning().map(str::to_owned);
                self.inner.publish(
                    CoreState::Restarting {
                        epoch: old_revision.epoch,
                        attempt: 0,
                    },
                    Some(spec_summary(&old_source_spec)),
                    Some(old_controller.host.clone()),
                    Some(old_revision.clone()),
                );
                let rollback = match self
                    .spawn_replacement(old_effective_spec, old_revision.epoch, old_controller)
                    .await
                {
                    Ok(instance) => {
                        let pid = instance.pid().unwrap_or_default();
                        let forwarder =
                            spawn_forwarder(&self.inner, instance.state(), old_revision.epoch);
                        ctrl.last_spec = Some(old_source_spec.clone());
                        ctrl.current = Some(Active {
                            instance,
                            forwarder,
                            source_spec: old_source_spec,
                            revision: old_revision.clone(),
                            source_document: old_source_document,
                            effective_document: old_effective_document,
                        });
                        let active = ctrl.current.as_ref().expect("rollback installed");
                        self.inner.publish_active(
                            active,
                            CoreState::Running {
                                epoch: old_revision.epoch,
                                pid,
                            },
                        );
                        if let Err(error) = self.inner.store.remove_backup(backup).await {
                            tracing::warn!("failed to remove rollback backup: {error}");
                        }
                        Ok(ApplyOutcome::RolledBack {
                            revision: old_revision,
                            failed_apply: apply_text,
                        })
                    }
                    Err(rollback_error @ Error::StopUnconfirmed(_)) => {
                        let error = Error::StopUnconfirmed(format!(
                            "desired apply failed ({apply_text}); rollback replacement {rollback_error}"
                        ));
                        Err(self.latch_quarantine(ctrl, old_revision.epoch, error))
                    }
                    Err(rollback_error) => {
                        let error = Error::ApplyRollbackFailed {
                            apply: apply_text,
                            rollback: rollback_error.to_string(),
                        };
                        self.publish_terminal_error(&error);
                        Err(error)
                    }
                };
                with_durability_result(rollback, restore_warning)
            }
        }
    }

    async fn switch_with_compensation(
        &self,
        ctrl: &mut Ctrl,
        input: InstanceSpec,
        snapshot: ConfigSnapshot,
    ) -> Result<ApplyOutcome, Error> {
        let epoch = self.next_epoch();
        let desired = self.prepare_launch(&input, epoch, &snapshot).await?;
        let old = ctrl.current.take().expect("current held by control lock");
        let old_epoch = old.revision.epoch;
        let old_effective_spec = old.instance.spec().clone();
        let old_controller = old.instance.controller().clone();
        let old_source_spec = old.source_spec.clone();
        let old_revision = old.revision.clone();
        let old_source_document = old.source_document.clone();
        let old_effective_document = old.effective_document.clone();
        abort_and_await(old.forwarder).await;
        if let Err(error) = old
            .instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            let _ = self.inner.store.cleanup_epoch(epoch).await;
            if matches!(error, Error::StopUnconfirmed(_)) {
                return Err(self.latch_quarantine(ctrl, old_epoch, error));
            }
            let message = format!("failed to stop current epoch for switch: {error}");
            self.publish_terminal_error(&Error::ApplyFailed(message.clone()));
            return Err(Error::ApplyFailed(message));
        }

        self.inner.publish(
            CoreState::Switching {
                from: Some(old_epoch),
                to: desired.revision.epoch,
            },
            Some(spec_summary(&desired.source_spec)),
            Some(desired.controller.host.clone()),
            Some(desired.revision.clone()),
        );

        match self
            .spawn_replacement(
                desired.effective_spec.clone(),
                desired.revision.epoch,
                desired.controller.clone(),
            )
            .await
        {
            Ok(instance) => {
                let revision = desired.revision.clone();
                let pid = instance.pid().unwrap_or_default();
                let forwarder = spawn_forwarder(&self.inner, instance.state(), revision.epoch);
                ctrl.last_spec = Some(desired.source_spec.clone());
                ctrl.current = Some(Active {
                    instance,
                    forwarder,
                    source_spec: desired.source_spec,
                    revision: desired.revision,
                    source_document: desired.source_document,
                    effective_document: desired.effective_document,
                });
                let active = ctrl.current.as_ref().expect("switch installed");
                self.inner.publish_active(
                    active,
                    CoreState::Running {
                        epoch: revision.epoch,
                        pid,
                    },
                );
                if let Err(error) = self.inner.store.cleanup_epoch(old_epoch).await {
                    tracing::warn!("failed to clean switched-out epoch: {error}");
                }
                Ok(ApplyOutcome::Restarted { revision })
            }
            Err(error @ Error::StopUnconfirmed(_)) => {
                Err(self.latch_quarantine(ctrl, desired.revision.epoch, error))
            }
            Err(apply_error) => {
                let apply_text = apply_error.to_string();
                if let Err(error) = self.inner.store.cleanup_epoch(epoch).await {
                    tracing::warn!("failed to clean rejected desired epoch: {error}");
                }
                self.inner.publish(
                    CoreState::Restarting {
                        epoch: old_revision.epoch,
                        attempt: 0,
                    },
                    Some(spec_summary(&old_source_spec)),
                    Some(old_controller.host.clone()),
                    Some(old_revision.clone()),
                );
                match self
                    .spawn_replacement(old_effective_spec, old_revision.epoch, old_controller)
                    .await
                {
                    Ok(instance) => {
                        let pid = instance.pid().unwrap_or_default();
                        let forwarder =
                            spawn_forwarder(&self.inner, instance.state(), old_revision.epoch);
                        ctrl.last_spec = Some(old_source_spec.clone());
                        ctrl.current = Some(Active {
                            instance,
                            forwarder,
                            source_spec: old_source_spec,
                            revision: old_revision.clone(),
                            source_document: old_source_document,
                            effective_document: old_effective_document,
                        });
                        let active = ctrl.current.as_ref().expect("switch rollback installed");
                        self.inner.publish_active(
                            active,
                            CoreState::Running {
                                epoch: old_revision.epoch,
                                pid,
                            },
                        );
                        Ok(ApplyOutcome::RolledBack {
                            revision: old_revision,
                            failed_apply: apply_text,
                        })
                    }
                    Err(rollback_error @ Error::StopUnconfirmed(_)) => {
                        let error = Error::StopUnconfirmed(format!(
                            "desired switch failed ({apply_text}); rollback replacement {rollback_error}"
                        ));
                        Err(self.latch_quarantine(ctrl, old_revision.epoch, error))
                    }
                    Err(rollback_error) => {
                        let error = Error::ApplyRollbackFailed {
                            apply: apply_text,
                            rollback: rollback_error.to_string(),
                        };
                        self.publish_terminal_error(&error);
                        Err(error)
                    }
                }
            }
        }
    }

    async fn spawn_replacement(
        &self,
        effective_spec: InstanceSpec,
        epoch: u64,
        controller: ResolvedController,
    ) -> Result<Instance, Error> {
        let instance = self
            .spawn_instance(effective_spec, epoch, controller)
            .await?;
        if let Err(error) = instance.wait_ready().await {
            return match instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                Ok(()) => Err(error),
                Err(stop_error) => Err(Error::StopUnconfirmed(format!(
                    "{error}; failed to stop rejected replacement: {stop_error}"
                ))),
            };
        }
        Ok(instance)
    }

    async fn spawn_instance(
        &self,
        effective_spec: InstanceSpec,
        epoch: u64,
        controller: ResolvedController,
    ) -> Result<Instance, Error> {
        let mut builder = Instance::builder(
            effective_spec,
            epoch,
            controller,
            self.inner.options.cancel_token.clone(),
        );
        if let Some(probe) = self.inner.probes.readiness.clone() {
            builder = builder.readiness_probe(probe);
        }
        if let Some(probe) = self.inner.probes.liveness.clone() {
            builder = builder.liveness_probe(probe);
        }
        if self.inner.probes.liveness_with_readiness {
            builder = builder.liveness_with_readiness_probe();
        }
        builder.spawn().await
    }

    fn next_epoch(&self) -> u64 {
        self.inner.epoch.fetch_add(1, Ordering::Relaxed) + 1
    }

    async fn prepare_launch(
        &self,
        spec: &InstanceSpec,
        epoch: u64,
        snapshot: &ConfigSnapshot,
    ) -> Result<PreparedLaunch, Error> {
        debug_assert_eq!(snapshot.source_path(), spec.config_path);
        if tokio::fs::metadata(&spec.core.binary_path).await.is_err() {
            return Err(Error::BinaryNotFound(spec.core.binary_path.clone()));
        }
        spec.core
            .kind
            .run_args(&spec.working_dir, &spec.config_path)?;
        let prepared = snapshot.prepare_full(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
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
            source_document: snapshot.document().clone(),
            effective_document: prepared.document,
        })
    }

    async fn prepare_graceful(
        &self,
        spec: &InstanceSpec,
        epoch: u64,
        snapshot: &ConfigSnapshot,
    ) -> Result<PreparedGraceful, Error> {
        debug_assert_eq!(snapshot.source_path(), spec.config_path);
        if tokio::fs::metadata(&spec.core.binary_path).await.is_err() {
            return Err(Error::BinaryNotFound(spec.core.binary_path.clone()));
        }
        spec.core
            .kind
            .run_args(&spec.working_dir, &spec.config_path)?;
        let full = snapshot.prepare_full(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
        )?;
        let bootstrap = snapshot.prepare_bootstrap(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
        )?;
        if full.controller.host != bootstrap.controller.host
            || full.controller.secret != bootstrap.controller.secret
        {
            return Err(Error::InvalidConfig(
                "full and bootstrap configs resolved different controllers".into(),
            ));
        }
        let restoration = config_diff::restoration_patch(&bootstrap.document, &full.document)?;

        let full_staged = self.inner.store.stage(epoch, &full.bytes).await?;
        let mut check_spec = spec.clone();
        check_spec.config_path = full_staged.path().to_owned();
        crate::kind::check_config(&check_spec).await?;

        let bootstrap_staged = self.inner.store.stage(epoch, &bootstrap.bytes).await?;
        check_spec.config_path = bootstrap_staged.path().to_owned();
        crate::kind::check_config(&check_spec).await?;
        let runtime_path = self.inner.store.commit_new(bootstrap_staged, epoch).await?;

        let mut effective_spec = spec.clone();
        effective_spec.config_path = runtime_path.clone();
        effective_spec.pid_file = Some(self.inner.store.pid_path(epoch));
        Ok(PreparedGraceful {
            launch: PreparedLaunch {
                source_spec: spec.clone(),
                effective_spec,
                controller: full.controller,
                revision: ConfigRevision {
                    epoch,
                    generation: 1,
                    source_hash: full.source_hash,
                    effective_hash: full.effective_hash,
                    runtime_path,
                },
                source_document: snapshot.document().clone(),
                effective_document: full.document,
            },
            full_staged,
            restoration,
        })
    }

    pub async fn start(&self, spec: InstanceSpec) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        reject_quarantine(&ctrl)?;
        let running = ctrl
            .current
            .as_ref()
            .is_some_and(|active| !active.instance.state().borrow().state.is_terminal());
        if running {
            return Err(Error::AlreadyRunning);
        }
        if let Some(stale) = ctrl.current.take() {
            abort_and_await(stale.forwarder).await;
            let epoch = stale.instance.epoch();
            if let Err(error) = stale
                .instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                if matches!(error, Error::StopUnconfirmed(_)) {
                    return Err(self.latch_quarantine(&mut ctrl, epoch, error));
                }
                return Err(error);
            }
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
        let prepared = match self.prepare_launch(&spec, epoch, &snapshot).await {
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
        let instance = match self
            .spawn_instance(prepared.effective_spec, epoch, prepared.controller)
            .await
        {
            Ok(instance) => instance,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.publish_terminal_error(&error);
                return Err(error);
            }
        };

        if let Err(readiness_error) = instance.wait_ready().await {
            match instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                Ok(()) => {
                    let _ = self.inner.store.cleanup_epoch(epoch).await;
                    self.publish_terminal_error(&readiness_error);
                    return Err(readiness_error);
                }
                Err(stop_error) => {
                    let error = Error::StopUnconfirmed(format!(
                        "{readiness_error}; failed to stop rejected initial instance: {stop_error}"
                    ));
                    return Err(self.latch_quarantine(ctrl, epoch, error));
                }
            }
        }

        let pid = instance.pid().unwrap_or_default();
        self.inner.publish_instance(
            &instance,
            CoreState::Running { epoch, pid },
            &prepared.source_spec,
            &prepared.revision,
        );
        let forwarder = spawn_forwarder(&self.inner, instance.state(), epoch);
        ctrl.last_spec = Some(prepared.source_spec.clone());
        ctrl.current = Some(Active {
            instance,
            forwarder,
            source_spec: prepared.source_spec,
            revision: prepared.revision,
            source_document: prepared.source_document,
            effective_document: prepared.effective_document,
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

    fn latch_quarantine(&self, ctrl: &mut Ctrl, epoch: u64, error: Error) -> Error {
        record_quarantine(ctrl, epoch, error.to_string());
        let quarantine = quarantine_error(ctrl).expect("quarantine was just inserted");
        self.publish_terminal_error(&quarantine);
        error
    }

    /// Attempts identity-verified recovery of every uncertain epoch. Manager
    /// operations remain rejected until every quarantined process is proven
    /// dead and its artifacts are cleaned.
    pub async fn recover_quarantine(&self) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        if ctrl.quarantine.is_empty() {
            return Ok(());
        }
        let quarantined = ctrl.quarantine.clone();
        let mut failures = Vec::new();
        for entry in quarantined {
            if !entry.death_proven {
                let pid_path = self.inner.store.pid_path(entry.epoch);
                match reap_epoch_pid_file(
                    pid_path.as_std_path(),
                    self.inner.store.dir().as_std_path(),
                )
                .await
                {
                    Ok(OrphanReapOutcome::AlreadyExited | OrphanReapOutcome::Killed) => {
                        if let Some(quarantine) = ctrl
                            .quarantine
                            .iter_mut()
                            .find(|quarantine| quarantine.epoch == entry.epoch)
                        {
                            quarantine.death_proven = true;
                        }
                    }
                    Ok(OrphanReapOutcome::NotFound) => {
                        failures.push(format!(
                            "epoch {}: {}; authoritative epoch pid record is unavailable",
                            entry.epoch, entry.reason
                        ));
                        continue;
                    }
                    Err(error) => {
                        failures.push(format!(
                            "epoch {}: {}; recovery failed: {error}",
                            entry.epoch, entry.reason
                        ));
                        continue;
                    }
                }
            }

            match self.inner.store.cleanup_epoch(entry.epoch).await {
                Ok(()) => ctrl
                    .quarantine
                    .retain(|quarantine| quarantine.epoch != entry.epoch),
                Err(error) => failures.push(format!(
                    "epoch {}: {}; artifact cleanup failed: {error}",
                    entry.epoch, entry.reason
                )),
            }
        }
        if !failures.is_empty() {
            let first_epoch = ctrl
                .quarantine
                .first()
                .map(|entry| entry.epoch)
                .unwrap_or_default();
            let error = Error::ManagerQuarantined {
                epoch: first_epoch,
                reason: failures.join(" | "),
            };
            return Err(error);
        }
        self.inner
            .publish(CoreState::Stopped { reason: None }, None, None, None);
        Ok(())
    }

    pub async fn restart(&self) -> Result<SwitchOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        reject_quarantine(&ctrl)?;
        let spec = ctrl.last_spec.clone().ok_or(Error::NotStarted)?;
        self.switch_locked(&mut ctrl, spec).await
    }

    pub async fn switch(&self, spec: InstanceSpec) -> Result<SwitchOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        reject_quarantine(&ctrl)?;
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
            .is_some_and(|active| !active.instance.state().borrow().state.is_terminal());
        if !running {
            if let Some(stale) = ctrl.current.take() {
                abort_and_await(stale.forwarder).await;
                let epoch = stale.instance.epoch();
                if let Err(error) = stale
                    .instance
                    .stop_and_confirm_dead(self.inner.options.stop_timeout)
                    .await
                {
                    if matches!(error, Error::StopUnconfirmed(_)) {
                        return Err(self.latch_quarantine(ctrl, epoch, error));
                    }
                    return Err(error);
                }
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
        match graceful_degrade_reason(
            managed,
            spec.core.kind,
            config_diff::overlap_block(snapshot.document()),
        ) {
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
        let prepared = match self.prepare_launch(&spec, epoch, &snapshot).await {
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
            if matches!(error, Error::StopUnconfirmed(_)) {
                return Err(self.latch_quarantine(ctrl, old_epoch, error));
            }
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
        let state = instance_core_state(
            active.instance.epoch(),
            &active.instance.state().borrow().state,
        );
        self.inner.publish_active(active, state);
    }

    fn install_switched(&self, ctrl: &mut Ctrl, instance: Instance, prepared: PreparedLaunch) {
        let epoch = prepared.revision.epoch;
        let pid = instance.pid().unwrap_or_default();
        self.inner.publish_instance(
            &instance,
            CoreState::Running { epoch, pid },
            &prepared.source_spec,
            &prepared.revision,
        );
        let forwarder = spawn_forwarder(&self.inner, instance.state(), epoch);
        ctrl.last_spec = Some(prepared.source_spec.clone());
        ctrl.current = Some(Active {
            instance,
            forwarder,
            source_spec: prepared.source_spec,
            revision: prepared.revision,
            source_document: prepared.source_document,
            effective_document: prepared.effective_document,
        });
    }

    async fn graceful_switch(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
        snapshot: ConfigSnapshot,
    ) -> Result<SwitchOutcome, Error> {
        let old_epoch = ctrl.current.as_ref().map(|active| active.instance.epoch());
        let epoch = self.next_epoch();
        let prepared = match self.prepare_graceful(&spec, epoch, &snapshot).await {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        let PreparedGraceful {
            launch,
            full_staged,
            restoration,
        } = prepared;
        self.inner.publish(
            CoreState::Switching {
                from: old_epoch,
                to: epoch,
            },
            Some(spec_summary(&launch.source_spec)),
            Some(launch.controller.host.clone()),
            Some(launch.revision.clone()),
        );

        let instance = match self
            .spawn_instance(
                launch.effective_spec.clone(),
                epoch,
                launch.controller.clone(),
            )
            .await
        {
            Ok(instance) => instance,
            Err(error) => {
                drop(full_staged);
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        if let Err(error) = instance.wait_ready().await {
            drop(full_staged);
            match instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                Ok(()) => {
                    let _ = self.inner.store.cleanup_epoch(epoch).await;
                    self.republish_retained(ctrl);
                    return Err(error);
                }
                Err(stop_error) => {
                    let error = Error::StopUnconfirmed(format!(
                        "{error}; failed to stop rejected graceful bootstrap: {stop_error}"
                    ));
                    return Err(self.latch_quarantine(ctrl, epoch, error));
                }
            }
        }

        let old = ctrl.current.take().expect("running checked by caller");
        abort_and_await(old.forwarder).await;
        let old_epoch = old.instance.epoch();
        if let Err(error) = old
            .instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            drop(full_staged);
            let old_uncertain = matches!(error, Error::StopUnconfirmed(_));
            let old_reason = error.to_string();
            let new_stop = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await;
            match new_stop {
                Ok(()) => {
                    let _ = self.inner.store.cleanup_epoch(epoch).await;
                    if old_uncertain {
                        return Err(self.latch_quarantine(ctrl, old_epoch, error));
                    }
                    self.publish_terminal_error(&error);
                    return Err(error);
                }
                Err(new_error) => {
                    if old_uncertain {
                        record_quarantine(ctrl, old_epoch, old_reason);
                    }
                    let error = Error::StopUnconfirmed(format!(
                        "old epoch stop failed: {error}; new bootstrap stop also failed: {new_error}"
                    ));
                    return Err(self.latch_quarantine(ctrl, epoch, error));
                }
            }
        }

        let commit = match self.inner.store.commit_replace(full_staged, epoch).await {
            Ok(commit) => commit,
            Err(error) => {
                let new_stop = instance
                    .stop_and_confirm_dead(self.inner.options.stop_timeout)
                    .await;
                if new_stop.is_ok() {
                    let _ = self.inner.store.cleanup_epoch(epoch).await;
                }
                let error = match new_stop {
                    Ok(()) => error,
                    Err(new_error) => Error::StopUnconfirmed(format!(
                        "full runtime commit failed: {error}; bootstrap stop also failed: {new_error}"
                    )),
                };
                if matches!(error, Error::StopUnconfirmed(_)) {
                    return Err(self.latch_quarantine(ctrl, epoch, error));
                }
                self.publish_terminal_error(&error);
                return Err(error);
            }
        };
        let durability_warning = commit.durability_warning().map(str::to_owned);
        if let Some(warning) = durability_warning.as_deref() {
            tracing::warn!("graceful runtime replacement durability is uncertain: {warning}");
        }

        let reconciled = tokio::time::timeout(self.inner.options.reconcile_timeout, async {
            match restoration.as_ref() {
                Some((patch, projection)) => {
                    self.patch_and_verify(&instance, patch, projection).await
                }
                None => instance.probe_now(ProbePhase::Reconcile).await.is_healthy(),
            }
        })
        .await
        .unwrap_or(false);
        if reconciled {
            self.install_switched(ctrl, instance, launch);
            let result = self
                .inner
                .store
                .cleanup_epoch(old_epoch)
                .await
                .map(|()| SwitchOutcome::Graceful);
            return with_switch_durability_result(result, durability_warning);
        }

        let effective_spec = launch.effective_spec.clone();
        let controller = launch.controller.clone();
        if let Err(error) = instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            let error = if matches!(error, Error::StopUnconfirmed(_)) {
                self.latch_quarantine(ctrl, epoch, error)
            } else {
                self.publish_terminal_error(&error);
                error
            };
            return with_switch_durability_result(Err(error), durability_warning);
        }
        let replacement = match self
            .spawn_replacement(effective_spec, epoch, controller)
            .await
        {
            Ok(replacement) => replacement,
            Err(error @ Error::StopUnconfirmed(_)) => {
                let error = self.latch_quarantine(ctrl, epoch, error);
                return with_switch_durability_result(Err(error), durability_warning);
            }
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.publish_terminal_error(&error);
                return with_switch_durability_result(Err(error), durability_warning);
            }
        };
        self.install_switched(ctrl, replacement, launch);
        let result =
            self.inner
                .store
                .cleanup_epoch(old_epoch)
                .await
                .map(|()| SwitchOutcome::Hard {
                    reason: DegradeReason::PatchFailed,
                });
        with_switch_durability_result(result, durability_warning)
    }

    /// Stops the active instance even while another epoch is quarantined.
    ///
    /// This intentionally bypasses the quarantine gate so callers can reduce
    /// the number of possibly live processes; it does not clear quarantine.
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
            ..
        } = active;
        let captured_status = instance.state().borrow().clone();
        abort_and_await(forwarder).await;
        if captured_status.state.is_terminal() {
            let epoch = instance.epoch();
            self.inner.publish(
                instance_core_state(epoch, &captured_status.state),
                Some(spec_summary(&source_spec)),
                Some(instance.controller().host.clone()),
                Some(revision),
            );
            if let Err(error) = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                if matches!(error, Error::StopUnconfirmed(_)) {
                    return Err(self.latch_quarantine(&mut ctrl, epoch, error));
                }
                return Err(error);
            }
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
            if matches!(error, Error::StopUnconfirmed(_)) {
                return Err(self.latch_quarantine(&mut ctrl, epoch, error));
            }
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

    /// Stops the active instance even while another epoch is quarantined.
    ///
    /// Like [`Self::stop`], shutdown intentionally bypasses the quarantine
    /// gate and never treats an unrelated uncertain epoch as recovered.
    pub async fn shutdown(&self) -> Result<(), Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        if let Some(active) = ctrl.current.take() {
            let Active {
                instance,
                forwarder,
                source_spec,
                revision,
                ..
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
                if matches!(error, Error::StopUnconfirmed(_)) {
                    return Err(self.latch_quarantine(&mut ctrl, epoch, error));
                }
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

fn quarantine_error(ctrl: &Ctrl) -> Option<Error> {
    let first = ctrl.quarantine.first()?;
    let reason = if ctrl.quarantine.len() == 1 {
        first.reason.clone()
    } else {
        let epochs = ctrl
            .quarantine
            .iter()
            .map(|entry| entry.epoch.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}; additional uncertain epochs: {epochs}", first.reason)
    };
    Some(Error::ManagerQuarantined {
        epoch: first.epoch,
        reason,
    })
}

fn record_quarantine(ctrl: &mut Ctrl, epoch: u64, reason: String) {
    if let Some(existing) = ctrl
        .quarantine
        .iter_mut()
        .find(|quarantine| quarantine.epoch == epoch)
    {
        existing.reason = reason;
    } else {
        ctrl.quarantine.push(QuarantinedEpoch {
            epoch,
            reason,
            death_proven: false,
        });
    }
}

fn reject_quarantine(ctrl: &Ctrl) -> Result<(), Error> {
    match quarantine_error(ctrl) {
        Some(error) => Err(error),
        None => Ok(()),
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
    inner: &Arc<Inner>,
    mut state_rx: watch::Receiver<InstanceStatus>,
    epoch: u64,
) -> tokio::task::JoinHandle<()> {
    let inner = Arc::downgrade(inner);
    tokio::spawn(async move {
        while state_rx.changed().await.is_ok() {
            let status = state_rx.borrow_and_update().clone();
            let terminal = status.state.is_terminal();
            let Some(inner) = inner.upgrade() else {
                break;
            };
            inner.publish_epoch_status(epoch, status);
            if terminal {
                break;
            }
        }
    })
}

fn with_durability_warning(outcome: ApplyOutcome, warning: Option<String>) -> ApplyOutcome {
    match warning {
        Some(warning) => ApplyOutcome::DurabilityUncertain {
            outcome: Box::new(outcome),
            warning,
        },
        None => outcome,
    }
}

fn with_durability_result(
    result: Result<ApplyOutcome, Error>,
    warning: Option<String>,
) -> Result<ApplyOutcome, Error> {
    match (result, warning) {
        (Ok(outcome), warning) => Ok(with_durability_warning(outcome, warning)),
        (Err(error), Some(warning)) => Err(Error::DurabilityUncertain {
            source: Box::new(error),
            warning,
        }),
        (Err(error), None) => Err(error),
    }
}

fn with_switch_durability_warning(
    outcome: SwitchOutcome,
    warning: Option<String>,
) -> SwitchOutcome {
    match warning {
        Some(warning) => SwitchOutcome::DurabilityUncertain {
            outcome: Box::new(outcome),
            warning,
        },
        None => outcome,
    }
}

fn with_switch_durability_result(
    result: Result<SwitchOutcome, Error>,
    warning: Option<String>,
) -> Result<SwitchOutcome, Error> {
    match (result, warning) {
        (Ok(outcome), warning) => Ok(with_switch_durability_warning(outcome, warning)),
        (Err(error), Some(warning)) => Err(Error::DurabilityUncertain {
            source: Box::new(error),
            warning,
        }),
        (Err(error), None) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_matrix_matches_the_spec() {
        assert_eq!(
            graceful_degrade_reason(false, CoreKind::Mihomo, None),
            Some(DegradeReason::PassthroughMode)
        );
        assert_eq!(
            graceful_degrade_reason(true, CoreKind::ClashRs, None),
            Some(DegradeReason::UnsupportedKind)
        );
        assert_eq!(
            graceful_degrade_reason(true, CoreKind::Mihomo, Some(OverlapBlock::DnsListen)),
            Some(DegradeReason::DnsListen)
        );
        assert_eq!(graceful_degrade_reason(true, CoreKind::Mihomo, None), None);
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
            let stale_status = InstanceStatus {
                state: match stale {
                    CoreState::Running { pid, .. } => InstanceState::Running { pid },
                    CoreState::Restarting { attempt, .. } => InstanceState::Restarting { attempt },
                    CoreState::Stopped { reason } => {
                        InstanceState::Stopped(reason.unwrap_or(StopReason::Finished))
                    }
                    _ => unreachable!(),
                },
                health: None,
            };
            assert!(!apply_epoch_status(&mut status, 8, &stale_status));
            assert!(matches!(
                status.state,
                CoreState::Running { epoch: 9, pid: 90 }
            ));
        }
    }

    #[test]
    fn stale_epoch_status_neither_mutates_nor_wakes_watchers() {
        let mut status = CoreStatus::initial();
        status.revision = Some(ConfigRevision {
            epoch: 9,
            generation: 1,
            source_hash: "source".into(),
            effective_hash: "effective".into(),
            runtime_path: "config-9.yaml".into(),
        });
        status.state = CoreState::Running { epoch: 9, pid: 90 };
        let (tx, rx) = watch::channel(status);
        let stale = InstanceStatus {
            state: InstanceState::Running { pid: 80 },
            health: Some(HealthStatus::starting()),
        };

        let sent = tx.send_if_modified(|status| apply_epoch_status(status, 8, &stale));

        assert!(!sent);
        assert!(!rx.has_changed().unwrap());
        assert!(matches!(
            rx.borrow().state,
            CoreState::Running { epoch: 9, pid: 90 }
        ));
    }

    #[test]
    fn pure_health_transition_preserves_lifecycle_changed_at() {
        let mut status = CoreStatus::initial();
        status.revision = Some(ConfigRevision {
            epoch: 3,
            generation: 1,
            source_hash: "source".into(),
            effective_hash: "effective".into(),
            runtime_path: "config-3.yaml".into(),
        });
        status.state = CoreState::Running { epoch: 3, pid: 30 };
        status.changed_at = 7;
        let mut health = HealthStatus::starting();
        health.state = crate::state::HealthState::Unhealthy;
        let instance = InstanceStatus {
            state: InstanceState::Running { pid: 30 },
            health: Some(health.clone()),
        };

        assert!(apply_epoch_status(&mut status, 3, &instance));
        assert_eq!(status.changed_at, 7);
        assert_eq!(status.health, Some(health));
    }

    #[test]
    fn durability_warning_preserves_structured_apply_error() {
        let result = with_durability_result(
            Err(Error::ApplyRollbackFailed {
                apply: "desired failed".into(),
                rollback: "rollback failed".into(),
            }),
            Some("directory sync failed".into()),
        );
        let Err(Error::DurabilityUncertain { source, warning }) = result else {
            panic!("structured error was flattened")
        };
        assert!(matches!(*source, Error::ApplyRollbackFailed { .. }));
        assert_eq!(warning, "directory sync failed");
    }

    #[test]
    fn durability_warning_wraps_stop_unconfirmed_without_flattening() {
        let apply = with_durability_result(
            Err(Error::StopUnconfirmed("apply stop uncertain".into())),
            Some("apply sync warning".into()),
        );
        let Err(Error::DurabilityUncertain { source, warning }) = apply else {
            panic!("apply stop uncertainty was not structurally wrapped")
        };
        assert!(matches!(*source, Error::StopUnconfirmed(_)));
        assert_eq!(warning, "apply sync warning");

        let switch = with_switch_durability_result(
            Err(Error::StopUnconfirmed("switch stop uncertain".into())),
            Some("switch sync warning".into()),
        );
        let Err(Error::DurabilityUncertain { source, warning }) = switch else {
            panic!("switch stop uncertainty was not structurally wrapped")
        };
        assert!(matches!(*source, Error::StopUnconfirmed(_)));
        assert_eq!(warning, "switch sync warning");
    }
}
