pub(crate) fn get_name(placeholder: &str) -> String {
    #[cfg(windows)]
    let name = format!(r"\\.\pipe\{}", placeholder);
    #[cfg(unix)]
    let name = format!("/var/run/{}.sock", placeholder);
    name
}
