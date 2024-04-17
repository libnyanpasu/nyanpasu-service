#[cfg(not(windows))]
fn main() {
    panic!("This program is only intended to run on Windows.");
}

/// install and start the service
#[cfg(windows)]
fn main() -> windows_service::Result<()> {
    use std::ffi::{OsStr, OsString};
    use windows_service::{
        service::{
            ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState,
            ServiceType,
        },
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)?;

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::START;
    if let Ok(service) = service_manager.open_service("clash_verge_service", service_access) {
        if let Ok(status) = service.query_status() {
            match status.current_state {
                ServiceState::StopPending
                | ServiceState::Stopped
                | ServiceState::PausePending
                | ServiceState::Paused => {
                    service.start(&Vec::<&OsStr>::new())?;
                }
                _ => {}
            };

            return Ok(());
        }
    }

    let service_binary_path = std::env::current_exe()
        .unwrap()
        .with_file_name("clash-verge-service.exe");

    if !service_binary_path.exists() {
        eprintln!("clash-verge-service.exe not found");
        std::process::exit(2);
    }

    let service_info = ServiceInfo {
        name: OsString::from("clash_verge_service"),
        display_name: OsString::from("Clash Verge Service"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: service_binary_path,
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // run as System
        account_password: None,
    };

    let start_access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START;
    let service = service_manager.create_service(&service_info, start_access)?;

    service.set_description("Clash Verge Service helps to launch clash core")?;
    service.start(&Vec::<&OsStr>::new())?;

    Ok(())
}
