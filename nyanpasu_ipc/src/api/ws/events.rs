use serde::{Deserialize, Serialize};

use crate::api::status::{CoreInfos, CoreState};

/// The event endpoint. There is no protocol negotiation and no version
/// parameter: the service binary ships with the program that consumes it, so
/// every connection speaks the same stream and any query string is ignored.
pub const EVENT_URI: &str = "/ws/events";

/// Which console stream a record arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "lowercase")]
pub enum CoreLogStream {
    Stdout,
    Stderr,
}

/// Normalized severity. Go's `fatal` and `panic` both terminate the process, so
/// the manager's parser collapses them into `Fatal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "lowercase")]
pub enum CoreLogLevel {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
}

/// Which core printed the record.
///
/// A deliberately separate type from [`nyanpasu_utils::core::CoreType`], for
/// the same reason [`crate::api::status::CoreControllerInfo`] is separate from
/// `clash_api::Host` — but here there is a second, sharper reason: `CoreType`
/// distinguishes alpha builds (`mihomo-alpha`, `clash-rs-alpha`) and a log
/// frame does not. The manager parses console output per *kind*, and its four
/// kinds are exactly these. Rendering a frame as `CoreType` would mean
/// inventing a distinction the frame never carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum CoreLogKind {
    #[serde(rename = "mihomo")]
    Mihomo,
    #[serde(rename = "clash-rs")]
    ClashRust,
    #[serde(rename = "clash")]
    ClashPremium,
    #[serde(rename = "meow")]
    Meow,
}

/// One structured field the core printed beside its message.
///
/// Not `clash_api::LogField`: clash-api is an internal dependency of the core
/// manager and must not enter the wire dependency tree — the same rule that
/// produced [`crate::api::status::CoreControllerInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CoreLogField {
    pub key: String,
    pub value: String,
}

/// One normalized core console record.
///
/// What is deliberately absent is the point of the type. The manager's frame
/// also carries `raw` (the whole logical record, up to 16 KiB), the timestamp's
/// original spelling, and whether that timestamp was inferred. `raw` overlaps
/// `message` almost entirely — for a record whose header did not parse they are
/// the same string — and the other two are parser internals. The JSONL archive
/// keeps all three for fidelity; this stream carries what a log panel renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CoreLogInfo {
    pub epoch: u64,
    pub kind: CoreLogKind,
    pub stream: CoreLogStream,
    pub level: CoreLogLevel,
    /// Unix milliseconds, stamped by the service when it observed the frame.
    /// Always present, which is what makes the stream sortable: a record whose
    /// header did not parse has no clock of its own.
    pub at: i64,
    /// Unix milliseconds as the core reported them, when it reported any. Clash
    /// premium and clash-rs do not print a full instant, so the manager infers
    /// theirs from the observation time — treat this as advisory and sort by
    /// [`Self::at`].
    pub timestamp_ms: Option<i64>,
    /// Clash premium's `[Tag]` prefix, meow's tracing target, or clash-rs's
    /// `file:line`.
    pub target: Option<String>,
    pub message: String,
    /// Populated only where field boundaries are decidable, which today means
    /// mihomo's quoted logfmt.
    pub fields: Vec<CoreLogField>,
    /// Continuation lines were dropped after hitting a size limit.
    pub truncated: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum Event {
    /// The lossy state, kept exactly as it has always been: `Starting` and
    /// `Restarting` are reported as `Stopped(None)`, so a crash loop is
    /// indistinguishable from a stop. Kept because the GUI still consumes it,
    /// and emitted beside every [`Self::CoreStatusChanged`];
    /// [`Self::CoreStatusChanged`] is where the faithful view lives.
    CoreStateChanged(CoreState),
    /// The full status snapshot — the same [`CoreInfos`] `/status` returns,
    /// including the faithful `detail`. Sent to every connection: once when the
    /// socket opens, once after a dropped-event recovery, and on every manager
    /// transition. Push *is* snapshot: the payload is byte-identical to
    /// `/status`'s `core_infos`, so a client feeds it into the same state it
    /// already keeps for `/status`. Treat it as idempotent — a reconnect or a
    /// lag recovery can repeat one.
    CoreStatusChanged(CoreInfos),
    /// One core console record, pushed live. Carried on its own broadcast ring
    /// inside the service, because the two streams fail differently: losing a
    /// status frame costs a full snapshot resend, losing a log line costs
    /// nothing. Consequence, stated rather than hidden: there is **no ordering
    /// guarantee between this variant and the status variants**. They render in
    /// different panels and have no causal relationship.
    ///
    /// Nothing is replayed on connect. The authoritative history is the JSONL
    /// archive the manager writes, whose directory `/status` reports.
    CoreLog(CoreLogInfo),
}

impl Event {
    pub fn new_core_state_changed(state: CoreState) -> Self {
        Self::CoreStateChanged(state)
    }

    pub fn new_core_status_changed(infos: CoreInfos) -> Self {
        Self::CoreStatusChanged(infos)
    }

    pub fn new_core_log(log: CoreLogInfo) -> Self {
        Self::CoreLog(log)
    }
}
