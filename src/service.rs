#![cfg(windows)]

use anyhow::{bail, Context};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::process::Command;
use std::sync::Arc;
use std::{ffi::OsString, process::Child, time::Duration};
use tokio::runtime::Runtime;
use warp::Filter;
use windows_service::{
  define_windows_service,
  service::{ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType},
  service_control_handler::{self, ServiceControlHandlerResult},
  service_dispatcher, Result,
};

const SERVICE_NAME: &str = "clash_verge_service";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
const LISTEN_PORT: u16 = 33211;

#[derive(Debug, Default)]
struct ClashStatus {
  pub child: Option<Child>,

  pub info: Option<StartBody>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct StartBody {
  bin_path: String,

  config_dir: String,

  log_file: String,
}

#[derive(Deserialize, Serialize)]
struct JsonResponse {
  code: u64,
  msg: String,
  data: Option<StartBody>,
}

macro_rules! wrap_err {
  ($expr: expr) => {
    match $expr {
      Ok(_) => warp::reply::json(&JsonResponse {
        code: 0,
        msg: "ok".into(),
        data: None,
      }),
      Err(err) => warp::reply::json(&JsonResponse {
        code: 400,
        msg: format!("{err}"),
        data: None,
      }),
    }
  };
}

macro_rules! wrap_err_data {
  ($expr: expr) => {
    match $expr {
      Ok(data) => warp::reply::json(&JsonResponse {
        code: 0,
        msg: "ok".into(),
        data: Some(data),
      }),
      Err(err) => warp::reply::json(&JsonResponse {
        code: 400,
        msg: format!("{err}"),
        data: None,
      }),
    }
  };
}

/// The Service
pub async fn run_service() -> anyhow::Result<()> {
  let clash_status = Arc::new(Mutex::new(ClashStatus::default()));

  let clash_status_clone = clash_status.clone();
  let path_start_clash = warp::post()
    .and(warp::path("start_clash"))
    .and(warp::body::json())
    .map(move |body: StartBody| wrap_err!(start_clash(body, clash_status_clone.clone())));

  let clash_status_clone = clash_status.clone();
  let path_stop_clash = warp::post()
    .and(warp::path("stop_clash"))
    .map(move || wrap_err!(stop_clash(clash_status_clone.clone())));

  let clash_status_clone = clash_status.clone();
  let path_get_clash = warp::get()
    .and(warp::path("get_clash"))
    .map(move || wrap_err_data!(get_clash(clash_status_clone.clone())));

  let path_stop_service = warp::post()
    .and(warp::path("stop_service"))
    .map(|| wrap_err!(stop_service()));

  start_service()?;

  warp::serve(
    path_start_clash
      .or(path_stop_clash)
      .or(path_stop_service)
      .or(path_get_clash),
  )
  .run(([127, 0, 0, 1], LISTEN_PORT))
  .await;

  Ok(())
}

// 开启服务 设置服务状态
fn start_service() -> Result<()> {
  let status_handle =
    service_control_handler::register(SERVICE_NAME, |_| ServiceControlHandlerResult::NoError)?;

  status_handle.set_service_status(ServiceStatus {
    service_type: SERVICE_TYPE,
    current_state: ServiceState::Running,
    controls_accepted: ServiceControlAccept::STOP,
    exit_code: ServiceExitCode::Win32(0),
    checkpoint: 0,
    wait_hint: Duration::default(),
    process_id: None,
  })?;

  Ok(())
}

// 停止服务
fn stop_service() -> Result<()> {
  let status_handle =
    service_control_handler::register(SERVICE_NAME, |_| ServiceControlHandlerResult::NoError)?;

  status_handle.set_service_status(ServiceStatus {
    service_type: SERVICE_TYPE,
    current_state: ServiceState::Stopped,
    controls_accepted: ServiceControlAccept::empty(),
    exit_code: ServiceExitCode::Win32(0),
    checkpoint: 0,
    wait_hint: Duration::default(),
    process_id: None,
  })?;

  Ok(())
}

// 启动clash进程
fn start_clash(body: StartBody, clash_status: Arc<Mutex<ClashStatus>>) -> anyhow::Result<()> {
  // stop the old clash bin
  let _ = stop_clash(clash_status.clone());

  let log = File::create(body.log_file.clone()).context("failed to open log")?;
  let cmd = Command::new(body.bin_path.clone())
    .args(["-d", body.config_dir.as_str()])
    .stdout(log)
    .spawn()?;

  let mut arc = clash_status.lock();
  arc.child = Some(cmd);
  arc.info = Some(body);

  Ok(())
}

// 停止clash进程
fn stop_clash(clash_status: Arc<Mutex<ClashStatus>>) -> anyhow::Result<()> {
  let mut arc = clash_status.lock();

  arc.info = None;

  match arc.child.take() {
    Some(mut child) => child.kill().context("failed to kill clash"),
    None => bail!("clash not executed"),
  }
}

// 获取clash当前执行信息
fn get_clash(clash_status: Arc<Mutex<ClashStatus>>) -> anyhow::Result<StartBody> {
  let arc = clash_status.lock();

  match arc.info.clone() {
    Some(info) => Ok(info),
    None => bail!("clash not executed"),
  }
}

/// Service Main function

pub fn main() -> Result<()> {
  service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

define_windows_service!(ffi_service_main, my_service_main);

pub fn my_service_main(_arguments: Vec<OsString>) {
  if let Ok(rt) = Runtime::new() {
    rt.block_on(async {
      let _ = run_service().await;
    });
  }
}
