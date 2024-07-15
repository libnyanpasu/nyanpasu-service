use crate::api::R;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, path::PathBuf};

pub const STATUS_ENDPOINT: &str = "/status";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreState {
    Running,
    Stopped(Option<String>),
}

impl Default for CoreState {
    fn default() -> Self {
        Self::Stopped(None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreInfos {
    pub r#type: Option<nyanpasu_utils::core::CoreType>,
    pub state: CoreState,
    pub state_changed_at: i64,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfos<'a> {
    pub service_data_dir: Cow<'a, PathBuf>,
    pub service_config_dir: Cow<'a, PathBuf>,
    pub nyanpasu_config_dir: Cow<'a, PathBuf>,
    pub nyanpasu_data_dir: Cow<'a, PathBuf>,
}

// TODO: more health check fields
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StatusResBody<'a> {
    pub version: Cow<'a, str>,
    pub core_infos: CoreInfos,
    pub runtime_infos: RuntimeInfos<'a>,
}

pub type StatusRes<'a> = R<'a, StatusResBody<'a>>;
