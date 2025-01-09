#![feature(error_generic_member_access)]

mod cmds;
pub mod consts;
mod logging;
mod server;
mod utils;

#[cfg(windows)]
mod win_service;

use consts::ExitCode;
use nyanpasu_utils::runtime::block_on;
use tracing::error;
use utils::{os::register_ctrlc_handler, register_panic_hook};

pub async fn handler() -> ExitCode {
    crate::utils::deadlock_detection();
    let result = cmds::process().await;
    match result {
        Ok(_) => ExitCode::Normal,
        Err(cmds::CommandError::PermissionDenied) => {
            eprintln!("Permission denied, please run as administrator or root");
            ExitCode::PermissionDenied
        }
        Err(cmds::CommandError::ServiceNotInstalled) => {
            eprintln!("Service not installed");
            ExitCode::ServiceNotInstalled
        }
        Err(cmds::CommandError::ServiceAlreadyInstalled) => {
            eprintln!("Service already installed");
            ExitCode::ServiceAlreadyInstalled
        }
        Err(cmds::CommandError::ServiceAlreadyStopped) => {
            eprintln!("Service already stopped");
            ExitCode::ServiceAlreadyStopped
        }
        Err(cmds::CommandError::ServiceAlreadyRunning) => {
            eprintln!("Service already running");
            ExitCode::ServiceAlreadyRunning
        }
        Err(e) => {
            error!("Error: {:#?}", e);
            ExitCode::Other
        }
    }
}

fn main() -> ExitCode {
    let mut rx = register_ctrlc_handler();
    register_panic_hook();
    #[cfg(windows)]
    {
        let args = std::env::args_os().any(|arg| &arg == "--service");
        if args {
            crate::win_service::run().unwrap();
            return ExitCode::Normal;
        }
    }

    block_on(async {
        tokio::select! {
            biased;
            Some(_) = rx.recv() => {
                ExitCode::Normal
            }
            exit_code = handler() => {
                exit_code
            }
        }
    })
}
