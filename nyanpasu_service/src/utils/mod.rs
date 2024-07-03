pub(crate) fn get_name(is_debug: bool) -> &'static str {
    if is_debug {
        "clash-nyanpasu"
    } else {
        "clash-nyanpasu-dev"
    }
}