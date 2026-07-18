mod common;

use nyanpasu_core_manager::{Error, kind::check_config};

#[tokio::test]
async fn check_config_passes_and_fails() {
    let (_guard, dir) = common::utf8_tempdir();
    let ok_config = common::write_config(&dir, "mixed-port: 7890\n");
    let spec = common::mihomo_spec(&dir, ok_config);
    check_config(&spec).await.expect("valid config passes");

    let bad_config = dir.join("bad.yaml");
    std::fs::write(
        &bad_config,
        "x-fake-core:\n  check-fail: port already in use\n",
    )
    .unwrap();
    let mut bad_spec = common::mihomo_spec(&dir, bad_config);
    bad_spec.config_path = dir.join("bad.yaml");
    let err = check_config(&bad_spec).await.expect_err("must fail");
    match err {
        Error::ConfigCheckFailed(msg) => assert_eq!(msg, "port already in use"),
        other => panic!("unexpected error: {other}"),
    }
}
