use serde::{Deserialize, Serialize};

use crate::api::status::StatusResBody;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    NotInstalled,
    Stopped,
    Running,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusInfo<'n> {
    pub name: &'n str,    // The client program name
    pub version: &'n str, // The client program version
    pub status: ServiceStatus,
    pub server: Option<StatusResBody<'n>>,
}
