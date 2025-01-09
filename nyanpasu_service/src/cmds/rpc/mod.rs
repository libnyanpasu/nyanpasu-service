use std::{borrow::Cow, net::IpAddr};

/// This module is a shortcut for client rpc calls.
/// It is useful for testing and debugging service rpc calls.
use clap::Subcommand;
use nyanpasu_ipc::{api::network::set_dns::NetworkSetDnsReq, client::shortcuts::Client};

fn core_type_parser(s: &str) -> Result<nyanpasu_utils::core::CoreType, String> {
    let mut s = s.to_string();
    unsafe {
        simd_json::serde::from_slice(s.as_bytes_mut())
            .map_err(|e| format!("Failed to parse core type: {}", e))
    }
}

#[derive(Debug, Subcommand)]
pub enum RpcCommand {
    /// Start specific core with the given config file
    StartCore {
        /// The core type to start
        #[clap(long)]
        #[arg(value_parser = core_type_parser)]
        core_type: nyanpasu_utils::core::CoreType,

        /// The path to the core config fileW
        #[clap(long)]
        config_file: std::path::PathBuf,
    },
    /// Stop the running core
    StopCore,
    /// Restart the running core
    RestartCore,
    /// Get the logs of the service
    InspectLogs,
    /// Set the dns servers
    SetDns { dns_servers: Option<Vec<IpAddr>> },
}

pub async fn rpc(commands: RpcCommand) -> Result<(), crate::cmds::CommandError> {
    // let client = Client::new().await?;
    match commands {
        RpcCommand::StartCore {
            core_type,
            config_file,
        } => {
            let client = Client::service_default();

            let payload = nyanpasu_ipc::api::core::start::CoreStartReq {
                core_type: Cow::Borrowed(&core_type),
                config_file: Cow::Borrowed(&config_file),
            };
            client
                .start_core(&payload)
                .await
                .map_err(|e| crate::cmds::CommandError::Other(e.into()))?;
        }
        RpcCommand::StopCore => {
            let client = Client::service_default();
            client
                .stop_core()
                .await
                .map_err(|e| crate::cmds::CommandError::Other(e.into()))?;
        }
        RpcCommand::RestartCore => {
            let client = Client::service_default();
            client
                .restart_core()
                .await
                .map_err(|e| crate::cmds::CommandError::Other(e.into()))?;
        }
        RpcCommand::InspectLogs => {
            let client = Client::service_default();
            let logs = client
                .inspect_logs()
                .await
                .map_err(|e| crate::cmds::CommandError::Other(e.into()))?;
            for log in logs.logs {
                println!("{}", log.trim_matches('\n'));
            }
        }
        RpcCommand::SetDns { dns_servers } => {
            let client = Client::service_default();
            client
                .set_dns(&NetworkSetDnsReq {
                    dns_servers: dns_servers
                        .as_ref()
                        .map(|v| v.iter().map(Cow::Borrowed).collect()),
                })
                .await
                .map_err(|e| crate::cmds::CommandError::Other(e.into()))?;
        }
    }
    Ok(())
}
