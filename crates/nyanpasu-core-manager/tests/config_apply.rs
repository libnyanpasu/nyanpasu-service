mod common;

use std::time::Duration;

use nyanpasu_core_manager::{
    ApplyOutcome, CoreManager, CoreState, Error, InstanceSpec, ManagerOptions, RevisionId,
};

async fn manager(dir: &camino::Utf8Path, control_timeout: Duration) -> CoreManager {
    CoreManager::new(ManagerOptions {
        runtime_dir: Some(dir.join("runtime")),
        control_timeout,
        reconcile_timeout: Duration::from_secs(5),
        ..ManagerOptions::default()
    })
    .await
    .expect("construct manager")
}

fn write_named(dir: &camino::Utf8Path, name: &str, body: &str) -> camino::Utf8PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write config");
    path
}

fn running(manager: &CoreManager) -> (u64, u32) {
    match manager.status().state {
        CoreState::Running { epoch, pid } => (epoch, pid),
        state => panic!("expected running, got {state:?}"),
    }
}

fn spec(dir: &camino::Utf8Path, path: camino::Utf8PathBuf) -> InstanceSpec {
    common::mihomo_spec(dir, path)
}

fn passthrough_yaml(port: u16, extra: &str) -> String {
    format!("external-controller: 127.0.0.1:{port}\nmode: rule\n{extra}")
}

