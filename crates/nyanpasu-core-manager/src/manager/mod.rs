//! Cross-epoch orchestration: manager-owned artifacts, start/stop/switch, and
//! atomic status publication.

mod apply;
mod publish;
mod quarantine;
mod switching;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use serde_yaml_ng::Mapping;
use tokio::sync::watch;

use crate::{
    config::{self, ConfigSnapshot, diff},
    error::Error,
    instance::Instance,
    probe::ProbeHandle,
    runtime_store::{RuntimeConfigStore, RuntimeDirectoryLock, StagedRuntimeConfig},
    spec::{ControllerMode, InstanceSpec, ManagerOptions, ResolvedController},
    state::{ConfigRevision, CoreState, CoreStatus, InstanceStatus, StopReason},
};

use publish::{instance_core_state, spec_summary};
use quarantine::{reject_quarantine, sweep_orphans};

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
    restoration: Option<(Box<clash_api::ConfigPatch>, diff::RuntimeProjection)>,
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

    fn next_epoch(&self) -> u64 {
        self.inner.epoch.fetch_add(1, Ordering::Relaxed) + 1
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
}

async fn abort_and_await(mut forwarder: tokio::task::JoinHandle<()>) {
    forwarder.abort();
    let _ = (&mut forwarder).await;
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
