use std::path::PathBuf;

use clap::Args;
use nyanpasu_utils::io::unwrap_infallible;

use crate::server::consts::RuntimeInfos;

use super::CommandError;

#[derive(Args, Debug, Clone)]
pub struct ServerContext {
    #[clap(long)]
    pub nyanpasu_config_dir: String,
    #[clap(long)]
    pub nyanpasu_data_dir: String,
}

pub async fn server(ctx: ServerContext) -> Result<(), CommandError> {
    let nyanpasu_config_dir = unwrap_infallible(ctx.nyanpasu_config_dir.parse::<PathBuf>());
    let nyanpasu_data_dir = unwrap_infallible(ctx.nyanpasu_data_dir.parse::<PathBuf>());

    // check dirs accessibility
    let _ = std::fs::metadata(&nyanpasu_config_dir)?;
    let _ = std::fs::metadata(&nyanpasu_data_dir)?;

    let service_data_dir = crate::utils::dirs::service_data_dir();
    let service_config_dir = crate::utils::dirs::service_config_dir();

    if !service_data_dir.exists() {
        std::fs::create_dir_all(&service_data_dir)?;
    }
    if !service_config_dir.exists() {
        std::fs::create_dir_all(&service_config_dir)?;
    }

    crate::server::consts::RuntimeInfos::set_infos(RuntimeInfos {
        service_data_dir,
        service_config_dir,
        nyanpasu_config_dir,
        nyanpasu_data_dir,
    });
    crate::server::run().await?;
    Ok(())
}
