use service_manager::{ServiceLabel, ServiceStatus};

use crate::consts::SERVICE_LABEL;

use super::CommandError;

pub fn restart() -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    let status = crate::utils::service::status(manager.as_ref(), &label)?;
    match status {
        ServiceStatus::NotInstalled => {
            return Err(CommandError::ServiceNotInstalled);
        }
        ServiceStatus::Stopped(_) => {
            tracing::info!("service already stopped, starting it...");
            crate::utils::service::start(manager.as_ref(), &label)?;
        }
        ServiceStatus::Running => {
            tracing::info!("service is running, stopping it...");
            crate::utils::service::stop(manager.as_ref(), &label)?;
            std::thread::sleep(std::time::Duration::from_secs(3)); // wait for the service to stop
            crate::utils::service::start(manager.as_ref(), &label)?;
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    // check if the service is running
    let status = crate::utils::service::status(manager.as_ref(), &label)?;
    if status != ServiceStatus::Running {
        return Err(CommandError::Other(anyhow::anyhow!(
            "service restart failed, status: {:?}",
            status
        )));
    }
    Ok(())
}
