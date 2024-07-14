#![feature(error_generic_member_access)]

mod cmds;
pub mod consts;
mod logging;
mod server;
mod utils;
use tracing::error;

#[tokio::main]
async fn main() {
    let result = cmds::process().await;
    match result {
        Ok(_) => {}
        Err(cmds::CommandError::PermissionDenied) => {
            eprintln!("Permission denied, please run as administrator or root");
            std::process::exit(consts::ExitCode::PermissionDenied as i32);
        }
        Err(cmds::CommandError::ServiceNotInstalled) => {
            eprintln!("Service not installed");
            std::process::exit(consts::ExitCode::ServiceNotInstalled as i32);
        }
        Err(cmds::CommandError::ServiceAlreadyInstalled) => {
            eprintln!("Service already installed");
            std::process::exit(consts::ExitCode::ServiceAlreadyInstalled as i32);
        }
        Err(cmds::CommandError::ServiceAlreadyStopped) => {
            eprintln!("Service already stopped");
            std::process::exit(consts::ExitCode::ServiceAlreadyStopped as i32);
        }
        Err(cmds::CommandError::ServiceAlreadyRunning) => {
            eprintln!("Service already running");
            std::process::exit(consts::ExitCode::ServiceAlreadyRunning as i32);
        }
        Err(e) => {
            error!("Error: {:#?}", e);
            std::process::exit(consts::ExitCode::Other as i32);
        }
    }
}
