use std::{path::PathBuf, sync::Arc};

use camino::{Utf8Path, Utf8PathBuf};
use nyanpasu_core_manager::{
    ControllerMode, CoreKind, CoreManager as Manager, CoreSpec, CoreState as ManagerCoreState,
    Error as ManagerError, InstanceOptions, InstanceSpec, LogFrame, LogStream, ManagerOptions,
};
use nyanpasu_ipc::api::{
    status::{CoreInfos, CoreState},
    ws::events::Event as WsEvent,
};
use nyanpasu_utils::core::{ClashCoreType, CoreType};
use parking_lot::RwLock;
use tokio::sync::broadcast::error::RecvError;
use tracing::instrument;

use super::{consts::RuntimeInfos, events::EventHub};

const CORE_LOG_TARGET: &str = "nyanpasu_service::core";

/// Legacy wire strings the GUI branches on. These are protocol, not
/// diagnostics: changing any of them is a breaking change to clash-nyanpasu.
pub(crate) const MSG_CORE_ALREADY_RUNNING: &str = "core is already running";
pub(crate) const MSG_CORE_ALREADY_STOPPED: &str = "core is already stopped";
pub(crate) const MSG_CORE_NOT_STARTED: &str = "core have not been started yet";

struct Inner {
    manager: Manager,
    /// Wire-type echo: the manager knows nothing about the alpha variants.
    requested_core: RwLock<Option<CoreType>>,
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
                requested_core: RwLock::new(None),
                control: tokio::sync::Mutex::new(ControlState { closing: false }),
            }),
        })
    }

    /// State → ws events and core logs → tracing.
    pub fn spawn_bridges(&self, hub: EventHub) {
        let mut states = self.inner.manager.subscribe();
        tokio::spawn(async move {
            let raw = states.borrow_and_update().clone();
            let mut last = map_core_state(&raw.state);
            while states.changed().await.is_ok() {
                let raw = states.borrow_and_update().clone();
                let next = map_core_state(&raw.state);
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

    pub async fn status(&self) -> CoreInfos {
        let status = self.inner.manager.status();
        CoreInfos {
            r#type: self.inner.requested_core.read().clone(),
            state: map_core_state(&status.state),
            state_changed_at: status.changed_at,
            config_path: status.spec.map(|spec| spec.config_path.into_std_path_buf()),
        }
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
        tracing::info!(
            core_type = %core_type,
            kind = %kind,
            working_dir = %working_dir,
            binary_path = %binary_path,
            config_path = %config_path,
            "Starting Core"
        );
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

/// The wire type carries no `PartialEq`, so equality is spelled out here.
fn same_ipc_state(previous: &CoreState, next: &CoreState) -> bool {
    match (previous, next) {
        (CoreState::Running, CoreState::Running) => true,
        (CoreState::Stopped(previous), CoreState::Stopped(next)) => previous == next,
        _ => false,
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
