use crate::runtime::block_on;

use super::{utils::spawn_pipe_reader, ClashCoreType, CommandEvent, CoreType, TerminatedPayload};
use os_pipe::pipe;
use parking_lot::{Mutex, RwLock};
use shared_child::SharedChild;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{ffi::OsStr, path::PathBuf, process::Command as StdCommand, sync::Arc};
use tokio::{process::Command as TokioCommand, sync::mpsc::Receiver};

// TODO: migrate to https://github.com/tauri-apps/tauri-plugin-shell/blob/v2/src/commands.rs

#[derive(Builder, Debug)]
#[builder(build_fn(validate = "Self::validate"))]
pub struct CoreInstance {
    core_type: CoreType,
    binary_path: PathBuf,
    app_dir: PathBuf,
    config_path: PathBuf,
    #[builder(default = "self.default_instance()", setter(skip))]
    instance: Mutex<Option<Arc<SharedChild>>>,
    #[builder(default = "self.default_state()", setter(skip))]
    state: Arc<RwLock<CoreInstanceState>>,
}

#[derive(Debug, Clone, Default)]
pub enum CoreInstanceState {
    Running,
    #[default]
    Stopped,
}

impl CoreInstanceBuilder {
    fn default_instance(&self) -> Mutex<Option<Arc<SharedChild>>> {
        Mutex::new(None)
    }

    fn default_state(&self) -> Arc<RwLock<CoreInstanceState>> {
        Arc::new(RwLock::new(CoreInstanceState::default()))
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
    #[error("State check failed, already running or stopped")]
    StateCheckFailed,
}

impl CoreInstance {
    pub fn set_config(&mut self, config: PathBuf) {
        self.config_path = config;
    }

    pub async fn check_config(&self, config: Option<PathBuf>) -> Result<(), CoreInstanceError> {
        let config = config.as_ref().unwrap_or(&self.config_path).as_os_str();
        let output = TokioCommand::new(&self.binary_path)
            .args(vec![
                OsStr::new("-t"),
                OsStr::new("-d"),
                self.app_dir.as_os_str(),
                OsStr::new("-f"),
                config,
            ])
            .output()
            .await?;
        if !output.status.success() {
            let error = if !matches!(self.core_type, CoreType::Clash(ClashCoreType::ClashRust)) {
                super::utils::parse_check_output(
                    String::from_utf8_lossy(&output.stdout).to_string(),
                )
            } else {
                // pipe stdout and stderr to the same string
                format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            };
            return Err(CoreInstanceError::CfgFailed(error));
        }
        Ok(())
    }

    pub async fn run(
        &self,
    ) -> Result<(Arc<SharedChild>, Receiver<CommandEvent>), CoreInstanceError> {
        {
            let state = self.state.read();
            if matches!(*state, CoreInstanceState::Running) {
                return Err(CoreInstanceError::StateCheckFailed);
            }
        }
        let args = match self.core_type {
            CoreType::Clash(ref core_type) => {
                core_type.get_run_args(&self.app_dir, &self.config_path)
            }
            CoreType::SingBox => {
                unimplemented!("SingBox is not supported yet")
            }
        };
        let args = args.iter().map(|arg| arg.as_ref()).collect::<Vec<&OsStr>>();
        let (stdout_reader, stdout_writer) = pipe()?;
        let (stderr_reader, stderr_writer) = pipe()?;
        // let (stdin_reader, stdin_writer) = pipe()?;
        let (tx, rx) = tokio::sync::mpsc::channel::<CommandEvent>(1);
        let child = Arc::new({
            let mut command = StdCommand::new(&self.binary_path);
            command
                .args(args)
                .stderr(stderr_writer)
                .stdout(stdout_writer)
                // .stdin(stdin_reader)
                .current_dir(&self.app_dir);
            #[cfg(windows)]
            command.creation_flags(0x8000000); // CREATE_NO_WINDOW
            SharedChild::spawn(&mut command)?
        });
        let child_ = child.clone();
        let guard = Arc::new(RwLock::new(()));
        spawn_pipe_reader(
            tx.clone(),
            guard.clone(),
            stdout_reader,
            CommandEvent::Stdout,
            None,
        );
        spawn_pipe_reader(
            tx.clone(),
            guard.clone(),
            stderr_reader,
            CommandEvent::Stderr,
            None,
        );

        let state_ = self.state.clone();
        std::thread::spawn(move || {
            let _ = match child_.wait() {
                Ok(status) => {
                    guard.write();
                    block_on(async move {
                        {
                            let mut state = state_.write();
                            *state = CoreInstanceState::Stopped;
                        }
                        tx.send(CommandEvent::Terminated(TerminatedPayload {
                            code: status.code(),
                            #[cfg(windows)]
                            signal: None,
                            #[cfg(unix)]
                            signal: status.signal(),
                        }))
                        .await
                    });
                }
                Err(err) => {
                    guard.write();
                    block_on(async move { tx.send(CommandEvent::Error(err.to_string())).await });
                }
            };
        });

        // 等待 1.5 秒，若进程结束则表示失败
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
        if let Some(state) = child.try_wait()? {
            return Err(CoreInstanceError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to start instance: {:?}", state),
            )));
        }
        {
            let mut instance = self.instance.lock();
            let mut state = self.state.write();
            *instance = Some(child.clone());
            *state = CoreInstanceState::Running;
        }
        Ok((child, rx))
    }

    /// Kill the instance, it is a blocking operation
    pub fn kill(&self) -> Result<(), CoreInstanceError> {
        let instance = self.instance.lock();
        if instance.is_none() {
            return Err(CoreInstanceError::StateCheckFailed);
        }
        let instance = instance.as_ref().unwrap();
        instance.kill()?;
        loop {
            if let Some(state) = instance.try_wait()? {
                if state.success() {
                    break;
                } else {
                    return Err(CoreInstanceError::Io(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Failed to kill instance: {:?}", state),
                    )));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        {
            let mut instance = self.instance.lock();
            let mut state = self.state.write();
            *instance = None;
            *state = CoreInstanceState::Stopped;
        }
        Ok(())
    }
}

/// clean-up the instance when the manager is dropped
impl Drop for CoreInstance {
    fn drop(&mut self) {
        let mut instance = self.instance.lock();
        if let Some(instance) = instance.take() {
            if let Err(err) = instance.kill() {
                tracing::error!("Failed to kill instance: {:?}", err);
            }
        }
    }
}
