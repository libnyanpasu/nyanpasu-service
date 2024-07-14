use crate::api::R;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CORE_START_ENDPOINT: &str = "/core/start";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CoreReq {
    pub core_type: nyanpasu_utils::core::CoreType,
    pub config_file: PathBuf,
}

pub type StatusRes<'a> = R<'a, None>;