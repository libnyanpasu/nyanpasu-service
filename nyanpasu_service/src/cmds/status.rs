use nyanpasu_ipc::{api::status::StatusResBody, client::shortcuts::Client, SERVICE_PLACEHOLDER};
use serde::Serialize;
use service_manager::{ServiceLabel, ServiceStatus, ServiceStatusCtx};

use crate::consts::SERVICE_LABEL;

use super::CommandError;

#[derive(Debug, clap::Args)]
pub struct StatusCommand {
    #[clap(long, default_value = "false")]
    json: bool,
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

impl From<ServiceStatus> for ServiceStatusWrapper {
    fn from(status: ServiceStatus) -> Self {
        Self(status)
    }
}

#[derive(Debug, Serialize)]
struct StatusInfo<'n> {
    status: ServiceStatusWrapper,
    server: Option<StatusResBody<'n>>,
}

// TODO: impl the health check if service is running
// such as data dir, config dir, core status.
pub async fn status(ctx: StatusCommand) -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    let mut status = manager.status(ServiceStatusCtx {
        label: label.clone(),
    })?;
    let client = Client::new(SERVICE_PLACEHOLDER);
    let info = if status == ServiceStatus::Running {
        let server = match client.status().await {
            Ok(server) => Some(server),
            Err(e) => {
                tracing::debug!("failed to get server status: {}", e);
                status = ServiceStatus::Stopped(None);
                None
            }
        };
        StatusInfo {
            status: status.into(),
            server,
        }
    } else {
        StatusInfo {
            status: status.into(),
            server: None,
        }
    };
    if ctx.json {
        println!("{}", simd_json::serde::to_string_pretty(&info)?);
    } else {
        println!("{:#?}", info);
    }
    Ok(())
}
