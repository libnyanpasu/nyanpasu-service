use nyanpasu_ipc::{api::status::StatusResBody, client::shortcuts::Client};
use serde::Serialize;
use service_manager::{ServiceLabel, ServiceStatus, ServiceStatusCtx};

use crate::consts::{APP_NAME, APP_VERSION, SERVICE_LABEL};

use super::CommandError;

#[derive(Debug, clap::Args)]
pub struct StatusCommand {
    /// Output the result in JSON format
    #[clap(long, default_value = "false")]
    json: bool,

    /// Skip the service check
    #[clap(long, default_value = "false")]
    skip_service_check: bool,
}

#[derive(Debug)]
struct ServiceStatusWrapper(ServiceStatus);

impl Serialize for ServiceStatusWrapper {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            ServiceStatus::NotInstalled => serializer.serialize_str("not_installed"),
            ServiceStatus::Stopped(_) => serializer.serialize_str("stopped"),
            ServiceStatus::Running => serializer.serialize_str("running"),
        }
    }
}

impl PartialEq<ServiceStatus> for ServiceStatusWrapper {
    fn eq(&self, other: &ServiceStatus) -> bool {
        &self.0 == other
    }
}

impl From<ServiceStatus> for ServiceStatusWrapper {
    fn from(status: ServiceStatus) -> Self {
        Self(status)
    }
}

#[derive(Debug, Serialize)]
struct StatusInfo<'n> {
    name: &'n str,    // The client program name
    version: &'n str, // The client program version
    status: ServiceStatusWrapper,
    server: Option<StatusResBody<'n>>,
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
    let client = Client::service_default();
    let mut info = StatusInfo {
        name: APP_NAME,
        version: APP_VERSION,
        status: status.into(),
        server: None,
    };
    if info.status == ServiceStatus::Running {
        let server = match client.status().await {
            Ok(server) => Some(server),
            Err(e) => {
                tracing::debug!("failed to get server status: {}", e);
                info.status =
                    ServiceStatus::Stopped(Some(format!("failed to get server status: {}", e)))
                        .into();
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
