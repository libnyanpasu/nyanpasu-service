use std::io::Error as IoError;
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
use tracing_attributes::instrument;

pub mod user;

pub fn pid_exists(pid: u32) -> bool {
    let kind = RefreshKind::new().with_processes(ProcessRefreshKind::new());
    let system = System::new_with_specifics(kind);
    system.process(Pid::from_u32(pid)).is_some()
}

pub fn register_ctrlc_handler() {
    ctrlc::set_handler(move || {
        eprintln!("Ctrl-C received, stopping service...");
        std::process::exit(0);
    })
    .expect("Error setting Ctrl-C handler");
}

#[instrument]
pub async fn kill_service_if_pid_is_running() -> Result<(), IoError> {
    let path = super::dirs::service_pid_file();
    if path.exists() {
        let pid = std::fs::read_to_string(&path)?;
        let pid = pid.trim().parse::<i32>().unwrap_or(-1);
        if pid > 0 && pid_exists(pid as u32) {
            let list = kill_tree::tokio::kill_tree(pid as u32).await.map_err(|e| {
                IoError::new(std::io::ErrorKind::Other, format!("kill error: {:?}", e))
            })?;
            for p in list {
                if matches!(p, kill_tree::Output::Killed { .. }) {
                    tracing::info!("process is killed: {:?}", p);
                }
            }
        }
        std::fs::remove_file(&path)?;
    }
    Ok(())
}
