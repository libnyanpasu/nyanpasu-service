use crate::api::status::StatusResBody;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    NotInstalled,
    Stopped,
    Running,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusInfo<'n> {
    pub name: Cow<'n, str>,    // The client program name
    pub version: Cow<'n, str>, // The client program version
    pub status: ServiceStatus,
    pub server: Option<StatusResBody<'n>>,
}
