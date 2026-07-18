mod common;

use nyanpasu_core_manager::{instance::Instance, spec::ResolvedController, state::InstanceState};
use tokio_util::sync::CancellationToken;

fn http_controller(port: u16) -> ResolvedController {
    ResolvedController {
        host: clash_api::Host::http(format!("127.0.0.1:{port}")).unwrap(),
        secret: None,
    }
}

#[tokio::test]
async fn start_confirms_via_version_probe() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(
        &dir,
        &format!("external-controller: 127.0.0.1:{port}\nx-fake-core:\n  ready-delay-ms: 300\n"),
    );
    let spec = common::mihomo_spec(&dir, config);

    let instance = Instance::spawn(spec, 1, http_controller(port), CancellationToken::new())
        .await
        .expect("spawn");
    let (recorder, log) = common::record_states(instance.state());

    instance.wait_ready().await.expect("becomes healthy");
    assert!(matches!(
        *instance.state().borrow(),
        InstanceState::Running { pid } if pid > 0
    ));
    assert_eq!(instance.epoch(), 1);

    instance.stop().await.expect("stop");
    recorder.abort();
    let states = log.lock().clone();
    // Starting must precede Running: the probe gates the Running transition.
    let starting = states
        .iter()
        .position(|s| matches!(s, InstanceState::Starting));
    let running = states
        .iter()
        .position(|s| matches!(s, InstanceState::Running { .. }));
    assert!(
        starting.unwrap() < running.unwrap(),
        "sequence was {states:?}"
    );
}

#[tokio::test]
async fn dropping_an_instance_kills_the_core() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(&dir, &format!("external-controller: 127.0.0.1:{port}\n"));
    let spec = common::mihomo_spec(&dir, config);

    let instance = Instance::spawn(spec, 1, http_controller(port), CancellationToken::new())
        .await
        .expect("spawn");
    instance.wait_ready().await.expect("healthy");
    drop(instance);
    common::wait_port_refused(port).await;
}

#[tokio::test]
async fn startup_timeout_kills_the_core() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(
        &dir,
        &format!("external-controller: 127.0.0.1:{port}\nx-fake-core:\n  never-ready: true\n"),
    );
    let mut spec = common::mihomo_spec(&dir, config);
    spec.options.startup_timeout = std::time::Duration::from_secs(1);

    let instance = Instance::spawn(spec, 1, http_controller(port), CancellationToken::new())
        .await
        .expect("spawn");
    let err = instance.wait_ready().await.expect_err("must time out");
    assert!(
        matches!(err, nyanpasu_core_manager::Error::StartupTimeout { .. }),
        "got {err}"
    );
    // The tree is killed: the fake core's controller port must be released.
    common::wait_port_refused(port).await;
}

#[tokio::test]
async fn immediate_exit_reports_stderr_tail() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(
        &dir,
        &format!(
            "external-controller: 127.0.0.1:{port}\nx-fake-core:\n  exit-code: 1\n  stderr-lines:\n    - \"boot marker failure\"\n"
        ),
    );
    let mut spec = common::mihomo_spec(&dir, config);
    // One retry, tiny backoff: GaveUp arrives well inside the startup deadline.
    spec.options.restart_policy =
        nyanpasu_utils::process::RestartPolicy::OnFailure { max_restarts: 1 };

    let instance = Instance::spawn(spec, 1, http_controller(port), CancellationToken::new())
        .await
        .expect("spawn succeeds; the failure is the exit");
    let err = instance.wait_ready().await.expect_err("must fail");
    match err {
        nyanpasu_core_manager::Error::StartupFailed { stderr_tail } => {
            assert!(
                stderr_tail.contains("boot marker failure"),
                "tail: {stderr_tail}"
            )
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[tokio::test]
async fn crash_recovers_through_restart_and_reprobe() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let state_file = dir.join("crash-state");
    let config = common::write_config(
        &dir,
        &format!(
            "external-controller: 127.0.0.1:{port}\nx-fake-core:\n  crash-after-ms: 400\n  crash-times: 1\n  state-file: {state_file}\n"
        ),
    );
    let spec = common::mihomo_spec(&dir, config);

    let instance = Instance::spawn(spec, 1, http_controller(port), CancellationToken::new())
        .await
        .expect("spawn");
    let (recorder, log) = common::record_states(instance.state());
    instance.wait_ready().await.expect("initially healthy");

    // First run crashes at ~400ms; the supervisor restarts; the re-probe
    // confirms the second (healthy) run.
    let mut rx = instance.state();
    common::wait_for_state(
        &mut rx,
        |s| matches!(s, InstanceState::Restarting { .. }),
        std::time::Duration::from_secs(5),
    )
    .await;
    common::wait_for_state(
        &mut rx,
        |s| matches!(s, InstanceState::Running { .. }),
        std::time::Duration::from_secs(10),
    )
    .await;

    instance.stop().await.expect("stop");
    recorder.abort();
    let states = log.lock().clone();
    let running_count = states
        .iter()
        .filter(|s| matches!(s, InstanceState::Running { .. }))
        .count();
    assert!(running_count >= 2, "sequence was {states:?}");
}

#[tokio::test]
async fn crash_loop_exhausts_the_budget() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let state_file = dir.join("crash-state");
    let config = common::write_config(
        &dir,
        &format!(
            "external-controller: 127.0.0.1:{port}\nx-fake-core:\n  crash-after-ms: 300\n  crash-times: 99\n  state-file: {state_file}\n"
        ),
    );
    let mut spec = common::mihomo_spec(&dir, config);
    spec.options.restart_policy =
        nyanpasu_utils::process::RestartPolicy::OnFailure { max_restarts: 1 };

    let instance = Instance::spawn(spec, 1, http_controller(port), CancellationToken::new())
        .await
        .expect("spawn");
    instance
        .wait_ready()
        .await
        .expect("first run is briefly healthy");

    let mut rx = instance.state();
    let terminal = common::wait_for_state(
        &mut rx,
        |s| s.is_terminal(),
        std::time::Duration::from_secs(15),
    )
    .await;
    assert!(
        matches!(
            &terminal,
            InstanceState::Stopped(nyanpasu_core_manager::StopReason::Error(msg))
                if msg.contains("restart budget exhausted")
        ),
        "terminal was {terminal:?}"
    );
}

#[tokio::test]
async fn user_stop_is_terminal_and_releases_the_port() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let config = common::write_config(&dir, &format!("external-controller: 127.0.0.1:{port}\n"));
    let spec = common::mihomo_spec(&dir, config);

    let instance = Instance::spawn(spec, 1, http_controller(port), CancellationToken::new())
        .await
        .expect("spawn");
    instance.wait_ready().await.expect("healthy");
    let mut rx = instance.state();
    instance.stop().await.expect("stop");

    let terminal = common::wait_for_state(
        &mut rx,
        |s| s.is_terminal(),
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(
        matches!(
            terminal,
            InstanceState::Stopped(nyanpasu_core_manager::StopReason::User)
        ),
        "terminal was {terminal:?}"
    );
    common::wait_port_refused(port).await;
}
