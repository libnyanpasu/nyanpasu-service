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
struct StatusInfo {
    status: ServiceStatusWrapper,
}

// TODO: impl the health check if service is running
// such as data dir, config dir, clash status.
pub fn status(ctx: StatusCommand) -> Result<(), CommandError> {
    let label: ServiceLabel = SERVICE_LABEL.parse()?;
    let manager = crate::utils::get_service_manager()?;
    let status = manager.status(ServiceStatusCtx {
        label: label.clone(),
    })?;
    let info = StatusInfo {
        status: status.into(),
    };
    if ctx.json {
        println!("{}", simd_json::serde::to_string_pretty(&info)?);
    } else {
        println!("{:?}", info);
    }
    Ok(())
}