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
