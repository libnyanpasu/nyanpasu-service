/// This module is a shortcut for client rpc calls.
/// It is useful for testing and debugging service rpc calls.
use clap::Subcommand;
use nyanpasu_ipc::client::shortcuts::Client;

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
                core_type,
                config_file,
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
        _ => unimplemented!(),
    }
    Ok(())
}
