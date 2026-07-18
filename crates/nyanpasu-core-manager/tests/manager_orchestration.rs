mod common;

use std::time::Duration;

use nyanpasu_core_manager::{CoreState, Error, ManagerOptions, StopReason, manager::CoreManager};

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

#[tokio::test]
async fn hard_switch_replaces_the_core_and_bumps_the_epoch() {
    let (_guard, dir) = common::utf8_tempdir();
    let port_a = common::free_port();
    let port_b = common::free_port();
    let config_a =
        common::write_config(&dir, &format!("external-controller: 127.0.0.1:{port_a}\n"));
    let config_b = dir.join("config-b.yaml");
    std::fs::write(
        &config_b,
        format!("external-controller: 127.0.0.1:{port_b}\n"),
    )
    .unwrap();

    let manager = manager();
    let mut rx = manager.subscribe();
    let mut spec_a = common::mihomo_spec(&dir, config_a);
    spec_a.options.startup_timeout = Duration::from_secs(15);
    manager.start(spec_a).await.expect("start");
    let CoreState::Running { epoch: first, .. } = manager.status().state else {
        panic!("not running")
    };

    let mut spec_b = common::mihomo_spec(&dir, config_b);
    spec_b.config_path = dir.join("config-b.yaml");
    spec_b.options.startup_timeout = Duration::from_secs(15);
    manager.switch(spec_b).await.expect("switch");

    let state = wait_core_state(
        &mut rx,
        |s| matches!(s, CoreState::Running { .. }),
        Duration::from_secs(10),
    )
    .await;
    let CoreState::Running { epoch: second, .. } = state else {
        unreachable!()
    };
    assert!(second > first, "epoch must increase: {first} -> {second}");
    common::wait_port_refused(port_a).await; // old core is dead

    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn restart_uses_the_last_spec_and_survives_stop() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(&dir, &format!("external-controller: 127.0.0.1:{port}\n"));

    let manager = manager();
    assert!(matches!(manager.restart().await, Err(Error::NotStarted)));

    let mut spec = common::mihomo_spec(&dir, config);
    spec.options.startup_timeout = Duration::from_secs(15);
    manager.start(spec).await.expect("start");
    manager.stop().await.expect("stop");
    // Legacy parity: restart after stop starts the remembered spec again.
    manager.restart().await.expect("restart after stop");
    assert!(matches!(manager.status().state, CoreState::Running { .. }));
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn switch_publishes_a_switching_window() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(&dir, &format!("external-controller: 127.0.0.1:{port}\n"));
    let mut spec = common::mihomo_spec(&dir, config);
    spec.options.startup_timeout = Duration::from_secs(15);

    let manager = manager();
    manager.start(spec.clone()).await.expect("start");

    let mut rx = manager.subscribe();
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen_ = seen.clone();
    let recorder = tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            seen_.lock().push(rx.borrow_and_update().state.clone());
        }
    });

    manager.restart().await.expect("restart");
    recorder.abort();
    let states = seen.lock().clone();
    assert!(
        states
            .iter()
            .any(|s| matches!(s, CoreState::Switching { .. })),
        "sequence was {states:?}"
    );
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn lifecycle_sequence_matches_legacy_contract() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let state_file = dir.join("crash-state");
    let config = common::write_config(
        &dir,
        &format!(
            "external-controller: 127.0.0.1:{port}\nx-fake-core:\n  crash-after-ms: 400\n  crash-times: 1\n  state-file: {state_file}\n"
        ),
    );

    let manager = manager();
    let mut rx = manager.subscribe();
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(vec![rx.borrow().state.clone()]));
    let seen_ = seen.clone();
    let recorder = tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            seen_.lock().push(rx.borrow_and_update().state.clone());
        }
    });

    manager
        .start(common::mihomo_spec(&dir, config))
        .await
        .expect("start");
    // Crash at ~400ms, recovery, then user stop.
    tokio::time::sleep(Duration::from_secs(3)).await;
    manager.stop().await.expect("stop");
    // Let the recorder drain the final Stopped notification before teardown — on the current-thread runtime the woken recorder task only runs once we yield.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !seen.lock().iter().any(|s| {
            matches!(
                s,
                CoreState::Stopped {
                    reason: Some(StopReason::User)
                }
            )
        }) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("terminal user stop never reached the recorder");
    recorder.abort();

    let states = seen.lock().clone();
    let position = |pred: &dyn Fn(&CoreState) -> bool| states.iter().position(pred);
    let starting = position(&|s| matches!(s, CoreState::Starting { .. })).expect("Starting");
    let running = position(&|s| matches!(s, CoreState::Running { .. })).expect("Running");
    let restarting = position(&|s| matches!(s, CoreState::Restarting { .. })).expect("Restarting");
    let stopped = states
        .iter()
        .rposition(|s| {
            matches!(
                s,
                CoreState::Stopped {
                    reason: Some(StopReason::User)
                }
            )
        })
        .expect("terminal user stop");
    assert!(
        starting < running && running < restarting && restarting < stopped,
        "sequence was {states:?}"
    );
    let running_after_restart = states[restarting..]
        .iter()
        .any(|s| matches!(s, CoreState::Running { .. }));
    assert!(
        running_after_restart,
        "recovery must re-confirm Running: {states:?}"
    );
}
