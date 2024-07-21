use nyanpasu_ipc::{api::status::CoreState, utils::get_current_ts};
use nyanpasu_utils::core::{
        instance::{CoreInstance, CoreInstanceBuilder},
        CommandEvent, CoreType,
    };
use parking_lot::RwLock;
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio::spawn;
use tracing::instrument;

use super::consts;

struct CoreManager {
    instance: Arc<CoreInstance>,
    config_path: PathBuf,
}

type StateChangedAt = i64;

const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;

pub struct CoreManagerWrapper(Arc<RwLock<(Option<CoreManager>, StateChangedAt)>>);

impl CoreManagerWrapper {
    pub fn global() -> &'static CoreManagerWrapper {
        static INSTANCE: OnceLock<CoreManagerWrapper> = OnceLock::new();
        INSTANCE.get_or_init(|| CoreManagerWrapper(Arc::new(RwLock::new((None, 0)))))
    }

    pub fn state<'a>(&self) -> Cow<'a, CoreState> {
        let this = self.0.read();
        match this.0 {
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
        let this = self.0.read();
        match this.0 {
            None => nyanpasu_ipc::api::status::CoreInfos {
                r#type: None,
                state: nyanpasu_ipc::api::status::CoreState::Stopped(None),
                state_changed_at: this.1,
                config_path: None,
            },
            Some(ref manager) => nyanpasu_ipc::api::status::CoreInfos {
                r#type: Some(manager.instance.core_type.clone()),
                state: self.state().into_owned(),
                state_changed_at: this.1,
                config_path: Some(manager.config_path.clone()),
            },
        }
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
        let binary_path = app_dir.join(core_type.get_executable_name());
        let pid_path = crate::utils::dirs::service_core_pid_file();
        let instance = CoreInstanceBuilder::default()
            .core_type(core_type.clone())
            .app_dir(app_dir)
            .binary_path(binary_path)
            .config_path(config_path.clone())
            .pid_path(pid_path)
            .build()?;
        let instance = {
            let mut this = self.0.write();
            let instance = Arc::new(instance);
            this.0 = Some(CoreManager {
                instance: instance.clone(),
                config_path,
            });
            this.1 = get_current_ts();
            instance
        };

        // start the core instance
        let inner = self.0.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<anyhow::Result<()>>(1); // use mpsc channel just to avoid type moved error, though it never fails
        tokio::spawn(async move {
            match instance.run().await {
                Ok((_, mut rx)) => {
                    let mut err_buf: Vec<String> = Vec::with_capacity(6);
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
                                    {
                                        let mut this = inner.write();
                                        this.1 = get_current_ts();
                                    }
                                    break;
                                }
                                CommandEvent::Terminated(status) => {
                                    tracing::info!("core terminated with status: {:?}", status);
                                    {
                                        let mut this = inner.write();
                                        this.1 = get_current_ts();
                                    }
                                    if status.code != Some(0)
                                        || !matches!(status.signal, Some(SIGKILL) | Some(SIGTERM))
                                    {
                                        let err = anyhow::anyhow!(format!(
                                            "core terminated with status: {:?}\n{}",
                                            status,
                                            err_buf.join("\n")
                                        ));
                                        tracing::error!("{}\n{}", err, err_buf.join("\n"));
                                        let _ = tx.send(Err(err)).await;
                                    }
                                    break;
                                }
                                CommandEvent::DelayCheckpointPass => {
                                    tracing::debug!("delay checkpoint pass");
                                    {
                                        let mut this = inner.write();
                                        this.1 = get_current_ts();
                                    }
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

    pub fn stop(&self) -> Result<(), anyhow::Error> {
        let state = self.state();
        if matches!(state.as_ref(), CoreState::Stopped(_)) {
            anyhow::bail!("core is already stopped");
        }
        let this = self.0.read();
        let instance = this.0.as_ref().unwrap().instance.clone();
        drop(this);
        instance.kill()?;
        Ok(())
    }

    pub async fn restart(&self) -> Result<(), anyhow::Error> {
        {
            let this = self.0.read();
            if this.0.is_none() {
                anyhow::bail!("core have not been started yet");
            }
        }
        self.stop()?;
        let (core_type, config_path) = {
            let this = self.0.read();
            let manager = this.0.as_ref().unwrap();
            (
                manager.instance.core_type.clone(),
                manager.config_path.clone(),
            )
        };
        self.start(&core_type, &config_path).await
    }
}
