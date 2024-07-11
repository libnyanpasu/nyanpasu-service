use std::path::PathBuf;

pub struct CoreInstanceBuilder {
    core_type: super::CoreType,
    path: PathBuf,
    config_path: PathBuf,
}

