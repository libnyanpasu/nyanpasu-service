use std::thread;

use service_manager::{ServiceLabel, ServiceStatus, ServiceStatusCtx, ServiceStopCtx};

use crate::consts::SERVICE_LABEL;

use super::CommandError;

pub fn stop() -> Result<(), CommandError> {
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
            tracing::info!("service already stopped");
            return Err(CommandError::ServiceAlreadyStopped);
        }
        ServiceStatus::Running => {
            tracing::info!("service is running, stopping it...");
            manager.stop(ServiceStopCtx {
                label: label.clone(),
            })?;
            tracing::info!("service stopped");
        }
    }
    thread::sleep(std::time::Duration::from_secs(3));
    // check if the service is stopped
    let status = manager.status(ServiceStatusCtx {
        label: label.clone(),
    })?;
    if status != ServiceStatus::Stopped(None) {
        return Err(CommandError::Other(anyhow::anyhow!(
            "service stop failed, status: {:?}",
            status
        )));
    }
    Ok(())
}