#[tokio::test]
async fn apply_noop_keeps_the_current_revision_and_process() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let body = passthrough_yaml(port, "rules:\n  - MATCH,DIRECT\n");
    let first = write_named(&dir, "first.yaml", &body);
    let reordered = write_named(
        &dir,
        "same.yaml",
        &format!("rules: ['MATCH,DIRECT']\nexternal-controller: 127.0.0.1:{port}\nmode: rule\n"),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before_process = running(&manager);
    let before_revision = manager.status().revision.expect("revision");

    let outcome = manager
        .apply_config(spec(&dir, reordered), Some(before_revision.id()))
        .await
        .expect("apply noop");

    assert!(matches!(outcome, ApplyOutcome::Noop { .. }));
    assert_eq!(running(&manager), before_process);
    assert_eq!(manager.status().revision, Some(before_revision));
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn apply_patch_updates_the_revision_without_restarting() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let first = write_named(&dir, "first.yaml", &passthrough_yaml(port, ""));
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, "allow-lan: true\n"),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before = running(&manager);

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("patch");

    let ApplyOutcome::Patched { revision } = outcome else {
        panic!("expected Patched")
    };
    assert_eq!(running(&manager), before);
    assert_eq!(revision.generation, 2);
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn installed_apply_with_parent_sync_failure_reports_real_outcome() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let first = write_named(&dir, "first.yaml", &passthrough_yaml(port, ""));
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, "allow-lan: true\n"),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before = running(&manager);
    manager.inject_runtime_parent_sync_failure_once_for_test();

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("installed apply must reconcile despite sync uncertainty");

    let ApplyOutcome::DurabilityUncertain { outcome, warning } = outcome else {
        panic!("expected durability wrapper")
    };
    assert!(matches!(*outcome, ApplyOutcome::Patched { .. }));
    assert!(warning.contains("injected"), "{warning}");
    assert_eq!(running(&manager), before);
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn apply_reload_uses_put_without_restarting() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let first = write_named(
        &dir,
        "first.yaml",
        &passthrough_yaml(port, "rules:\n  - MATCH,DIRECT\n"),
    );
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, "rules:\n  - MATCH,REJECT\n"),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before = running(&manager);

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("reload");

    assert!(
        matches!(outcome, ApplyOutcome::Reloaded { .. }),
        "got {outcome:?}"
    );
    assert_eq!(running(&manager), before);
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn apply_unknown_change_restarts_to_the_committed_desired_config() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let first = write_named(
        &dir,
        "first.yaml",
        &passthrough_yaml(port, "x-setting: old\n"),
    );
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, "x-setting: new\n"),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before = running(&manager);

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("restart");

    assert!(matches!(outcome, ApplyOutcome::Restarted { .. }));
    let after = running(&manager);
    assert!(after.0 > before.0, "switch-class change gets a new epoch");
    assert_ne!(after.1, before.1);
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn failed_desired_restart_restores_and_restarts_the_old_revision() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let behavior = "x-fake-core:\n  patch-no-effect: true\n  fail-start-when-allow-lan: true\n";
    let first = write_named(&dir, "first.yaml", &passthrough_yaml(port, behavior));
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, &format!("allow-lan: true\n{behavior}")),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let old_revision = manager.status().revision.expect("old revision");

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("rollback succeeds");

    let ApplyOutcome::RolledBack {
        revision,
        failed_apply,
    } = outcome
    else {
        panic!("expected RolledBack")
    };
    assert_eq!(revision, old_revision);
    assert!(!failed_apply.is_empty());
    let restored: serde_yaml_ng::Mapping =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&revision.runtime_path).unwrap()).unwrap();
    assert_ne!(
        restored
            .get(serde_yaml_ng::Value::String("allow-lan".into()))
            .and_then(serde_yaml_ng::Value::as_bool),
        Some(true)
    );
    assert!(matches!(manager.status().state, CoreState::Running { .. }));
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn desired_and_rollback_commits_preserve_both_durability_warnings() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let behavior = "x-fake-core:\n  patch-no-effect: true\n  fail-start-when-allow-lan: true\n";
    let first = write_named(&dir, "first.yaml", &passthrough_yaml(port, behavior));
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, &format!("allow-lan: true\n{behavior}")),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    manager.inject_runtime_parent_sync_failure_once_for_test();
    manager.inject_runtime_parent_sync_failure_once_for_test();

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("rollback succeeds despite both durability warnings");

    let ApplyOutcome::DurabilityUncertain { outcome, .. } = outcome else {
        panic!("desired commit warning was lost")
    };
    let ApplyOutcome::DurabilityUncertain { outcome, .. } = *outcome else {
        panic!("rollback commit warning was lost")
    };
    assert!(matches!(*outcome, ApplyOutcome::RolledBack { .. }));
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn revision_conflict_has_zero_process_file_and_status_side_effects() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let first = write_named(&dir, "first.yaml", &passthrough_yaml(port, ""));
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, "allow-lan: true\n"),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before_status = manager.status();
    let before_runtime = std::fs::read(
        &before_status
            .revision
            .as_ref()
            .expect("revision")
            .runtime_path,
    )
    .unwrap();
    let mut stale = before_status.revision.as_ref().unwrap().id();
    stale.generation += 1;

    let error = manager
        .apply_config(spec(&dir, desired), Some(stale))
        .await
        .expect_err("CAS conflict");

    assert!(matches!(error, Error::RevisionConflict { .. }));
    assert_eq!(manager.status().revision, before_status.revision);
    assert_eq!(manager.status().state, before_status.state);
    assert_eq!(
        running(&manager),
        match before_status.state {
            CoreState::Running { epoch, pid } => (epoch, pid),
            _ => unreachable!(),
        }
    );
    assert_eq!(
        std::fs::read(manager.status().revision.unwrap().runtime_path).unwrap(),
        before_runtime
    );
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn patch_timeout_with_verified_effect_does_not_restart() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let behavior = "x-fake-core:\n  patch-delay-ms: 250\n";
    let first = write_named(&dir, "first.yaml", &passthrough_yaml(port, behavior));
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, &format!("allow-lan: true\n{behavior}")),
    );
    let manager = manager(&dir, Duration::from_millis(50)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before = running(&manager);

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("verified timed-out patch");

    assert!(matches!(outcome, ApplyOutcome::Patched { .. }));
    assert_eq!(
        running(&manager),
        before,
        "verified effect must avoid restart"
    );
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn patch_success_with_get_mismatch_restarts_desired() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let behavior = "x-fake-core:\n  patch-no-effect: true\n";
    let first = write_named(&dir, "first.yaml", &passthrough_yaml(port, behavior));
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, &format!("allow-lan: true\n{behavior}")),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager.start(spec(&dir, first)).await.expect("start");
    let before = running(&manager);

    let outcome = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect("restart desired");

    assert!(matches!(outcome, ApplyOutcome::Restarted { .. }));
    assert_ne!(running(&manager).1, before.1);
    manager.shutdown().await.expect("shutdown");
}

