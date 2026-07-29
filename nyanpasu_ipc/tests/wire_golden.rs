//! Golden snapshots of the IPC wire format.
//!
//! Every literal here is shipped protocol: the GUI decodes these exact shapes.
//! A diff in this file is a breaking change to clash-nyanpasu, never a test
//! that needs updating — if one of these fails, revert the change that broke
//! it or ship a protocol version.

use std::{
    borrow::Cow,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

use indexmap::IndexMap;
use nyanpasu_ipc::api::{
    R, RBuilder, ResponseCode,
    core::start::CoreStartReq,
    log::LogsResBody,
    network::set_dns::NetworkSetDnsReq,
    status::{CoreInfos, CoreState, RuntimeInfos, StatusResBody},
    ws::events::{Event, TraceLog},
};
use nyanpasu_utils::core::{ClashCoreType, CoreType};

/// Frozen so the envelope's `ts` does not make the goldens time-dependent.
const TS: i64 = 1_700_000_000;

fn ok_envelope<T>(data: T) -> R<'static, T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
{
    let mut envelope: R<'static, T> = RBuilder::success(data);
    envelope.ts = TS;
    envelope
}

fn error_envelope<T>(msg: &'static str) -> R<'static, T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
{
    let mut envelope: R<'static, T> = RBuilder::other_error(Cow::Borrowed(msg));
    envelope.ts = TS;
    envelope
}

#[test]
fn response_codes_and_their_messages_are_pinned() {
    assert_eq!(serde_json::to_string(&ResponseCode::Ok).unwrap(), r#""Ok""#);
    assert_eq!(
        serde_json::to_string(&ResponseCode::OtherError).unwrap(),
        r#""OtherError""#
    );
    assert_eq!(ResponseCode::Ok.msg(), "ok");
    assert_eq!(ResponseCode::OtherError.msg(), "other error");
}

#[test]
fn the_unit_response_envelope_is_pinned() {
    let mut built: R<'static, ()> = RBuilder::success(());
    built.ts = TS;
    assert_eq!(
        serde_json::to_string(&built).unwrap(),
        r#"{"code":"Ok","msg":"ok","data":null,"ts":1700000000}"#
    );
}

/// The three legacy core error strings are protocol, not diagnostics: the GUI
/// branches on them.
#[test]
fn the_legacy_core_error_envelopes_are_pinned() {
    for msg in [
        "core is already running",
        "core is already stopped",
        "core have not been started yet",
    ] {
        let envelope: R<'static, ()> = error_envelope(msg);
        assert_eq!(
            serde_json::to_string(&envelope).unwrap(),
            format!(r#"{{"code":"OtherError","msg":"{msg}","data":null,"ts":1700000000}}"#)
        );
    }
}

#[test]
fn the_core_start_request_is_pinned() {
    let request = CoreStartReq {
        core_type: Cow::Owned(CoreType::Clash(ClashCoreType::Mihomo)),
        config_file: Cow::Owned(PathBuf::from("/etc/nyanpasu/config.yaml")),
    };
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"core_type":{"clash":"mihomo"},"config_file":"/etc/nyanpasu/config.yaml"}"#
    );
}

#[test]
fn every_core_type_tag_is_pinned() {
    let cases = [
        (ClashCoreType::Mihomo, r#"{"clash":"mihomo"}"#),
        (ClashCoreType::MihomoAlpha, r#"{"clash":"mihomo-alpha"}"#),
        (ClashCoreType::ClashRust, r#"{"clash":"clash-rs"}"#),
        (
            ClashCoreType::ClashRustAlpha,
            r#"{"clash":"clash-rs-alpha"}"#,
        ),
        (ClashCoreType::ClashPremium, r#"{"clash":"clash"}"#),
        (ClashCoreType::Meow, r#"{"clash":"meow"}"#),
    ];
    for (core, expected) in cases {
        assert_eq!(
            serde_json::to_string(&CoreType::Clash(core)).unwrap(),
            expected
        );
    }
    assert_eq!(
        serde_json::to_string(&CoreType::SingBox).unwrap(),
        r#""singbox""#
    );
}

#[test]
fn the_set_dns_request_is_pinned() {
    let with_servers = NetworkSetDnsReq {
        dns_servers: Some(vec![
            Cow::Owned(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))),
            Cow::Owned(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))),
        ]),
    };
    assert_eq!(
        serde_json::to_string(&with_servers).unwrap(),
        r#"{"dns_servers":["1.1.1.1","8.8.8.8"]}"#
    );
    assert_eq!(
        serde_json::to_string(&NetworkSetDnsReq { dns_servers: None }).unwrap(),
        r#"{"dns_servers":null}"#
    );
}

#[test]
fn the_logs_response_is_pinned() {
    let body = LogsResBody {
        logs: vec![Cow::Borrowed("first"), Cow::Borrowed("second")],
    };
    assert_eq!(
        serde_json::to_string(&ok_envelope(body)).unwrap(),
        r#"{"code":"Ok","msg":"ok","data":{"logs":["first","second"]},"ts":1700000000}"#
    );
}

#[test]
fn the_status_response_is_pinned() {
    let body = StatusResBody {
        version: Cow::Borrowed("9.9.9-golden"),
        core_infos: CoreInfos {
            r#type: Some(CoreType::Clash(ClashCoreType::Mihomo)),
            state: CoreState::Running,
            state_changed_at: 42,
            config_path: Some(PathBuf::from("/etc/nyanpasu/config.yaml")),
        },
        runtime_infos: RuntimeInfos {
            service_data_dir: Cow::Owned(PathBuf::from("/srv/data")),
            service_config_dir: Cow::Owned(PathBuf::from("/srv/config")),
            nyanpasu_config_dir: Cow::Owned(PathBuf::from("/home/config")),
            nyanpasu_data_dir: Cow::Owned(PathBuf::from("/home/data")),
        },
    };
    assert_eq!(
        serde_json::to_string(&ok_envelope(body)).unwrap(),
        concat!(
            r#"{"code":"Ok","msg":"ok","data":{"version":"9.9.9-golden","#,
            r#""core_infos":{"type":{"clash":"mihomo"},"state":"Running","#,
            r#""state_changed_at":42,"config_path":"/etc/nyanpasu/config.yaml"},"#,
            r#""runtime_infos":{"service_data_dir":"/srv/data","#,
            r#""service_config_dir":"/srv/config","#,
            r#""nyanpasu_config_dir":"/home/config","#,
            r#""nyanpasu_data_dir":"/home/data"}},"ts":1700000000}"#
        )
    );
}

#[test]
fn the_core_states_are_pinned() {
    assert_eq!(
        serde_json::to_string(&CoreState::Running).unwrap(),
        r#""Running""#
    );
    assert_eq!(
        serde_json::to_string(&CoreState::Stopped(None)).unwrap(),
        r#"{"Stopped":null}"#
    );
    assert_eq!(
        serde_json::to_string(&CoreState::Stopped(Some("boom".to_owned()))).unwrap(),
        r#"{"Stopped":"boom"}"#
    );
}

#[test]
fn the_ws_events_are_pinned() {
    let mut fields = IndexMap::new();
    fields.insert("epoch".to_owned(), serde_json::json!(1));
    let log = Event::new_log(TraceLog {
        timestamp: "2026-01-01T00:00:00Z".to_owned(),
        level: "INFO".to_owned(),
        message: "hello".to_owned(),
        target: "nyanpasu_service::core".to_owned(),
        fields,
    });
    assert_eq!(
        serde_json::to_string(&log).unwrap(),
        concat!(
            r#"{"Log":{"timestamp":"2026-01-01T00:00:00Z","level":"INFO","#,
            r#""message":"hello","target":"nyanpasu_service::core","#,
            r#""fields":{"epoch":1}}}"#
        )
    );
    assert_eq!(
        serde_json::to_string(&Event::new_core_state_changed(CoreState::Running)).unwrap(),
        r#"{"CoreStateChanged":"Running"}"#
    );
}

#[test]
fn the_error_envelope_decodes_back() {
    let encoded = serde_json::to_string(&error_envelope::<()>("core is already stopped")).unwrap();
    let decoded: R<'static, ()> = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.code, ResponseCode::OtherError);
    assert_eq!(decoded.msg, "core is already stopped");
    assert!(decoded.data.is_none());
    assert_eq!(decoded.ts, TS);
}
