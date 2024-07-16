use std::path::PathBuf;

use clap::Args;
use tracing_attributes::instrument;

use crate::server::consts::RuntimeInfos;

use super::CommandError;

#[derive(Args, Debug, Clone)]
pub struct ServerContext {
    #[clap(long)]
    pub nyanpasu_config_dir: PathBuf,
    #[clap(long)]
    pub nyanpasu_data_dir: PathBuf,
}

#[instrument]
pub async fn server(ctx: ServerContext) -> Result<(), CommandError> {
    crate::utils::deadlock_detection();
    tracing::info!("nyanpasu config dir: {:?}", ctx.nyanpasu_config_dir);
    tracing::info!("nyanpasu data dir: {:?}", ctx.nyanpasu_data_dir);

    // check dirs accessibility
    let _ = std::fs::metadata(&ctx.nyanpasu_config_dir)?;
    let _ = std::fs::metadata(&ctx.nyanpasu_data_dir)?;

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

    crate::server::consts::RuntimeInfos::set_infos(RuntimeInfos {
        service_data_dir,
        service_config_dir,
        nyanpasu_config_dir: ctx.nyanpasu_config_dir,
        nyanpasu_data_dir: ctx.nyanpasu_data_dir,
    });
    crate::server::run().await?;
    Ok(())
}
