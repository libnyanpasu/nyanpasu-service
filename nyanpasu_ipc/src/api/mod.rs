mod core;
mod status;

use serde::{Deserialize, Serialize};
use std::fmt::Debug;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default)]
pub enum ResponseCode {
    #[default]
    Ok = 0,
    OtherError = -1,
}

/// The IPC Response body definition
#[derive(Debug, Serialize, Deserialize)]
pub struct R<'a, T: Serialize + Deserialize + Debug> {
    pub code: ResponseCode,
    pub msg: &'a str,
    pub data: T,
    pub ts: i64,
}
