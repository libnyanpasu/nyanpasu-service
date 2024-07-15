use crate::api::R;
use serde::{Deserialize, Serialize};

pub const CORE_STOP_ENDPOINT: &str = "/core/stop";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CoreReq {}

pub type StatusRes<'a> = R<'a, ()>;
