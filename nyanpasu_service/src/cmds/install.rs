use std::{env::current_exe, ffi::OsString, path::PathBuf};

use service_manager::{ServiceInstallCtx, ServiceLabel, ServiceStatus, ServiceStatusCtx};

use crate::consts::{APP_NAME, SERVICE_LABEL};

use super::CommandError;

#[derive(Debug, clap::Args)]
pub struct InstallCommand {
    #[clap(long)]
    user: String, // Should manual specify because the runner should be administrator/root
    #[clap(long)]
    nyanpasu_data_dir: PathBuf,
    #[clap(long)]
    nyanpasu_config_dir: PathBuf,
}

pub fn install(ctx: InstallCommand) -> Result<(), CommandError> {
    tracing::info!("nyanpasu data dir: {:?}", ctx.nyanpasu_data_dir);
    tracing::info!("nyanpasu config dir: {:?}", ctx.nyanpasu_config_dir);
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    // before we install the service, we need to check if the service is already installed
    if !matches!(
        manager.status(ServiceStatusCtx {
            label: label.clone(),
        })?,
        ServiceStatus::NotInstalled
    ) {
        return Err(CommandError::ServiceAlreadyInstalled);
    }

    let service_data_dir = crate::utils::dirs::service_data_dir();
    let service_config_dir = crate::utils::dirs::service_config_dir();
    tracing::info!("suggested service data dir: {:?}", service_data_dir);
    tracing::info!("suggested service config dir: {:?}", service_config_dir);
    // copy nyanpasu service binary to the service data dir
    if !service_data_dir.exists() {
        std::fs::create_dir_all(&service_data_dir)?;
    }
    let service_binary =
        service_data_dir.join(format!("{}{}", APP_NAME, std::env::consts::EXE_SUFFIX));
    tracing::info!("copying service binary to: {:?}", service_binary);
    std::fs::copy(current_exe()?, &service_binary)?;

    // create nyanpasu group to ensure share unix socket access
    #[cfg(not(windows))]
    {
        tracing::info!("checking nyanpasu group exists...");
        if !crate::utils::os::user::is_nyanpasu_group_exists() {
            tracing::info!("nyanpasu group not exists, creating...");
            crate::utils::os::user::create_nyanpasu_group()?;
        }
        tracing::info!("checking whether user is in nyanpasu group...");
        if !crate::utils::os::user::is_user_in_nyanpasu_group(&ctx.user) {
            tracing::info!("adding user to nyanpasu group...");
            crate::utils::os::user::add_user_to_nyanpasu_group(&ctx.user)?;
        }
    }
    tracing::info!("working dir: {:?}", service_data_dir);
    let mut envs = Vec::new();
    envs.push(("USER_LIST".to_string(), ctx.user));
    if let Ok(home) = std::env::var("HOME") {
        envs.push(("HOME".to_string(), home));
    }
    tracing::info!("installing service...");
    manager.install(ServiceInstallCtx {
        label: label.clone(),
        program: service_binary,
        args: vec![
            OsString::from("server"),
            OsString::from("--nyanpasu-data-dir"),
            ctx.nyanpasu_data_dir.into(),
            OsString::from("--nyanpasu-config-dir"),
            ctx.nyanpasu_config_dir.into(),
            OsString::from("--service"),
        ],
        contents: None,
        username: None, // because we just need to run the service as root
        working_directory: Some(service_data_dir),
        environment: Some(envs),
        autostart: true,
    })?;
    // confirm the service is installed
    if matches!(
        manager.status(ServiceStatusCtx { label })?,
        ServiceStatus::NotInstalled
    ) {
        return Err(CommandError::Other(anyhow::anyhow!(
            "service install failed"
        )));
    }
    tracing::info!("service installed");
    Ok(())
}
