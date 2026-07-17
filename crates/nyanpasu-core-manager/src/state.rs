//! Instance and manager state machines and the published status snapshot.

use camino::Utf8PathBuf;

use crate::kind::CoreKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceState {
    /// Spawned; the version health probe has not passed yet.
    Starting,
    Running { pid: u32 },
    /// Crashed; the supervisor is backing off, respawning, or re-probing.
    Restarting { attempt: u32 },
    Stopping,
    Stopped(StopReason),
}

impl InstanceState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The process exited cleanly (code 0); the supervisor does not restart it.
    Finished,
    User,
    Error(String),
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreState {
    Stopped { reason: Option<StopReason> },
    Starting { epoch: u64 },
    Running { epoch: u64, pid: u32 },
    Restarting { epoch: u64, attempt: u32 },
    /// A hard or graceful switch is in flight.
    Switching { from: Option<u64>, to: u64 },
    Stopping { epoch: u64 },
}

#[derive(Debug, Clone)]
pub struct SpecSummary {
    pub kind: CoreKind,
    pub config_path: Utf8PathBuf,
}

/// Snapshot published on the manager's watch channel.
#[derive(Debug, Clone)]
pub struct CoreStatus {
    pub state: CoreState,
    /// Unix milliseconds of the last state transition (feeds IPC `state_changed_at`).
    pub changed_at: i64,
    pub spec: Option<SpecSummary>,
    /// The managed controller endpoint, when `ControllerMode::Managed` is active.
    pub controller: Option<clash_api::Host>,
}

impl CoreStatus {
    pub(crate) fn initial() -> Self {
        Self {
            state: CoreState::Stopped { reason: None },
            changed_at: now_ms(),
            spec: None,
            controller: None,
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
