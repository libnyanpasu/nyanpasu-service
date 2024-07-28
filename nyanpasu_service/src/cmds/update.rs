use nyanpasu_ipc::client::shortcuts;
use semver::Version;
use tokio::task::spawn_blocking;

use crate::consts::{APP_NAME, APP_VERSION};

use super::CommandError;

pub async fn update() -> Result<(), CommandError> {
    tracing::info!("Checking for updates...");
    let service_data_dir = crate::utils::dirs::service_data_dir();
    tracing::info!("Service data dir: {:?}", service_data_dir);
    tracing::info!("Client version: {}", APP_VERSION);
    let service_binary =
        service_data_dir.join(format!("{}{}", APP_NAME, std::env::consts::EXE_SUFFIX));
    if !service_binary.exists() {
        tracing::info!("Service binary not found, copying from current binary directly...");
        tokio::fs::copy(std::env::current_exe()?, &service_binary).await?;
        return Ok(());
    }
    let client_version = Version::parse(APP_VERSION).unwrap();
    tracing::info!("Get server version...");
    let client = shortcuts::Client::service_default();
    let status = client.status().await.ok();
    match status {
        None => {
            // server is stopped or not installed
            tracing::info!("Server is stopped or not installed, replacing the binary directly...");
            tokio::fs::copy(std::env::current_exe()?, &service_binary).await?;
        }
        Some(status) => {
            let server_version = Version::parse(&status.version).unwrap();
            if client_version > server_version {
                tracing::info!("Client version is newer than server version, prepare updating...");
                tracing::info!("Stopping the service before updating...");
                spawn_blocking(super::stop::stop).await??; // stop the service before updating
                tracing::info!("Copying the binary...");
                tokio::fs::copy(std::env::current_exe()?, &service_binary).await?;
                tracing::info!("Service binary updated, starting the service...");
                spawn_blocking(super::start::start).await??; // start the service after updating
            } else {
                tracing::info!("Client version is the same as server version, no need to update.");
            }
        }
    }
    Ok(())
}
