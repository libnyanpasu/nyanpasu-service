use std::{process::Stdio, thread};

use service_manager::{ServiceLabel, ServiceStatus, ServiceStatusCtx, ServiceUninstallCtx};

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
            tracing::info!("Service is running, we need to stop it first");
            std::process::Command::new(std::env::current_exe()?)
                .arg("stop")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .status()
                .inspect_err(|e| tracing::error!("failed to stop service: {:?}", e))
                .map_err(CommandError::Io)?
                .exit_ok()
                .inspect_err(|e| tracing::error!("failed to stop service: {:?}", e))
                .map_err(|e| CommandError::Other(e.into()))?;
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
