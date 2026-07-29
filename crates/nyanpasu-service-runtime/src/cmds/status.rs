use std::{borrow::Cow, time::Duration};

use super::CommandError;
use crate::consts::{APP_NAME, APP_VERSION, SERVICE_LABEL};
use nyanpasu_ipc::{client::shortcuts::Client, types::StatusInfo};
use service_manager::{ServiceLabel, ServiceManager, ServiceStatus};
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
    let manager = crate::utils::get_service_manager()?;
    status_with(manager.as_ref(), ctx).await
}

pub async fn status_with(
    manager: &dyn ServiceManager,
    ctx: StatusCommand,
) -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let status = if ctx.skip_service_check {
        ServiceStatus::Running
    } else {
        crate::utils::service::status(manager, &label)?
    };
    tracing::debug!("Note that the service original state is: {:?}", status);
    let client = Client::service_default();
    let mut info = StatusInfo {
        name: Cow::Borrowed(APP_NAME),
        version: Cow::Borrowed(APP_VERSION),
        status: map_service_status(status),
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
        println!("{info:#?}");
    }
    Ok(())
}

fn map_service_status(status: ServiceStatus) -> nyanpasu_ipc::types::ServiceStatus {
    match status {
        ServiceStatus::NotInstalled => nyanpasu_ipc::types::ServiceStatus::NotInstalled,
        ServiceStatus::Stopped(_) => nyanpasu_ipc::types::ServiceStatus::Stopped,
        ServiceStatus::Running => nyanpasu_ipc::types::ServiceStatus::Running,
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use crate::cmds::test_support::{MockServiceManager, status_call};

    #[test]
    fn service_status_maps_onto_the_ipc_status() {
        assert_eq!(
            map_service_status(ServiceStatus::NotInstalled),
            nyanpasu_ipc::types::ServiceStatus::NotInstalled
        );
        assert_eq!(
            map_service_status(ServiceStatus::Stopped(None)),
            nyanpasu_ipc::types::ServiceStatus::Stopped
        );
        assert_eq!(
            map_service_status(ServiceStatus::Stopped(Some("details".into()))),
            nyanpasu_ipc::types::ServiceStatus::Stopped
        );
        assert_eq!(
            map_service_status(ServiceStatus::Running),
            nyanpasu_ipc::types::ServiceStatus::Running
        );
    }

    #[tokio::test]
    async fn status_reports_a_missing_service_without_probing_the_server() {
        let manager = MockServiceManager::with_statuses([ServiceStatus::NotInstalled]);

        status_with(
            &manager,
            StatusCommand {
                json: true,
                skip_service_check: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(manager.calls(), [status_call()]);
    }
}
