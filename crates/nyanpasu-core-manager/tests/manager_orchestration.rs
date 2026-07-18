mod common;

use std::time::Duration;

use nyanpasu_core_manager::{CoreState, Error, ManagerOptions, manager::CoreManager};

fn manager() -> CoreManager {
    CoreManager::new(ManagerOptions::default())
}

async fn wait_core_state(
    rx: &mut tokio::sync::watch::Receiver<nyanpasu_core_manager::CoreStatus>,
    pred: impl Fn(&CoreState) -> bool,
    timeout: Duration,
) -> CoreState {
    tokio::time::timeout(timeout, async {
        loop {
            let current = rx.borrow_and_update().state.clone();
            if pred(&current) {
                return current;
            }
            rx.changed().await.expect("status channel open");
        }
    })
    .await
    .expect("timed out waiting for core state")
}

#[tokio::test]
async fn start_publishes_running_and_rejects_double_start() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(&dir, &format!("external-controller: 127.0.0.1:{port}\n"));
    let spec = common::mihomo_spec(&dir, config);

    let manager = manager();
    let mut rx = manager.subscribe();
    assert!(matches!(
        manager.status().state,
        CoreState::Stopped { reason: None }
    ));

    manager.start(spec.clone()).await.expect("start");
    let state = wait_core_state(
        &mut rx,
        |s| matches!(s, CoreState::Running { .. }),
        Duration::from_secs(10),
    )
    .await;
    let CoreState::Running { epoch, pid } = state else {
        unreachable!()
    };
    assert!(epoch >= 1 && pid > 0);
    let status = manager.status();
    assert_eq!(
        status.spec.as_ref().map(|s| s.config_path.clone()),
        Some(spec.config_path.clone())
    );
    assert!(status.changed_at > 0);

    let err = manager.start(spec).await.expect_err("double start");
    assert!(matches!(err, Error::AlreadyRunning), "got {err}");

    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn stop_requires_a_running_core() {
    let manager = manager();
    assert!(matches!(manager.stop().await, Err(Error::NotStarted)));
}

#[tokio::test]
async fn failed_start_reports_error_and_publishes_stopped() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(
        &dir,
        &format!("external-controller: 127.0.0.1:{port}\nx-fake-core:\n  never-ready: true\n"),
    );
    let mut spec = common::mihomo_spec(&dir, config);
    spec.options.startup_timeout = Duration::from_secs(1);

    let manager = manager();
    let err = manager.start(spec).await.expect_err("must fail");
    assert!(matches!(err, Error::StartupTimeout { .. }), "got {err}");
    assert!(matches!(
        manager.status().state,
        CoreState::Stopped { reason: Some(_) }
    ));
}

#[tokio::test]
async fn missing_controller_is_rejected_strictly() {
    let (_guard, dir) = common::utf8_tempdir();
    let config = common::write_config(&dir, "mixed-port: 7890\n");
    let spec = common::mihomo_spec(&dir, config);
    let manager = manager();
    assert!(matches!(
        manager.start(spec).await,
        Err(Error::ControllerMissing)
    ));
}