#[test]
fn revision_id_is_an_explicit_cas_token() {
    let token = RevisionId {
        epoch: 4,
        generation: 8,
        effective_hash: "hash".into(),
    };
    assert_eq!(token.epoch, 4);
}

#[tokio::test]
async fn source_mutation_during_staged_check_cannot_change_the_apply() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let behavior = "x-fake-core:\n  check-delay-ms: 300\n";
    let first = write_named(
        &dir,
        "first.yaml",
        &passthrough_yaml(port, &format!("x-setting: old\n{behavior}")),
    );
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, &format!("x-setting: desired\n{behavior}")),
    );
    let manager = std::sync::Arc::new(manager(&dir, Duration::from_secs(1)).await);
    manager
        .start(spec(&dir, first))
        .await
        .expect("start delayed-check config");

    let apply = {
        let manager = manager.clone();
        let apply_spec = spec(&dir, desired.clone());
        tokio::spawn(async move { manager.apply_config(apply_spec, None).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(
        &desired,
        passthrough_yaml(port, "x-setting: mutated\nx-fake-core:\n  exit-code: 91\n"),
    )
    .unwrap();

    let outcome = apply.await.unwrap().expect("snapshot apply");
    assert!(matches!(outcome, ApplyOutcome::Restarted { .. }));
    let runtime =
        std::fs::read_to_string(manager.status().revision.expect("revision").runtime_path).unwrap();
    assert!(runtime.contains("x-setting: desired"));
    assert!(!runtime.contains("exit-code"));
    assert!(matches!(manager.status().state, CoreState::Running { .. }));
    manager.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn desired_and_rollback_restart_failures_report_both_errors() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let counter = dir.join("launch-count.txt");
    let behavior = format!(
        "x-fake-core:\n  launch-count-file: '{}'\n  fail-after-launches: 1\n",
        counter.as_str().replace('\\', "/")
    );
    let first = write_named(
        &dir,
        "first.yaml",
        &passthrough_yaml(port, &format!("x-setting: old\n{behavior}")),
    );
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(
            port,
            &format!("x-setting: desired\n{behavior}  exit-code: 23\n"),
        ),
    );
    let manager = manager(&dir, Duration::from_secs(1)).await;
    manager
        .start(spec(&dir, first))
        .await
        .expect("initial start");

    let error = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect_err("both restart attempts fail");

    let Error::ApplyRollbackFailed { apply, rollback } = error else {
        panic!("expected combined error")
    };
    assert!(!apply.is_empty() && !rollback.is_empty());
    let CoreState::Stopped {
        reason: Some(reason),
    } = manager.status().state
    else {
        panic!("expected terminal error state")
    };
    let text = reason.to_string();
    assert!(text.contains("rollback also failed"), "{text}");
}

#[tokio::test]
async fn unconfirmed_replacement_stop_never_cleans_or_reuses_its_epoch() {
    let (_guard, dir) = common::utf8_tempdir();
    let runtime_dir = dir.join("runtime");
    let port = common::free_port();
    let counter = dir.join("launch-count.txt");
    let counter_path = counter.as_str().replace('\\', "/");
    let first = write_named(
        &dir,
        "first.yaml",
        &passthrough_yaml(
            port,
            &format!(
                "x-setting: old\nx-fake-core:\n  launch-count-file: '{counter_path}'\n  fail-after-launches: 99\n"
            ),
        ),
    );
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(
            port,
            &format!(
                "x-setting: desired\nx-fake-core:\n  launch-count-file: '{counter_path}'\n  fail-after-launches: 99\n  never-ready: true\n"
            ),
        ),
    );
    let manager = std::sync::Arc::new(
        CoreManager::new(ManagerOptions {
            runtime_dir: Some(runtime_dir.clone()),
            stop_timeout: Duration::from_secs(1),
            reconcile_timeout: Duration::from_secs(3),
            ..ManagerOptions::default()
        })
        .await
        .expect("construct manager"),
    );
    manager
        .start(spec(&dir, first.clone()))
        .await
        .expect("start");

    let apply = {
        let manager = manager.clone();
        let mut desired_spec = spec(&dir, desired.clone());
        desired_spec.options.startup_timeout = Duration::from_millis(300);
        tokio::spawn(async move { manager.apply_config(desired_spec, None).await })
    };
    let rejected_pid = runtime_dir.join("core-2.pid");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tokio::fs::try_exists(&rejected_pid).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("replacement pid record never appeared");
    let valid_pid_record = std::fs::read_to_string(&rejected_pid).unwrap();
    std::fs::write(&rejected_pid, "identity deliberately unavailable\n").unwrap();

    let result = apply.await.unwrap();
    assert!(
        matches!(result, Err(Error::StopUnconfirmed(_))),
        "unexpected compensation result: {result:?}"
    );
    assert!(runtime_dir.join("config-2.yaml").exists());
    assert!(rejected_pid.exists());
    assert_eq!(std::fs::read_to_string(counter).unwrap().trim(), "2");
    assert!(matches!(
        manager.status().state,
        CoreState::Stopped { reason: Some(_) }
    ));

    let CoreState::Stopped {
        reason: Some(reason),
    } = manager.status().state
    else {
        panic!("quarantine was not published")
    };
    let status_reason = reason.to_string();
    assert!(status_reason.contains("quarantin"), "{status_reason}");
    let start_error = manager
        .start(spec(&dir, first.clone()))
        .await
        .expect_err("quarantine must reject start");
    assert!(
        start_error.to_string().contains("quarantin"),
        "{start_error}"
    );
    let apply_error = manager
        .apply_config(spec(&dir, desired), None)
        .await
        .expect_err("quarantine must reject apply");
    assert!(
        apply_error.to_string().contains("quarantin"),
        "{apply_error}"
    );
    assert!(matches!(
        manager.switch(spec(&dir, first.clone())).await,
        Err(Error::ManagerQuarantined { .. })
    ));
    assert!(matches!(
        manager.restart().await,
        Err(Error::ManagerQuarantined { .. })
    ));

    std::fs::write(rejected_pid, valid_pid_record).unwrap();
    common::wait_port_refused(port).await;
    manager
        .recover_quarantine()
        .await
        .expect("identity-verified recovery");
    assert!(!runtime_dir.join("config-2.yaml").exists());
    manager
        .start(spec(&dir, first))
        .await
        .expect("start after quarantine recovery");
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn compensation_publishes_restart_before_replacement_is_ready() {
    let (_guard, dir) = common::utf8_tempdir();
    let port = common::free_port();
    let launches = dir.join("launch-count.txt");
    let launch_path = launches.as_str().replace('\\', "/");
    let behavior = format!(
        "x-fake-core:\n  patch-no-effect: true\n  ready-delay-ms: 700\n  launch-count-file: '{launch_path}'\n  fail-after-launches: 99\n"
    );
    let first = write_named(
        &dir,
        "first.yaml",
        &passthrough_yaml(port, &format!("allow-lan: false\n{behavior}")),
    );
    let desired = write_named(
        &dir,
        "desired.yaml",
        &passthrough_yaml(port, &format!("allow-lan: true\n{behavior}")),
    );
    let manager = std::sync::Arc::new(manager(&dir, Duration::from_secs(1)).await);
    manager.start(spec(&dir, first)).await.expect("start");
    let CoreState::Running { pid: old_pid, .. } = manager.status().state else {
        panic!("initial core not running")
    };

    let apply = {
        let manager = manager.clone();
        tokio::spawn(async move { manager.apply_config(spec(&dir, desired), None).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(&launches)
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                == Some(2)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("replacement never launched");
    assert!(
        matches!(manager.status().state, CoreState::Restarting { .. }),
        "dead pid {old_pid} remained published as {:?}",
        manager.status().state
    );
    assert!(matches!(
        apply.await.unwrap().unwrap(),
        ApplyOutcome::Restarted { .. }
    ));
    manager.shutdown().await.unwrap();
}
