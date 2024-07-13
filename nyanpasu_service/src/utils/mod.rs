use service_manager::ServiceManager;

pub mod dirs;
pub mod os;

pub(crate) fn get_name(is_debug: bool) -> &'static str {
    if is_debug {
        "clash-nyanpasu"
    } else {
        "clash-nyanpasu-dev"
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
    if !manager.available()? {
        anyhow::bail!("service manager not available");
    }
    Ok(manager)
}
