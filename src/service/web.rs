use super::data::*;
use anyhow::{bail, Context, Result};

use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::process::Child;
use std::process::Command;
use std::sync::{Arc, OnceLock};

#[derive(Debug, Default)]
pub struct ClashStatus {
    pub child: Option<Child>,

    pub info: Option<StartBody>,
}

impl ClashStatus {
    pub fn global() -> &'static Arc<Mutex<ClashStatus>> {
        static CLASHSTATUS: OnceLock<Arc<Mutex<ClashStatus>>> = OnceLock::new();

        CLASHSTATUS.get_or_init(|| Arc::new(Mutex::new(ClashStatus::default())))
    }
}

/// GET /version
/// 获取服务进程的版本
pub fn get_version() -> Result<HashMap<String, String>> {
    let version = env!("CARGO_PKG_VERSION");

    let mut map = HashMap::new();

    map.insert("service".into(), "Clash Verge Service".into());
    map.insert("version".into(), version.into());

    Ok(map)
}

/// POST /start_clash
/// 启动clash进程
pub fn start_clash(body: StartBody) -> Result<()> {
    // stop the old clash bin
    let _ = stop_clash();

    let body_cloned = body.clone();
    let core_type = body.core_type.unwrap_or("clash".into());

    let config_dir = body.config_dir.as_str();

    let config_file = body.config_file.as_str();

    let args = match core_type.as_str() {
        "mihomo" | "mihomo-alpha" => vec!["-m", "-d", config_dir, "-f", config_file],
        "clash-rs" => vec!["-d", config_dir, "-c", config_file],
        _ => vec!["-d", config_dir, "-f", config_file],
    };

    let log = File::create(body.log_file).context("failed to open log")?;
    let cmd = Command::new(body.bin_path).args(args).stdout(log).spawn()?;

    let mut arc = ClashStatus::global().lock();
    arc.child = Some(cmd);
    arc.info = Some(body_cloned);

    Ok(())
}

/// POST /stop_clash
/// 停止clash进程
pub fn stop_clash() -> Result<()> {
    let mut arc = ClashStatus::global().lock();

    arc.info = None;

    match arc.child.take() {
        Some(mut child) => child.kill().context("failed to kill clash"),
        None => bail!("clash not executed"),
    }
}

/// GET /get_clash
/// 获取clash当前执行信息
pub fn get_clash() -> Result<StartBody> {
    let arc = ClashStatus::global().lock();

    match arc.info.clone() {
        Some(info) => Ok(info),
        None => bail!("clash not executed"),
    }
}
