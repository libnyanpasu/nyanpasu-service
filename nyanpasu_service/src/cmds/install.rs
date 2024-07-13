use std::{env::current_exe, ffi::OsString};

use service_manager::{
    ServiceInstallCtx, ServiceLabel, ServiceManager, ServiceStatus, ServiceStatusCtx,
};

use crate::consts::SERVICE_LABEL;

#[derive(Debug, clap::Args)]
pub struct InstallCommand {
    #[clap(long)]
    user: String, // Should manual specify because the runner should be administrator/root
    #[clap(long)]
    nyanpasu_data_dir: String,
    #[clap(long)]
    nyanpasu_config_dir: String,
}

pub fn install(ctx: InstallCommand) -> Result<(), anyhow::Error> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = <dyn ServiceManager>::native()?;
    if !manager.available()? {
        anyhow::bail!("service manager not available");
    }
    // before we install the service, we need to check if the service is already installed
    if !matches!(
        manager.status(ServiceStatusCtx {
            label: label.clone(),
        })?,
        ServiceStatus::NotInstalled
    ) {
        anyhow::bail!("service already installed");
    }

    let service_data_dir = crate::utils::dirs::service_data_dir();
    let service_config_dir = crate::utils::dirs::service_config_dir();
    tracing::info!("suggested service data dir: {:?}", service_data_dir);
    tracing::info!("suggested service config dir: {:?}", service_config_dir);
    // create nyanpasu group to ensure share unix socket access
    #[cfg(not(windows))]
    {
        if !crate::utils::os::user::is_nyanpasu_group_exists()? {
            crate::utils::os::user::create_nyanpasu_group()?;
        }
        crate::utils::os::user::add_user_to_nyanpasu_group(&ctx.user)?;
    }
    let path = current_exe()?;
    let dir = path.parent().unwrap(); // It must be the data dir of the nyanpasu app
    tracing::info!("working dir: {:?}", dir);
    let mut envs = Vec::new();
    envs.push(("USER_LIST".to_string(), ctx.user));
    if let Ok(home) = std::env::var("HOME") {
        envs.push(("HOME".to_string(), home));
    }
    tracing::info!("installing service...");
    manager.install(ServiceInstallCtx {
        label,
        program: current_exe()?,
        args: vec![
            OsString::from("server"),
            OsString::from("--config-dir"),
            service_config_dir.into_os_string(),
            OsString::from("--data-dir"),
            service_data_dir.into_os_string(),
            OsString::from("--nyanpasu-data-dir"),
            ctx.nyanpasu_data_dir.into(),
            OsString::from("--nyanpasu-config-dir"),
            ctx.nyanpasu_config_dir.into(),
        ],
        contents: None,
        username: None, // because we just need to run the service as root
        working_directory: Some(dir.into()),
        environment: Some(envs),
        autostart: true,
    })?;
    tracing::info!("service installed");
    Ok(())
}
