use std::{path::PathBuf, sync::OnceLock};

use clap::Args;
use tokio_util::sync::CancellationToken;
use tracing_attributes::instrument;

use crate::{
    consts::{ENV_USER_LIST, USER_LIST_SEPARATOR},
    server::consts::RuntimeInfos,
};

use super::CommandError;

#[derive(Args, Debug, Clone)]
pub struct ServerContext {
    /// nyanpasu config dir
    #[clap(long)]
    pub nyanpasu_config_dir: PathBuf,
    /// nyanpasu data dir
    #[clap(long)]
    pub nyanpasu_data_dir: PathBuf,
    /// The nyanpasu install directory, allowing to search the sidecar binary
    #[clap(long)]
    pub nyanpasu_app_dir: PathBuf,
    /// run as service
    #[clap(long, default_value = "false")]
    pub service: bool,
}

pub static SHUTDOWN_TOKEN: OnceLock<CancellationToken> = OnceLock::new();

pub async fn server_inner(
    ctx: ServerContext,
    token: CancellationToken,
) -> Result<(), CommandError> {
    nyanpasu_utils::os::kill_by_pid_file(
        crate::utils::dirs::service_pid_file(),
        // TODO: use common name
        Some(&["mihomo", "clash"]),
    )
    .await?;
    tracing::info!("nyanpasu config dir: {:?}", ctx.nyanpasu_config_dir);
    tracing::info!("nyanpasu data dir: {:?}", ctx.nyanpasu_data_dir);

    // check dirs accessibility
    let _ = std::fs::metadata(&ctx.nyanpasu_config_dir)?;
    let _ = std::fs::metadata(&ctx.nyanpasu_data_dir)?;
    let _ = std::fs::metadata(&ctx.nyanpasu_app_dir)?;

    let service_data_dir = crate::utils::dirs::service_data_dir();
    let service_config_dir = crate::utils::dirs::service_config_dir();
    tracing::info!("suggested service data dir: {:?}", service_data_dir);
    tracing::info!("suggested service config dir: {:?}", service_config_dir);

    if !service_data_dir.exists() {
        std::fs::create_dir_all(&service_data_dir)?;
    }
    if !service_config_dir.exists() {
        std::fs::create_dir_all(&service_config_dir)?;
    }

    // Write current process id to file
    if let Err(e) = nyanpasu_utils::os::create_pid_file(
        crate::utils::dirs::service_pid_file(),
        std::process::id(),
    )
    .await
    {
        tracing::error!("create pid file error: {}", e);
    };

    crate::server::consts::RuntimeInfos::set_infos(RuntimeInfos {
        service_data_dir,
        service_config_dir,
        nyanpasu_config_dir: ctx.nyanpasu_config_dir,
        nyanpasu_data_dir: ctx.nyanpasu_data_dir,
        nyanpasu_app_dir: ctx.nyanpasu_app_dir,
    });

    #[cfg(windows)]
    let sids = std::env::var(ENV_USER_LIST)
        .ok()
        .map(|v| {
            v.split(USER_LIST_SEPARATOR)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            vec![
                nyanpasu_ipc::utils::acl::get_current_user_sid_string()
                    .expect("failed to get current user sid"),
            ]
        });
    #[cfg(not(windows))]
    let sids = ();

    crate::server::run(token, &sids.iter().map(|s| s.as_str()).collect::<Vec<_>>()).await?;
    Ok(())
}

#[instrument]
pub async fn server(ctx: ServerContext) -> Result<(), CommandError> {
    let token = CancellationToken::new();
    SHUTDOWN_TOKEN.set(token.clone()).unwrap();
    server_inner(ctx, token).await?;
    Ok(())
}
