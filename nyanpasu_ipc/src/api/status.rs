use crate::api::R;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const STATUS_ENDPOINT: &str = "/status";

pub struct StatusReq {}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum CoreState {
    Running,
    #[default]
    Stopped(Option<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreInfos {
    pub r#type: Option<nyanpasu_utils::core::CoreType>,
    pub state: CoreState,
    pub state_changed_at: i64,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfos {
    pub service_data_dir: PathBuf,
    pub service_config_dir: PathBuf,
    pub nyanpasu_config_dir: PathBuf,
    pub nyanpasu_data_dir: PathBuf,
}

// TODO: more health check fields
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StatusResBody<'a> {
    pub version: &'a str,
    pub core_infos: CoreInfos,
    pub runtime_infos: RuntimeInfos,
}

pub type StatusRes<'a> = R<'a, StatusResBody<'a>>;
