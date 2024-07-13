use constcat::concat;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_NAME: &str = env!("CARGO_PKG_NAME");
pub const SERVICE_LABEL: &str = concat!("moe.elaina.", APP_NAME);

pub enum ExitCode {
    PermissionDenied = 64,
    Other = 1,
}
