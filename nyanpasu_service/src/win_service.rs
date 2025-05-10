#![allow(dead_code)]
use std::{ffi::OsString, io::Result, time::Duration};

use nyanpasu_utils::runtime::block_on;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
};

use crate::consts::SERVICE_LABEL;

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

pub fn run() -> Result<()> {
    service_dispatcher::start(SERVICE_LABEL, ffi_service_main)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

define_windows_service!(ffi_service_main, service_main);

pub fn service_main(args: Vec<OsString>) {
    if let Err(e) = run_service(args) {
        panic!("Error starting service: {:?}", e);
    }
}

struct ServiceHandleGuard(ServiceStatusHandle);
impl Drop for ServiceHandleGuard {
    fn drop(&mut self) {
        let _ = self.0.set_service_status(ServiceStatus {
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
            service_type: SERVICE_TYPE,
        });
    }
}

fn set_stop_pending(status_handle: &ServiceStatusHandle) -> windows_service::Result<()> {
    let next_status = ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::StopPending,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: Duration::from_secs(15),
        process_id: Some(std::process::id()),
        controls_accepted: ServiceControlAccept::empty(),
    };
    status_handle.set_service_status(next_status)?;
    Ok(())
}

pub fn run_service(_arguments: Vec<OsString>) -> windows_service::Result<()> {
    let shutdown_token = tokio_util::sync::CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop => {
                tracing::info!("Received stop event. shutting down...");
                shutdown_token_clone.cancel();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Preshutdown => {
                tracing::info!("Received shutdown event. shutting down...");
                shutdown_token_clone.cancel();
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    // Register system service event handler
    let status_handle = service_control_handler::register(SERVICE_LABEL, event_handler)?;

    let pid = std::process::id();
    let next_status = ServiceStatus {
        // Should match the one from system service registry
        service_type: SERVICE_TYPE,
        // The new state
        current_state: ServiceState::Running,
        // Accept stop events when running
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::PRESHUTDOWN,
        // Used to report an error when starting or stopping only, otherwise must be zero
        exit_code: ServiceExitCode::Win32(0),
        // Only used for pending states, otherwise must be zero
        checkpoint: 0,
        // Only used for pending states, otherwise must be zero
        wait_hint: Duration::default(),
        process_id: Some(pid),
    };

    // Tell the system that the service is running now
    status_handle.set_service_status(next_status)?;

    let guard = ServiceHandleGuard(status_handle);
    let handle = std::thread::spawn(move || {
        block_on(crate::handler());
    });

    // Wait for shutdown signal
    block_on(shutdown_token.cancelled());

    // Give the service 15 seconds to stop
    set_stop_pending(&status_handle)?;

    // cancel the server handle
    if let Some(token) = crate::cmds::SERVER_SHUTDOWN_TOKEN.get() {
        token.cancel();
    }
    handle.join().unwrap();

    tracing::info!("Service stopped.");

    // drop the guard to set the service status to stopped
    drop(guard);
    Ok(())
}
