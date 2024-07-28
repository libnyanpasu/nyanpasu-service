use nyanpasu_ipc::{api::status::CoreState, utils::get_current_ts};
use nyanpasu_utils::core::{
    instance::{CoreInstance, CoreInstanceBuilder},
    CommandEvent, CoreType,
};
use parking_lot::Mutex;
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, OnceLock,
    },
};
use tokio::spawn;
use tracing::instrument;

use super::consts;

struct CoreManager {
    instance: Arc<CoreInstance>,
    config_path: PathBuf,
}

const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;

pub struct CoreManagerWrapper {
    instance: Arc<Mutex<Option<CoreManager>>>,
    state_changed_at: Arc<AtomicI64>,
    kill_flag: Arc<AtomicBool>,
}

impl CoreManagerWrapper {
    pub fn global() -> &'static CoreManagerWrapper {
        static INSTANCE: OnceLock<CoreManagerWrapper> = OnceLock::new();
        INSTANCE.get_or_init(|| CoreManagerWrapper {
            instance: Arc::new(Mutex::new(None)),
            state_changed_at: Arc::new(AtomicI64::new(0)),
            kill_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn state<'a>(&self) -> Cow<'a, CoreState> {
        let instance = self.instance.lock();
        match *instance {
            None => Cow::Borrowed(&CoreState::Stopped(None)),
            Some(ref manager) => Cow::Owned(match manager.instance.state() {
                nyanpasu_utils::core::instance::CoreInstanceState::Running => CoreState::Running,
                nyanpasu_utils::core::instance::CoreInstanceState::Stopped => {
                    CoreState::Stopped(None)
                }
            }),
        }
    }

    pub fn status(&self) -> nyanpasu_ipc::api::status::CoreInfos {
        let instance = self.instance.lock();
        let state_changed_at = self
            .state_changed_at
            .load(std::sync::atomic::Ordering::Relaxed);
        match *instance {
            None => nyanpasu_ipc::api::status::CoreInfos {
                r#type: None,
                state: nyanpasu_ipc::api::status::CoreState::Stopped(None),
                state_changed_at,
                config_path: None,
            },
            Some(ref manager) => nyanpasu_ipc::api::status::CoreInfos {
                r#type: Some(manager.instance.core_type.clone()),
                state: self.state().into_owned(),
                state_changed_at,
                config_path: Some(manager.config_path.clone()),
            },
        }
    }

    pub(self) fn recover_core(&'static self) {
        tracing::info!("Try to recover the core instance");
        std::thread::spawn(move || {
            nyanpasu_utils::runtime::block_on(async move {
                if let Err(e) = CoreManagerWrapper::global().restart().await {
                    tracing::error!("Failed to recover the core instance: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    CoreManagerWrapper::global().recover_core();
                }
            });
        });
    }

    #[instrument(skip(self))]
    pub async fn start(
        &self,
        core_type: &CoreType,
        config_path: &PathBuf,
    ) -> Result<(), anyhow::Error> {
        let state = self.state();
        if matches!(state.as_ref(), CoreState::Running) {
            anyhow::bail!("core is already running");
        }

        // check config_path
        let config_path = config_path.canonicalize()?;
        tokio::fs::metadata(&config_path).await?; // check if the file exists
        let infos = consts::RuntimeInfos::global();
        let app_dir = infos.nyanpasu_data_dir.clone();
        let binary_path = find_binary_path(core_type)?;
        let pid_path = crate::utils::dirs::service_core_pid_file();
        let instance = CoreInstanceBuilder::default()
            .core_type(core_type.clone())
            .app_dir(app_dir)
            .binary_path(binary_path)
            .config_path(config_path.clone())
            .pid_path(pid_path)
            .build()?;
        let instance = {
            let mut this = self.instance.lock();
            let instance = Arc::new(instance);
            *this = Some(CoreManager {
                instance: instance.clone(),
                config_path,
            });
            instance
        };
        self.state_changed_at
            .store(get_current_ts(), Ordering::Relaxed);

        // start the core instance
        let state_changed_at = self.state_changed_at.clone();
        let kill_flag = self.kill_flag.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<anyhow::Result<()>>(1); // use mpsc channel just to avoid type moved error, though it never fails
        tokio::spawn(async move {
            match instance.run().await {
                Ok((_, mut rx)) => {
                    let mut err_buf: Vec<String> = Vec::with_capacity(6);
                    kill_flag.store(false, Ordering::Relaxed); // reset the kill flag
                    loop {
                        if let Some(event) = rx.recv().await {
                            match event {
                                CommandEvent::Stdout(line) => {
                                    tracing::info!("{}", line);
                                }
                                CommandEvent::Stderr(line) => {
                                    tracing::error!("{}", line);
                                    err_buf.push(line);
                                }
                                CommandEvent::Error(e) => {
                                    tracing::error!("{}", e);
                                    let err =
                                        anyhow::anyhow!(format!("{}\n{}", e, err_buf.join("\n")));
                                    let _ = tx.send(Err(err)).await;
                                    state_changed_at.store(get_current_ts(), Ordering::Relaxed);
                                    break;
                                }
                                CommandEvent::Terminated(status) => {
                                    tracing::info!("core terminated with status: {:?}", status);
                                    state_changed_at.store(get_current_ts(), Ordering::Relaxed);
                                    if status.code != Some(0)
                                        || !matches!(status.signal, Some(SIGKILL) | Some(SIGTERM))
                                    {
                                        let err = anyhow::anyhow!(format!(
                                            "core terminated with status: {:?}\n{}",
                                            status,
                                            err_buf.join("\n")
                                        ));
                                        tracing::error!("{}\n{}", err, err_buf.join("\n"));
                                        if tx.send(Err(err)).await.is_err()
                                            && !kill_flag.load(Ordering::Relaxed)
                                        {
                                            CoreManagerWrapper::global().recover_core();
                                        }
                                    }
                                    break;
                                }
                                CommandEvent::DelayCheckpointPass => {
                                    tracing::debug!("delay checkpoint pass");
                                    state_changed_at.store(get_current_ts(), Ordering::Relaxed);
                                    tx.send(Ok(())).await.unwrap();
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    spawn(async move {
                        tx.send(Err(err.into())).await.unwrap();
                    });
                }
            }
        });
        rx.recv().await.unwrap()?;
        drop(rx);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), anyhow::Error> {
        let state = self.state();
        if matches!(state.as_ref(), CoreState::Stopped(_)) {
            anyhow::bail!("core is already stopped");
        }
        self.kill_flag.store(true, Ordering::Relaxed);
        let instance = {
            let instance = self.instance.lock();
            instance.as_ref().unwrap().instance.clone()
        };
        instance.kill().await?;
        Ok(())
    }

    pub async fn restart(&self) -> Result<(), anyhow::Error> {
        {
            let instance = self.instance.lock();
            if instance.is_none() {
                anyhow::bail!("core have not been started yet");
            }
        }
        let state = self.state();
        if matches!(state.as_ref(), CoreState::Running) {
            self.stop().await?;
        }
        let (core_type, config_path) = {
            let instance = self.instance.lock();
            let manager = instance.as_ref().unwrap();
            (
                manager.instance.core_type.clone(),
                manager.config_path.clone(),
            )
        };
        self.start(&core_type, &config_path).await
    }
}

// TODO: support system path search via a config or flag
/// Search the binary path of the core: Data Dir -> Sidecar Dir
pub fn find_binary_path(core_type: &CoreType) -> std::io::Result<PathBuf> {
    let infos = consts::RuntimeInfos::global();
    let data_dir = &infos.nyanpasu_data_dir;
    let binary_path = data_dir.join(core_type.get_executable_name());
    if binary_path.exists() {
        return Ok(binary_path);
    }
    let app_dir = &infos.nyanpasu_app_dir;
    let binary_path = app_dir.join(core_type.get_executable_name());
    if binary_path.exists() {
        return Ok(binary_path);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{} not found", core_type.get_executable_name()),
    ))
}
