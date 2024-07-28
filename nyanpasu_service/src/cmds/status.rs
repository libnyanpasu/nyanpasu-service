use std::{borrow::Cow, time::Duration};

use super::CommandError;
use crate::consts::{APP_NAME, APP_VERSION, SERVICE_LABEL};
use nyanpasu_ipc::{client::shortcuts::Client, types::StatusInfo};
use service_manager::{ServiceLabel, ServiceStatus, ServiceStatusCtx};
use tokio::time::timeout;

#[derive(Debug, clap::Args)]
pub struct StatusCommand {
    /// Output the result in JSON format
    #[clap(long, default_value = "false")]
    json: bool,

    /// Skip the service check
    #[clap(long, default_value = "false")]
    skip_service_check: bool,
}

// TODO: impl the health check if service is running
// such as data dir, config dir, core status.
pub async fn status(ctx: StatusCommand) -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    let status = if ctx.skip_service_check {
        ServiceStatus::Running
    } else {
        manager.status(ServiceStatusCtx {
            label: label.clone(),
        })?
    };
    tracing::debug!("Note that the service original state is: {:?}", status);
    let client = Client::service_default();
    let mut info = StatusInfo {
        name: Cow::Borrowed(APP_NAME),
        version: Cow::Borrowed(APP_VERSION),
        status: match status {
            ServiceStatus::NotInstalled => nyanpasu_ipc::types::ServiceStatus::NotInstalled,
            ServiceStatus::Stopped(_) => nyanpasu_ipc::types::ServiceStatus::Stopped,
            ServiceStatus::Running => nyanpasu_ipc::types::ServiceStatus::Running,
        },
        server: None,
    };
    if info.status == nyanpasu_ipc::types::ServiceStatus::Running {
        let server = match timeout(Duration::from_secs(3), client.status()).await {
            Ok(Ok(server)) => Some(server),
            Ok(Err(e)) => {
                tracing::debug!("failed to get server status: {}", e);
                info.status = nyanpasu_ipc::types::ServiceStatus::Stopped;
                None
            }
            Err(e) => {
                tracing::debug!("get server status timeout: {}", e);
                info.status = nyanpasu_ipc::types::ServiceStatus::Stopped;
                None
            }
        };

        info = StatusInfo { server, ..info }
    }
    if ctx.json {
        println!("{}", simd_json::serde::to_string_pretty(&info)?);
    } else {
        println!("{:#?}", info);
    }
    Ok(())
}
