use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::api::status::CoreState;

pub const EVENT_URI: &str = "/ws/events";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceLog {
    pub timestamp: i64,
    pub level: String,
    pub message: String,
    fields: IndexMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Event {
    Log(TraceLog),
    CoreStateChanged(CoreState),
}

impl Event {
    pub fn new_log(log: TraceLog) -> Self {
        Self::Log(log)
    }

    pub fn new_core_state_changed(state: CoreState) -> Self {
        Self::CoreStateChanged(state)
    }
}
