use std::path::PathBuf;
use std::sync::OnceLock;

pub struct RuntimeInfos {
    pub service_data_dir: PathBuf,
    pub service_config_dir: PathBuf,
    pub nyanpasu_config_dir: PathBuf,
    pub nyanpasu_data_dir: PathBuf,
}
static INSTANCE: OnceLock<RuntimeInfos> = OnceLock::new();

impl RuntimeInfos {
    pub fn global() -> &'static RuntimeInfos {
        &INSTANCE.get().unwrap() // RUNTIME_INFOS should access in the server command, or it will panic
    }

    pub fn set_infos(runtime_infos: RuntimeInfos) {
        INSTANCE.set(runtime_infos);
    }
}