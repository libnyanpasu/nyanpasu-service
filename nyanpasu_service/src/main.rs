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

pub async fn handler() {
    crate::utils::deadlock_detection();
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

fn main() {
    register_ctrlc_handler();
    register_panic_hook();
    #[cfg(windows)]
    {
        let args = std::env::args_os().any(|arg| &arg == "--service");
        if args {
            crate::win_service::run().unwrap();
        } else {
            block_on(handler());
        }
    }
    #[cfg(not(windows))]
    {
        block_on(handler());
    }
}
