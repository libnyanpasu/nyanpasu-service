//! Cross-epoch orchestration: start/stop/switch and status publication.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio::sync::watch;

use crate::{
    config,
    error::Error,
    instance::Instance,
    spec::{ControllerMode, InstanceSpec, ManagerOptions, ResolvedController},
    state::{CoreState, CoreStatus, InstanceState, SpecSummary, StopReason, now_ms},
};

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
        _epoch: u64,
    ) -> Result<(InstanceSpec, ResolvedController, Option<clash_api::Host>), Error> {
        match &self.inner.options.controller_mode {
            ControllerMode::Passthrough => {
                let info = config::inspect(&spec.config_path).await?;
                let controller = config::resolve_controller(&info)?;
                Ok((spec.clone(), controller, None))
            }
            #[allow(unreachable_patterns)] // `Managed` arrives in M4
            _ => Err(Error::ControllerMissing),
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
        let epoch = self.next_epoch();
        let (effective_spec, controller, advertised) = self.prepare(&spec, epoch).await?;
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
                });
                ctrl.last_spec = Some(spec);
                Ok(())
            }
            Err(error) => {
                self.inner.publish_state(CoreState::Stopped {
                    reason: Some(StopReason::Error(error.to_string())),
                });
                Err(error)
            }
        }
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
        active.instance.stop().await?;
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
                active.instance.stop().await?;
            }
            self.inner.publish_state(CoreState::Stopped {
                reason: Some(StopReason::User),
            });
        }
        Ok(())
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
