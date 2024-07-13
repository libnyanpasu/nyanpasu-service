use constcat::concat;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_NAME: &str = env!("CARGO_PKG_NAME");
pub const SERVICE_LABEL: &str = concat!("moe.elaina.", APP_NAME);

pub enum ExitCode {
    PermissionDenied = 64,
    ServiceNotInstalled = 100,
    ServiceAlreadyInstalled = 101,
    ServiceAlreadyStopped = 102,
    ServiceAlreadyRunning = 103,
    Other = 1,
}
