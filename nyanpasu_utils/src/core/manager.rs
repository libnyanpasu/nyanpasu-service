use parking_lot::{Mutex, RwLock};
use runas::Command as RunasCommand;
use core::error;
use std::{
    path::PathBuf,
    process::{Child, Command as StdCommand},
    rc::Rc,
};
use tokio::process::Command as TokioCommand;

use super::builder;
#[derive(Builder, Debug)]
#[builder(build_fn(validate = "Self::validate"))]
pub struct CoreInstance {
    core_type: super::CoreType,
    binary_path: PathBuf,
    app_dir: PathBuf,
    config_path: PathBuf,
    #[builder(default = "self.default_instance()", setter(skip))]
    instance: Mutex<Option<Child>>,
}

impl CoreInstanceBuilder {
    fn default_instance(&self) -> Mutex<Option<Child>> {
        Mutex::new(None)
    }

    fn validate(&self) -> Result<(), String> {
        match self.binary_path {
            Some(ref path) if !path.exists() => {
                return Err(format!("binary_path {:?} does not exist", path));
            }
            None => {
                return Err("binary_path is required".into());
            }
            _ => {}
        }

        match self.app_dir {
            Some(ref path) if !path.exists() => {
                return Err(format!("app_dir {:?} does not exist", path));
            }
            None => {
                return Err("app_dir is required".into());
            }
            _ => {}
        }

        match self.config_path {
            Some(ref path) if !path.exists() => {
                return Err(format!("config_path {:?} does not exist", path));
            }
            None => {
                return Err("config_path is required".into());
            }
            _ => {}
        }

        if self.core_type.is_none() {
            return Err("core_type is required".into());
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CoreInstanceError {
    #[error("Failed to start instance: {0}")]
    Io(#[from] std::io::Error),
    #[error("Cfg is not correct: {0}")]
    CfgFailed(String),
}

impl CoreInstance {
    pub async fn check_config(&self, config: Option<PathBuf>) -> Result<(), CoreInstanceError> {
        let config = config.as_ref().unwrap_or(&self.config_path).as_os_str().to_str();
        let output = TokioCommand::new(self.binary_path)
            .args(&["-t", "-d", self.app_dir.as_os_str().to_str().unwrap_or(), "-f", config])
            .output().await?;
        Ok(())
    }
}

/// clean-up the instance when the manager is dropped
impl Drop for CoreInstance {
    fn drop(&mut self) {
        let mut instance = self.instance.lock();
        if let Some(mut instance) = instance.take() {
            if let Err(err) = instance.kill() {
                tracing::error!("Failed to kill instance: {:?}", err);
            }
        }
    }
}
