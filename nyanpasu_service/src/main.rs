#![feature(error_generic_member_access)]

mod cmds;
pub mod consts;
mod logging;
mod server;
mod utils;

#[cfg(windows)]
mod win_service;

use nyanpasu_utils::runtime::block_on;
use tracing::error;
use utils::{os::register_ctrlc_handler, register_panic_hook};

pub async fn handler() -> Result<(), i32> {
    crate::utils::deadlock_detection();
    let result = cmds::process().await;
    match result {
        Ok(_) => Ok(()),
        Err(cmds::CommandError::PermissionDenied) => {
            eprintln!("Permission denied, please run as administrator or root");
            Err(consts::ExitCode::PermissionDenied as i32)
        }
        Err(cmds::CommandError::ServiceNotInstalled) => {
            eprintln!("Service not installed");
            Err(consts::ExitCode::ServiceNotInstalled as i32)
        }
        Err(cmds::CommandError::ServiceAlreadyInstalled) => {
            eprintln!("Service already installed");
            Err(consts::ExitCode::ServiceAlreadyInstalled as i32)
        }
        Err(cmds::CommandError::ServiceAlreadyStopped) => {
            eprintln!("Service already stopped");
            Err(consts::ExitCode::ServiceAlreadyStopped as i32)
        }
        Err(cmds::CommandError::ServiceAlreadyRunning) => {
            eprintln!("Service already running");
            Err(consts::ExitCode::ServiceAlreadyRunning as i32)
        }
        Err(e) => {
            error!("Error: {:#?}", e);
            Err(consts::ExitCode::Other as i32)
        }
    }
}

fn main() -> Result<(), i32> {
    let mut rx = register_ctrlc_handler();
    register_panic_hook();
    #[cfg(windows)]
    {
        let args = std::env::args_os().any(|arg| &arg == "--service");
        if args {
            crate::win_service::run().unwrap();
            return Ok(());
        }
    }

    block_on(async {
        tokio::select! {
            biased;
            Some(_) = rx.recv() => {
                Ok(())
            }
            res = handler() => {
                res
            }
        }
    })
}
