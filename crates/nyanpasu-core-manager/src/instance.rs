//! Single-epoch core instance: process supervision + health-probed state machine.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use nyanpasu_utils::process::{
    Command, EpochPidFile, OrphanReapOutcome, ProcessError, ProcessEvent, ReadinessProbe,
    Supervisor, SupervisorEvent, TerminatedPayload, reap_epoch_pid_file,
};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::Error,
    health::HealthCheck,
    kind::{self, MIHOMO_SAFE_PATHS_ENV_NAME},
    spec::{InstanceOptions, InstanceSpec, ResolvedController},
    state::{InstanceState, StopReason},
};

const STDERR_TAIL_LINES: usize = 32;

/// One epoch of a running core. The spec is immutable; a config change means a
/// new `Instance` with a new epoch (created by `CoreManager`).
pub struct Instance {
    epoch: u64,
    spec: Arc<InstanceSpec>,
    controller: ResolvedController,
    state_rx: watch::Receiver<InstanceState>,
    shared: Arc<Shared>,
}

struct Shared {
    state_tx: watch::Sender<InstanceState>,
    user_stop: AtomicBool,
    probe_timeout: AtomicBool,
    stderr_tail: parking_lot::Mutex<VecDeque<String>>,
    cancel: CancellationToken,
    supervisor: tokio::sync::Mutex<Option<Supervisor>>,
    monitor: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Shared {
    fn publish(&self, state: InstanceState) {
        let _ = self.state_tx.send(state);
    }

    fn tail(&self) -> String {
        let buf = self.stderr_tail.lock();
        buf.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

impl Instance {
    pub async fn spawn(
        spec: InstanceSpec,
        epoch: u64,
        controller: ResolvedController,
        parent: CancellationToken,
    ) -> Result<Instance, Error> {
        if tokio::fs::metadata(&spec.config_path).await.is_err() {
            return Err(Error::ConfigNotFound(spec.config_path.clone()));
        }
        if tokio::fs::metadata(&spec.core.binary_path).await.is_err() {
            return Err(Error::BinaryNotFound(spec.core.binary_path.clone()));
        }
        // Rejects kinds without a launch profile (`Meow`) before spawning.
        spec.core
            .kind
            .run_args(&spec.working_dir, &spec.config_path)?;

        let spec = Arc::new(spec);
        let (state_tx, state_rx) = watch::channel(InstanceState::Starting);
        let cancel = parent.child_token();
        let shared = Arc::new(Shared {
            state_tx,
            user_stop: AtomicBool::new(false),
            probe_timeout: AtomicBool::new(false),
            stderr_tail: parking_lot::Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)),
            cancel: cancel.clone(),
            supervisor: tokio::sync::Mutex::new(None),
            monitor: tokio::sync::Mutex::new(None),
        });

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let supervisor = Supervisor::builder({
            let spec = spec.clone();
            move || build_command(&spec, epoch)
        })
        .restart_policy(spec.options.restart_policy)
        .backoff(spec.options.backoff)
        .readiness(ReadinessProbe::Acknowledged)
        .cancel_token(cancel.clone())
        .on_event(move |event| {
            let _ = event_tx.send(event);
        })
        .on_process_event({
            let shared = shared.clone();
            move |event| match event {
                ProcessEvent::Stdout(line) => tracing::info!(target: "core", "{line}"),
                ProcessEvent::Stderr(line) => {
                    tracing::warn!(target: "core", "{line}");
                    let mut tail = shared.stderr_tail.lock();
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                ProcessEvent::Error(error) => {
                    tracing::warn!(target: "core", "output pump: {error}")
                }
                _ => {}
            }
        })
        .spawn()
        .await?;
        *shared.supervisor.lock().await = Some(supervisor);

        let monitor = tokio::spawn(monitor_loop(
            event_rx,
            shared.clone(),
            spec.options.clone(),
            controller.clone(),
        ));
        *shared.monitor.lock().await = Some(monitor);

        Ok(Instance {
            epoch,
            spec,
            controller,
            state_rx,
            shared,
        })
    }

    pub fn state(&self) -> watch::Receiver<InstanceState> {
        self.state_rx.clone()
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn spec(&self) -> &InstanceSpec {
        &self.spec
    }

    pub fn controller(&self) -> &ResolvedController {
        &self.controller
    }

    pub fn pid(&self) -> Option<u32> {
        match &*self.state_rx.borrow() {
            InstanceState::Running { pid } => Some(*pid),
            _ => None,
        }
    }

    /// Resolves once the initial start is confirmed (`Running`) or failed
    /// (`Stopped`). The startup timeout is enforced by the monitor task.
    pub async fn wait_ready(&self) -> Result<(), Error> {
        let mut rx = self.state_rx.clone();
        loop {
            let state = rx.borrow_and_update().clone();
            match state {
                InstanceState::Running { .. } => return Ok(()),
                InstanceState::Stopped(reason) => {
                    let text = match reason {
                        StopReason::Error(text) => text,
                        other => format!("stopped before ready: {other:?}"),
                    };
                    let stderr_tail = kind::error_summary(self.spec.core.kind, &text);
                    return Err(if self.shared.probe_timeout.load(Ordering::SeqCst) {
                        Error::StartupTimeout { stderr_tail }
                    } else {
                        Error::StartupFailed { stderr_tail }
                    });
                }
                _ => {}
            }
            if rx.changed().await.is_err() {
                return Err(Error::StartupFailed {
                    stderr_tail: self.shared.tail(),
                });
            }
        }
    }

    /// Compatibility wrapper for callers that do not provide a manager stop
    /// deadline.
    pub async fn stop(self) -> Result<(), Error> {
        self.stop_and_confirm_dead(std::time::Duration::from_secs(10))
            .await
    }

    /// Stops supervision and proves the epoch process is dead before returning.
    /// If normal supervisor termination is uncertain, the structured epoch pid
    /// record is the only authority used for the fallback kill.
    pub async fn stop_and_confirm_dead(self, timeout: std::time::Duration) -> Result<(), Error> {
        if self.state_rx.borrow().is_terminal() {
            self.reap_epoch_record_if_present().await?;
            return Ok(());
        }
        self.shared.user_stop.store(true, Ordering::SeqCst);
        self.shared.publish(InstanceState::Stopping);
        let supervisor = self.shared.supervisor.lock().await.take();
        let stop_result = match supervisor {
            Some(supervisor) => match tokio::time::timeout(timeout, supervisor.stop()).await {
                Ok(Ok(())) | Ok(Err(ProcessError::AlreadyExited)) => Ok(()),
                Ok(Err(error)) => Err(format!("supervisor stop failed: {error}")),
                Err(_) => Err(format!("supervisor stop exceeded {timeout:?}")),
            },
            None => Ok(()),
        };

        let mut monitor_confirmed = false;
        if let Some(mut monitor) = self.shared.monitor.lock().await.take() {
            match tokio::time::timeout(timeout, &mut monitor).await {
                Ok(_) => monitor_confirmed = true,
                Err(_) => {
                    monitor.abort();
                    let _ = monitor.await;
                }
            }
        }
        let terminal = self.state_rx.borrow().is_terminal();
        if stop_result.is_ok() && (monitor_confirmed || terminal) {
            self.reap_epoch_record_if_present().await?;
            return Ok(());
        }
        let stop_error = stop_result
            .err()
            .unwrap_or_else(|| "instance monitor did not confirm termination".to_owned());

        let Some(pid_file) = epoch_pid_path(&self.spec, self.epoch) else {
            return Err(Error::StopUnconfirmed(stop_error));
        };
        let runtime_dir = self.spec.config_path.parent().ok_or_else(|| {
            Error::StopUnconfirmed("runtime config has no parent directory".into())
        })?;
        let reaped = reap_epoch_pid_file(pid_file.as_std_path(), runtime_dir.as_std_path())
            .await
            .map_err(|error| {
                Error::StopUnconfirmed(format!(
                    "{stop_error}; epoch identity reaper failed: {error}"
                ))
            })?;
        if matches!(
            reaped,
            OrphanReapOutcome::AlreadyExited | OrphanReapOutcome::Killed
        ) || (matches!(reaped, OrphanReapOutcome::NotFound) && terminal)
        {
            if !terminal {
                self.shared
                    .publish(InstanceState::Stopped(StopReason::User));
            }
            return Ok(());
        }
        Err(Error::StopUnconfirmed(stop_error))
    }

    async fn reap_epoch_record_if_present(&self) -> Result<(), Error> {
        let Some(pid_file) = epoch_pid_path(&self.spec, self.epoch) else {
            return Ok(());
        };
        if !tokio::fs::try_exists(pid_file).await? {
            return Ok(());
        }
        let runtime_dir = self.spec.config_path.parent().ok_or_else(|| {
            Error::StopUnconfirmed("runtime config has no parent directory".into())
        })?;
        let _ = reap_epoch_pid_file(pid_file.as_std_path(), runtime_dir.as_std_path()).await?;
        Ok(())
    }
}

/// Dropping an `Instance` without `stop()` cancels supervision so the core
/// process tree is killed instead of orphaned (the monitor and supervisor
/// tasks hold `Arc<Shared>`, so drop alone would never reach them).
impl Drop for Instance {
    fn drop(&mut self) {
        self.shared.cancel.cancel();
    }
}

fn build_command(spec: &InstanceSpec, epoch: u64) -> Command {
    let args = spec
        .core
        .kind
        .run_args(&spec.working_dir, &spec.config_path)
        .expect("kind validated in Instance::spawn");
    let config_dir = spec
        .config_path
        .parent()
        .unwrap_or(spec.config_path.as_path());
    let mut command = Command::new(spec.core.binary_path.as_str())
        .args(args)
        .env(
            MIHOMO_SAFE_PATHS_ENV_NAME,
            kind::mihomo_safe_paths(&spec.working_dir, config_dir),
        )
        .current_dir(spec.working_dir.as_str());
    if let Some(pid_file) = &spec.pid_file {
        command = if epoch_pid_path(spec, epoch).is_some() {
            command.epoch_pid_file(EpochPidFile::new(
                pid_file.as_std_path(),
                epoch,
                spec.config_path.as_std_path(),
            ))
        } else {
            command.pid_file(pid_file.as_std_path())
        };
    }
    command
}

fn epoch_pid_path(spec: &InstanceSpec, epoch: u64) -> Option<&camino::Utf8Path> {
    let pid_file = spec.pid_file.as_deref()?;
    let expected_pid = format!("core-{epoch}.pid");
    let expected_config = format!("config-{epoch}.yaml");
    (pid_file.file_name() == Some(expected_pid.as_str())
        && spec.config_path.file_name() == Some(expected_config.as_str())
        && pid_file.parent() == spec.config_path.parent())
    .then_some(pid_file)
}

struct Probe {
    pid: u32,
    deadline: Instant,
}

async fn monitor_loop(
    mut events: mpsc::UnboundedReceiver<SupervisorEvent>,
    shared: Arc<Shared>,
    options: InstanceOptions,
    controller: ResolvedController,
) {
    let health = match HealthCheck::new(&controller) {
        Ok(health) => health,
        Err(error) => {
            shared.cancel.cancel();
            shared.publish(InstanceState::Stopped(StopReason::Error(format!(
                "failed to build the health-probe client: {error}"
            ))));
            return;
        }
    };

    let initial_deadline = Instant::now() + options.startup_timeout;
    let mut ever_ready = false;
    let mut timeout_fired = false;
    let mut probe: Option<Probe> = None;
    let mut last_exit: Option<TerminatedPayload> = None;

    loop {
        tokio::select! {
            maybe = events.recv() => match maybe {
                Some(SupervisorEvent::Started { pid }) => {
                    probe = Some(Probe {
                        pid,
                        deadline: Instant::now() + options.startup_timeout,
                    });
                }
                Some(SupervisorEvent::Restarting { attempt, .. }) => {
                    probe = None;
                    shared.publish(InstanceState::Restarting { attempt });
                }
                Some(SupervisorEvent::Exited(payload)) => last_exit = Some(payload),
                Some(SupervisorEvent::GaveUp) => {
                    shared.publish(InstanceState::Stopped(StopReason::Error(format!(
                        "core kept crashing; restart budget exhausted\n{}",
                        shared.tail()
                    ))));
                    return;
                }
                Some(SupervisorEvent::Stopped) => {
                    publish_terminal(&shared, last_exit.as_ref());
                    return;
                }
                Some(_) => {} // `Ready` (alive-after) only resets the restart budget
                None => {
                    publish_terminal(&shared, last_exit.as_ref());
                    return;
                }
            },
            _ = tokio::time::sleep_until(initial_deadline), if !ever_ready && !timeout_fired => {
                // Total limit for the initial start, crash-retries included.
                timeout_fired = true;
                probe = None;
                shared.probe_timeout.store(true, Ordering::SeqCst);
                shared.cancel.cancel(); // the supervisor kills the tree, then emits Stopped
            }
            _ = tokio::time::sleep(options.probe_interval), if probe.is_some() => {
                let deadline = probe.as_ref().expect("guarded").deadline;
                if health.probe_once().await {
                    let pid = probe.take().expect("guarded").pid;
                    let supervisor = shared.supervisor.lock().await;
                    let acknowledged = match supervisor.as_ref() {
                        Some(supervisor) => supervisor.acknowledge_ready(pid).await,
                        None => false,
                    };
                    drop(supervisor);
                    if acknowledged {
                        ever_ready = true;
                        shared.publish(InstanceState::Running { pid });
                    }
                } else if ever_ready && Instant::now() >= deadline {
                    // A post-crash respawn never became healthy again.
                    probe = None;
                    shared.probe_timeout.store(true, Ordering::SeqCst);
                    shared.cancel.cancel();
                }
            }
        }
    }
}

fn publish_terminal(shared: &Shared, last_exit: Option<&TerminatedPayload>) {
    let reason = if shared.user_stop.load(Ordering::SeqCst) {
        StopReason::User
    } else if shared.probe_timeout.load(Ordering::SeqCst) {
        StopReason::Error(format!("health probe timed out\n{}", shared.tail()))
    } else if last_exit.is_some_and(|payload| payload.code == Some(0)) {
        StopReason::Finished
    } else {
        StopReason::Error(format!(
            "core exited unexpectedly ({last_exit:?})\n{}",
            shared.tail()
        ))
    };
    let _ = shared.state_tx.send(InstanceState::Stopped(reason));
}
