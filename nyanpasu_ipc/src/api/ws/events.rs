use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::api::status::{CoreInfos, CoreState};

pub const EVENT_URI: &str = "/ws/events";

/// Query parameter on [`EVENT_URI`] selecting the event protocol version.
///
/// Absent, empty, unknown or malformed all mean v1 — the two variants every
/// shipped client already decodes. Only the exact value [`EVENT_VERSION_V2`]
/// opts in. Fail-closed on purpose: a client that never asked for v2 must never
/// be handed a variant it cannot decode, because the GUI's tolerance of a single
/// decode error is unverified (report §7 R1).
pub const EVENT_VERSION_PARAM: &str = "v";

/// The one [`EVENT_VERSION_PARAM`] value that selects the v2 stream.
pub const EVENT_VERSION_V2: &str = "2";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TraceLog {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub target: String,
    pub fields: IndexMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum Event {
    Log(TraceLog),
    /// The lossy state, kept exactly as it has always been: `Starting` and
    /// `Restarting` are reported as `Stopped(None)`, so a crash loop is
    /// indistinguishable from a stop. v1 clients depend on this projection;
    /// [`Self::CoreStatusChanged`] is where the faithful view lives.
    CoreStateChanged(CoreState),
    /// The full status snapshot — the same [`CoreInfos`] `/status` returns,
    /// including the faithful `detail`. Sent only to connections that asked for
    /// v2 (`?v=2`), once when the socket opens, once after a dropped-event
    /// recovery, and on every manager transition. Push *is* snapshot: the
    /// payload is byte-identical to `/status`'s `core_infos`, so a client feeds
    /// it into the same state it already keeps for `/status`. Treat it as
    /// idempotent — a reconnect or a lag recovery can repeat one.
    CoreStatusChanged(CoreInfos),
}

impl Event {
    pub fn new_log(log: TraceLog) -> Self {
        Self::Log(log)
    }

    pub fn new_core_state_changed(state: CoreState) -> Self {
        Self::CoreStateChanged(state)
    }

    pub fn new_core_status_changed(infos: CoreInfos) -> Self {
        Self::CoreStatusChanged(infos)
    }

    /// Whether this event exists in protocol v1.
    ///
    /// An exhaustive match on purpose: a variant added later must fail to
    /// compile here rather than default into a v1 stream that cannot decode it.
    pub fn is_protocol_v1(&self) -> bool {
        match self {
            Self::Log(_) | Self::CoreStateChanged(_) => true,
            Self::CoreStatusChanged(_) => false,
        }
    }
}
