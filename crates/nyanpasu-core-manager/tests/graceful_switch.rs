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
