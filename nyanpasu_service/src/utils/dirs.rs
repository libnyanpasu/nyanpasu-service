use std::path::PathBuf;

use crate::consts;

const LOGS_DIR_NAME: &str = "logs";

pub fn service_logs_dir() -> PathBuf {
    nyanpasu_utils::dirs::suggest_service_data_dir(consts::APP_NAME).join(LOGS_DIR_NAME)
}

pub fn service_data_dir() -> PathBuf {
    nyanpasu_utils::dirs::suggest_service_data_dir(consts::APP_NAME)
}

pub fn service_config_dir() -> PathBuf {
    nyanpasu_utils::dirs::suggest_service_data_dir(consts::APP_NAME).join("config")
}
