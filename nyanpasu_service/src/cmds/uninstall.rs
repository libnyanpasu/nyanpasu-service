use std::thread;

use service_manager::{
    ServiceLabel, ServiceStatus, ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};

use crate::consts::SERVICE_LABEL;

use super::CommandError;

pub fn uninstall() -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    let status = manager.status(ServiceStatusCtx {
        label: label.clone(),
    })?;
    match status {
        ServiceStatus::NotInstalled => {
            tracing::info!("service not installed, nothing to do");
            return Err(CommandError::ServiceNotInstalled);
        }
        ServiceStatus::Stopped(_) => {
            tracing::info!("service already stopped, so we can uninstall it directly");
            manager.uninstall(ServiceUninstallCtx {
                label: label.clone(),
            })?;
        }
        ServiceStatus::Running => {
            tracing::info!("service is running, we need to stop it first");
            manager.stop(ServiceStopCtx {
                label: label.clone(),
            })?;
            thread::sleep(std::time::Duration::from_secs(5)); // wait for the service to stop
            manager.uninstall(ServiceUninstallCtx {
                label: label.clone(),
            })?;
        }
    }
    tracing::info!("confirming service is uninstalled...");
    let status = manager.status(ServiceStatusCtx { label })?;
    if status != ServiceStatus::NotInstalled {
        return Err(CommandError::Other(anyhow::anyhow!(
            "service uninstall failed, status: {:?}",
            status
        )));
    }
    Ok(())
}
