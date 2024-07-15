use anyhow::Context;
use service_manager::ServiceManager;

pub mod dirs;
pub mod os;

pub(crate) fn get_name(is_debug: bool) -> &'static str {
    if is_debug {
        "core-nyanpasu"
    } else {
        "core-nyanpasu-dev"
    }
}

pub fn must_check_elevation() -> bool {
    #[cfg(windows)]
    {
        use check_elevation::is_elevated;
        is_elevated().unwrap()
    }
    #[cfg(not(windows))]
    {
        use whoami::username;
        username() == "root"
    }
}

pub fn get_service_manager() -> Result<Box<dyn ServiceManager>, anyhow::Error> {
    let manager = <dyn ServiceManager>::native()?;
    if !manager.available().context(
        "service manager is not available, please make sure you are running as root or administrator",
    )? {
        anyhow::bail!("service manager not available");
    }
    Ok(manager)
}