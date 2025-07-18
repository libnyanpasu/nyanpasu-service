use std::{thread, time::Duration};

use anyhow::Context;
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
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let label_ = label.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    let manager = crate::utils::get_service_manager()?;
                    manager.stop(ServiceStopCtx { label: label_ })?;
                    anyhow::Ok(())
                });

                match tokio::time::timeout(Duration::from_secs(8), handle).await {
                    Ok(res) => res.context("failed to join service stop task").flatten(),
                    Err(e) => {
                        tracing::error!("service stop timed out: {:?}, trying to kill it", e);
                        let mut sys = sysinfo::System::new_all();
                        sys.refresh_all();
                        let pkg_name = env!("CARGO_PKG_NAME");
                        let current_pid = std::process::id();
                        tracing::info!("Try to find `{pkg_name}`...");
                        for (pid, process) in sys.processes() {
                            if let Some(path) = process.cwd()
                                && path.to_string_lossy().contains(pkg_name)
                                && pid.as_u32() != current_pid
                            {
                                tracing::info!("killing process: {:?}", pid);
                                process.kill();
                            }
                        }
                        Ok(())
                    }
                }
            })?;
            tracing::info!("service stopped");
        }
    }
    thread::sleep(std::time::Duration::from_secs(3));
    // check if the service is stopped
    let manager = crate::utils::get_service_manager()?;
    let status = manager.status(ServiceStatusCtx {
        label: label.clone(),
    })?;
    // macOS possibly returns ServiceStatus::NotInstalled
    if !matches!(
        status,
        ServiceStatus::Stopped(None) | ServiceStatus::NotInstalled
    ) {
        return Err(CommandError::Other(anyhow::anyhow!(
            "service stop failed, status: {:?}",
            status
        )));
    }
    Ok(())
}
