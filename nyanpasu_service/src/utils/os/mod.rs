pub mod user;

pub fn register_ctrlc_handler() {
    ctrlc::set_handler(move || {
        eprintln!("Ctrl-C received, stopping service...");
        std::process::exit(0);
    })
    .expect("Error setting Ctrl-C handler");
}
