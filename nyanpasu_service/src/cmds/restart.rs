use service_manager::ServiceLabel;

use crate::consts::SERVICE_LABEL;

use super::CommandError;

pub fn restart() -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    let status = manager.status(service_manager::ServiceStatusCtx {
        label: label.clone(),
    })?;
    match status {
        service_manager::ServiceStatus::NotInstalled => {
            return Err(CommandError::ServiceNotInstalled);
        }
        service_manager::ServiceStatus::Stopped(_) => {
            tracing::info!("service already stopped, starting it...");
            manager.start(service_manager::ServiceStartCtx {
                label: label.clone(),
            })?;
        }
        service_manager::ServiceStatus::Running => {
            tracing::info!("service is running, stopping it...");
            manager.stop(service_manager::ServiceStopCtx {
                label: label.clone(),
            })?;
            std::thread::sleep(std::time::Duration::from_secs(3)); // wait for the service to stop
            manager.start(service_manager::ServiceStartCtx {
                label: label.clone(),
            })?;
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    // check if the service is running
    let status = manager.status(service_manager::ServiceStatusCtx {
        label: label.clone(),
    })?;
    if status != service_manager::ServiceStatus::Running {
        return Err(CommandError::Other(anyhow::anyhow!(
            "service restart failed, status: {:?}",
            status
        )));
    }
    Ok(())
}
