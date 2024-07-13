mod cmds;
pub mod consts;
mod logging;
mod utils;
use tracing::error;

#[tokio::main]
async fn main() {
    match cmds::parse() {
        Ok(_) => {}
        Err(cmds::CommandError::PermissionDenied) => {
            eprintln!("Permission denied, please run as administrator or root");
            std::process::exit(consts::ExitCode::PermissionDenied as i32);
        }
        Err(e) => {
            error!("Error: {}", e);
            std::process::exit(consts::ExitCode::Other as i32);
        }
    }
}
