//! Java SDK와 주고받을 길이와 판번호가 포함된 JSON 메시지를 정의한다.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub protocol_version: u16,
    pub job_id: String,
    pub command: Vec<String>,
    pub budget: ResourceBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceBudget {
    pub memory_bytes: u64,
    pub max_processes: u32,
    pub wall_time_nanos: u64,
    pub max_output_bytes: u64,
}
