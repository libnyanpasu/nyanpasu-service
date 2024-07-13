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
