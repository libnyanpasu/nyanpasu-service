//! Cross-epoch orchestration: manager-owned artifacts, start/stop/switch, and
//! atomic status publication.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use nyanpasu_utils::process::reap_epoch_pid_file;
use serde_yaml_ng::Mapping;
use tokio::sync::watch;

use crate::{
    config::{self, ConfigSnapshot},
    config_diff::{self, ConfigChange, OverlapBlock},
    error::Error,
    instance::Instance,
    kind::CoreKind,
    runtime_store::{RuntimeConfigStore, StagedRuntimeConfig},
    spec::{ControllerMode, InstanceSpec, ManagerOptions, ResolvedController},
    state::{
        ConfigRevision, CoreState, CoreStatus, InstanceState, RevisionId, SpecSummary, StopReason,
        now_ms,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    Graceful,
    Hard { reason: DegradeReason },
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

    pub async fn apply_config(
        &self,
        input: InstanceSpec,
        expected_revision: Option<RevisionId>,
    ) -> Result<ApplyOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        let current = ctrl.current.as_ref().ok_or(Error::NotStarted)?;
        if current.instance.state().borrow().is_terminal() {
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
        if let Err(error) = self
            .inner
            .store
            .commit_replace(staged, revision.epoch)
            .await
        {
            let _ = self.inner.store.remove_backup(backup).await;
            return Err(error);
        }
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
            return Ok(outcome);
        }

        self.restart_with_compensation(&mut ctrl, desired, backup)
            .await
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
                .patch_and_verify(current.instance.controller(), patch, projection)
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
        self.probe_health(current.instance.controller()).await
    }

    async fn patch_and_verify(
        &self,
        controller: &ResolvedController,
        patch: &clash_api::ConfigPatch,
        projection: &config_diff::RuntimeProjection,
    ) -> bool {
        let client = match crate::health::build_control_client(
            controller,
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
        self.probe_health(controller).await
    }

    async fn probe_health(&self, controller: &ResolvedController) -> bool {
        match crate::health::HealthCheck::new(controller) {
            Ok(health) => health.probe_once().await,
            Err(error) => {
                tracing::warn!("failed to build post-apply health probe: {error}");
                false
            }
        }
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
            let message = format!("failed to stop current epoch for reconcile: {error}");
            self.publish_terminal_error(&Error::ApplyFailed(message.clone()));
            return Err(Error::ApplyFailed(message));
        }

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
                let forwarder =
                    spawn_forwarder(self.inner.clone(), instance.state(), revision.epoch);
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
            Err(apply_error) => {
                let apply_text = apply_error.to_string();
                if let Err(restore_error) = self.inner.store.restore(&backup).await {
                    let error = Error::ApplyRollbackFailed {
                        apply: apply_text,
                        rollback: format!("runtime restore failed: {restore_error}"),
                    };
                    self.publish_terminal_error(&error);
                    return Err(error);
                }
                match self
                    .spawn_replacement(old_effective_spec, old_revision.epoch, old_controller)
                    .await
                {
                    Ok(instance) => {
                        let pid = instance.pid().unwrap_or_default();
                        let forwarder = spawn_forwarder(
                            self.inner.clone(),
                            instance.state(),
                            old_revision.epoch,
                        );
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
            let message = format!("failed to stop current epoch for switch: {error}");
            self.publish_terminal_error(&Error::ApplyFailed(message.clone()));
            return Err(Error::ApplyFailed(message));
        }

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
                let forwarder =
                    spawn_forwarder(self.inner.clone(), instance.state(), revision.epoch);
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
            Err(apply_error) => {
                let apply_text = apply_error.to_string();
                if let Err(error) = self.inner.store.cleanup_epoch(epoch).await {
                    tracing::warn!("failed to clean rejected desired epoch: {error}");
                }
                match self
                    .spawn_replacement(old_effective_spec, old_revision.epoch, old_controller)
                    .await
                {
                    Ok(instance) => {
                        let pid = instance.pid().unwrap_or_default();
                        let forwarder = spawn_forwarder(
                            self.inner.clone(),
                            instance.state(),
                            old_revision.epoch,
                        );
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
        let instance = Instance::spawn(
            effective_spec,
            epoch,
            controller,
            self.inner.options.cancel_token.clone(),
        )
        .await?;
        if let Err(error) = instance.wait_ready().await {
            return match instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                Ok(()) => Err(error),
                Err(stop_error) => Err(Error::ApplyFailed(format!(
                    "{error}; failed to stop rejected replacement: {stop_error}"
                ))),
            };
        }
        Ok(instance)
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

    fn install_switched(&self, ctrl: &mut Ctrl, instance: Instance, prepared: PreparedLaunch) {
        let epoch = prepared.revision.epoch;
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

        let instance = match Instance::spawn(
            launch.effective_spec.clone(),
            epoch,
            launch.controller.clone(),
            self.inner.options.cancel_token.clone(),
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
            drop(full_staged);
            let new_stop = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await;
            if new_stop.is_ok() {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
            }
            let error = match new_stop {
                Ok(()) => error,
                Err(new_error) => Error::ApplyFailed(format!(
                    "old epoch stop failed: {error}; new bootstrap stop also failed: {new_error}"
                )),
            };
            self.publish_terminal_error(&error);
            return Err(error);
        }

        if let Err(error) = self.inner.store.commit_replace(full_staged, epoch).await {
            let new_stop = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await;
            if new_stop.is_ok() {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
            }
            let error = match new_stop {
                Ok(()) => error,
                Err(new_error) => Error::ApplyFailed(format!(
                    "full runtime commit failed: {error}; bootstrap stop also failed: {new_error}"
                )),
            };
            self.publish_terminal_error(&error);
            return Err(error);
        }

        let reconciled = tokio::time::timeout(self.inner.options.reconcile_timeout, async {
            match restoration.as_ref() {
                Some((patch, projection)) => {
                    self.patch_and_verify(instance.controller(), patch, projection)
                        .await
                }
                None => self.probe_health(instance.controller()).await,
            }
        })
        .await
        .unwrap_or(false);
        if reconciled {
            self.install_switched(ctrl, instance, launch);
            self.inner.store.cleanup_epoch(old_epoch).await?;
            return Ok(SwitchOutcome::Graceful);
        }

        let effective_spec = launch.effective_spec.clone();
        let controller = launch.controller.clone();
        if let Err(error) = instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            self.publish_terminal_error(&error);
            return Err(error);
        }
        let replacement = match self
            .spawn_replacement(effective_spec, epoch, controller)
            .await
        {
            Ok(replacement) => replacement,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.publish_terminal_error(&error);
                return Err(error);
            }
        };
        self.install_switched(ctrl, replacement, launch);
        self.inner.store.cleanup_epoch(old_epoch).await?;
        Ok(SwitchOutcome::Hard {
            reason: DegradeReason::PatchFailed,
        })
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
            ..
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
            assert!(!apply_epoch_state(&mut status, 8, stale));
            assert!(matches!(
                status.state,
                CoreState::Running { epoch: 9, pid: 90 }
            ));
        }
    }
}
