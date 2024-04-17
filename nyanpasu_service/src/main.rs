
pub mod envs;
mod commands;
mod ipc;
mod logging;
#[cfg(not(windows))]
fn main() {
    panic!("This program is only intended to run on Windows.");
}

#[cfg(windows)]
mod service;

#[cfg(windows)]
fn main() -> windows_service::Result<()> {
    service::main()
}
