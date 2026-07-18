mod common;

use std::time::Duration;

use nyanpasu_core_manager::{ControllerMode, CoreState, ManagerOptions, manager::CoreManager};

fn unique_template() -> Option<String> {
    #[cfg(windows)]
    {
        Some(format!(
            r"\\.\pipe\nyanpasu-test-{}-{{epoch}}",
            std::process::id()
        ))
    }
    #[cfg(not(windows))]
    {
        None // unix default derives the socket under derived_dir (already unique)
    }
}

fn managed_manager(derived_dir: camino::Utf8PathBuf) -> CoreManager {
    CoreManager::new(ManagerOptions {
        controller_mode: ControllerMode::Managed {
            derived_dir,
            controller_template: unique_template(),
        },
        ..Default::default()
    })
}

#[tokio::test]
async fn managed_start_injects_the_epoch_endpoint_and_advertises_it() {
    let (_guard, dir) = common::utf8_tempdir();
    let derived_dir = dir.join("derived");
    // Stale artifacts from a "previous run" must be swept by CoreManager::new.
    std::fs::create_dir_all(&derived_dir).unwrap();
    std::fs::write(derived_dir.join("epoch-99.yaml"), "stale").unwrap();

    // No external-controller in the user config — Managed mode injects one.
    let config = common::write_config(&dir, "mixed-port: 0\n");
    let manager = managed_manager(derived_dir.clone());
    assert!(
        !derived_dir.join("epoch-99.yaml").exists(),
        "stale derived config swept on construction"
    );

    manager
        .start(common::mihomo_spec(&dir, config))
        .await
        .expect("managed start");
    let status = manager.status();
    assert!(matches!(status.state, CoreState::Running { .. }));
    let controller = status.controller.expect("advertised managed endpoint");
    let endpoint = format!("{controller:?}");
    assert!(
        endpoint.contains('1'),
        "endpoint should embed the epoch: {endpoint}"
    );
    assert!(derived_dir.join("epoch-1.yaml").exists());

    manager.shutdown().await.expect("shutdown");
    assert!(
        !derived_dir.join("epoch-1.yaml").exists(),
        "derived config removed after shutdown"
    );
    let _ = Duration::ZERO;
}

use nyanpasu_core_manager::SwitchOutcome;
use parking_lot::Mutex;
use std::sync::Arc;

#[tokio::test]
async fn graceful_switch_overlaps_and_restores_listeners() {
    let (_guard, dir) = common::utf8_tempdir();
    let derived_dir = dir.join("derived");
    let mixed = common::free_port();
    let patch_log_b = dir.join("patch-b.log");

    let config_a = common::write_config(&dir, &format!("mixed-port: {mixed}\n"));
    let config_b_path = dir.join("config-b.yaml");
    std::fs::write(
        &config_b_path,
        format!("mixed-port: {mixed}\nx-fake-core:\n  patch-log: {patch_log_b}\n"),
    )
    .unwrap();

    let manager = managed_manager(derived_dir);
    manager
        .start(common::mihomo_spec(&dir, config_a))
        .await
        .expect("start A");
    tokio::net::TcpStream::connect(("127.0.0.1", mixed))
        .await
        .expect("A holds the mixed port");

    let mut rx = manager.subscribe();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_ = seen.clone();
    let recorder = tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            seen_.lock().push(rx.borrow_and_update().state.clone());
        }
    });

    let mut spec_b = common::mihomo_spec(&dir, config_b_path.clone());
    spec_b.config_path = config_b_path;
    let outcome = manager.switch(spec_b).await.expect("switch");
    assert_eq!(outcome, SwitchOutcome::Graceful);
    recorder.abort();

    // The user-visible overlap guarantee: never Stopped during the switch.
    let states = seen.lock().clone();
    assert!(
        states
            .iter()
            .any(|s| matches!(s, CoreState::Switching { .. })),
        "sequence was {states:?}"
    );
    assert!(
        !states
            .iter()
            .any(|s| matches!(s, CoreState::Stopped { .. })),
        "graceful switch must not publish Stopped: {states:?}"
    );

    // The new core received the original listener values via PATCH.
    let log = std::fs::read_to_string(&patch_log_b).expect("patch log");
    assert!(
        log.contains(&format!("\"mixed-port\":{mixed}")),
        "log: {log}"
    );
    // And rebound the port after the old core released it.
    tokio::net::TcpStream::connect(("127.0.0.1", mixed))
        .await
        .expect("B serves the mixed port after the switch");

    let CoreState::Running { epoch, .. } = manager.status().state else {
        panic!("not running after switch")
    };
    assert_eq!(epoch, 2);
    manager.shutdown().await.expect("shutdown");
}
