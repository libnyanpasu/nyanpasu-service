use std::thread;

use service_manager::{ServiceLabel, ServiceStartCtx, ServiceStatus, ServiceStatusCtx};

use crate::consts::SERVICE_LABEL;

use super::CommandError;

pub fn start() -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    let status = manager.status(ServiceStatusCtx {
        label: label.clone(),
    })?;
    match status {
        ServiceStatus::NotInstalled => {
            return Err(CommandError::ServiceNotInstalled);
        }
        ServiceStatus::Stopped(_) => {
            manager.start(ServiceStartCtx {
                label: label.clone(),
            })?;
        }
        ServiceStatus::Running => {
            tracing::info!("service already running, nothing to do");
            return Err(CommandError::ServiceAlreadyRunning);
        }
    }
    thread::sleep(std::time::Duration::from_secs(3));
    // check if the service is running
    let status = manager.status(ServiceStatusCtx {
        label: label.clone(),
    })?;
    if status != ServiceStatus::Running {
        return Err(CommandError::Other(anyhow::anyhow!(
            "service start failed, status: {:?}",
            status
        )));
    }

    Ok(())
}
