use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct StartBody {
    pub core_type: Option<String>,

    pub bin_path: String,

    pub config_dir: String,

    pub config_file: String,

    pub log_file: String,
}

#[derive(Deserialize, Serialize)]
pub struct JsonResponse<T: Serialize> {
    pub code: u64,
    pub msg: String,
    pub data: Option<T>,
}
