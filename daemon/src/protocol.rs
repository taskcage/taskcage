//! `docs/api-mvp.md`에 정의된 TaskCage protocol v1 wire 타입이다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Request {
    #[serde(rename = "getCapabilities")]
    GetCapabilities {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: EmptyPayload,
    },
    #[serde(rename = "submitTask")]
    SubmitTask {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: SubmitTaskPayload,
    },
    #[serde(rename = "getTask")]
    GetTask {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskIdPayload,
    },
    #[serde(rename = "cancelTask")]
    CancelTask {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskIdPayload,
    },
}

impl Request {
    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::GetCapabilities {
                protocol_version, ..
            }
            | Self::SubmitTask {
                protocol_version, ..
            }
            | Self::GetTask {
                protocol_version, ..
            }
            | Self::CancelTask {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    pub fn request_id(&self) -> &str {
        match self {
            Self::GetCapabilities { request_id, .. }
            | Self::SubmitTask { request_id, .. }
            | Self::GetTask { request_id, .. }
            | Self::CancelTask { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct EmptyPayload {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitTaskPayload {
    pub client_request_id: String,
    pub command: CommandSpec,
    pub limits: ResourceLimits,
    pub output: OutputLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLimits {
    pub cpu_max: CpuMax,
    pub memory_max_bytes: u64,
    pub pids_max: u64,
    pub wall_time_limit_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpuMax {
    pub quota_micros: u64,
    pub period_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputLimits {
    pub stdout_tail_max_bytes: u32,
    pub stderr_tail_max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskIdPayload {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    #[serde(rename = "capabilities")]
    Capabilities {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: CapabilitiesPayload,
    },
    #[serde(rename = "taskAccepted")]
    TaskAccepted {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskAcceptedPayload,
    },
    #[serde(rename = "task")]
    Task {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskPayload,
    },
    #[serde(rename = "taskCancelled")]
    TaskCancelled {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskCancelledPayload,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ErrorPayload,
    },
}

impl Response {
    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::Capabilities {
                protocol_version, ..
            }
            | Self::TaskAccepted {
                protocol_version, ..
            }
            | Self::Task {
                protocol_version, ..
            }
            | Self::TaskCancelled {
                protocol_version, ..
            }
            | Self::Error {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    pub fn request_id(&self) -> &str {
        match self {
            Self::Capabilities { request_id, .. }
            | Self::TaskAccepted { request_id, .. }
            | Self::Task { request_id, .. }
            | Self::TaskCancelled { request_id, .. }
            | Self::Error { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesPayload {
    pub daemon_version: String,
    pub protocol_versions: Vec<u32>,
    pub max_frame_bytes: u32,
    pub max_concurrent_tasks: u32,
    pub cgroup_v2_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskAcceptedPayload {
    pub task_id: String,
    pub state: TaskState,
    pub effective_limits: ResourceLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum TaskPayload {
    #[serde(rename = "RUNNING")]
    Running {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "submittedAt")]
        submitted_at: String,
        #[serde(rename = "startedAt")]
        started_at: String,
    },
    #[serde(rename = "FINISHED")]
    Finished {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "terminationReason")]
        termination_reason: TerminationReason,
        process: ProcessResult,
        timing: TaskTiming,
        usage: TaskUsage,
        output: TaskOutput,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCancelledPayload {
    pub task_id: String,
    pub state: TaskState,
    pub termination_reason: TerminationReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskTiming {
    pub submitted_at: String,
    pub started_at: String,
    pub finished_at: String,
    pub wall_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskUsage {
    pub cpu_time_micros: u64,
    pub memory_peak_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOutput {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    #[serde(rename = "RUNNING")]
    Running,
    #[serde(rename = "FINISHED")]
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminationReason {
    #[serde(rename = "EXITED")]
    Exited,
    #[serde(rename = "EXECUTION_FAILED")]
    ExecutionFailed,
    #[serde(rename = "CANCELLED")]
    Cancelled,
    #[serde(rename = "TIMED_OUT")]
    TimedOut,
    #[serde(rename = "MEMORY_LIMIT_EXCEEDED")]
    MemoryLimitExceeded,
    #[serde(rename = "PROCESS_LIMIT_EXCEEDED")]
    ProcessLimitExceeded,
    #[serde(rename = "DAEMON_ERROR")]
    DaemonError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    #[serde(rename = "INVALID_REQUEST")]
    InvalidRequest,
    #[serde(rename = "UNSUPPORTED_PROTOCOL_VERSION")]
    UnsupportedProtocolVersion,
    #[serde(rename = "FRAME_TOO_LARGE")]
    FrameTooLarge,
    #[serde(rename = "ENVIRONMENT_UNAVAILABLE")]
    EnvironmentUnavailable,
    #[serde(rename = "CAPACITY_EXHAUSTED")]
    CapacityExhausted,
    #[serde(rename = "TASK_NOT_FOUND")]
    TaskNotFound,
    #[serde(rename = "TASK_ALREADY_FINISHED")]
    TaskAlreadyFinished,
    #[serde(rename = "IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[serde(rename = "LIMIT_EXCEEDS_POLICY")]
    LimitExceedsPolicy,
    #[serde(rename = "INTERNAL_ERROR")]
    InternalError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_request_fields() {
        let json = r#"{
            "protocolVersion": 1,
            "requestId": "11111111-1111-1111-1111-111111111111",
            "type": "getCapabilities",
            "payload": {},
            "unexpected": true
        }"#;

        assert!(serde_json::from_str::<Request>(json).is_err());
    }

    #[test]
    fn rejects_unknown_request_payload_fields() {
        let json = r#"{
            "protocolVersion": 1,
            "requestId": "11111111-1111-1111-1111-111111111111",
            "type": "getCapabilities",
            "payload": { "unexpected": true }
        }"#;

        assert!(serde_json::from_str::<Request>(json).is_err());
    }

    #[test]
    fn response_ignores_unknown_fields() {
        let json = r#"{
            "protocolVersion": 1,
            "requestId": "11111111-1111-1111-1111-111111111111",
            "type": "capabilities",
            "payload": {
                "daemonVersion": "0.1.0",
                "protocolVersions": [1],
                "maxFrameBytes": 1048576,
                "maxConcurrentTasks": 4,
                "cgroupV2Ready": true,
                "futureField": true
            },
            "futureTopLevelField": true
        }"#;

        assert!(serde_json::from_str::<Response>(json).is_ok());
    }

    #[test]
    fn daemon_unavailable_is_not_a_wire_error_code() {
        assert!(serde_json::from_str::<ErrorCode>(r#""DAEMON_UNAVAILABLE""#).is_err());
    }
}
