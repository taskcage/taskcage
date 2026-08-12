//! `docs/api-mvp.md`에 정의된 TaskCage protocol v1 wire 타입이다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
/// Local Profile Core가 추가하는 additive wire protocol version이다.
pub const PROFILE_PROTOCOL_VERSION: u32 = 2;
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
    #[serde(rename = "submitProfile")]
    SubmitProfile {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ProfileRequestPayload,
    },
    #[serde(rename = "getProfileResult")]
    GetProfileResult {
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
            }
            | Self::SubmitProfile {
                protocol_version, ..
            }
            | Self::GetProfileResult {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    pub fn request_id(&self) -> &str {
        match self {
            Self::GetCapabilities { request_id, .. }
            | Self::SubmitTask { request_id, .. }
            | Self::GetTask { request_id, .. }
            | Self::CancelTask { request_id, .. }
            | Self::SubmitProfile { request_id, .. }
            | Self::GetProfileResult { request_id, .. } => request_id,
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

/// Protocol v2의 daemon-installed Profile 실행 요청이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileRequestPayload {
    pub client_request_id: String,
    pub profile: ProfileIdentity,
    pub inputs: BTreeMap<String, ProfileInputValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_overrides: Option<ProfileResourceOverrides>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ProfileInputValue {
    #[serde(rename = "STRING")]
    String { value: String },
    #[serde(rename = "INT64")]
    Int64 { value: i64 },
    #[serde(rename = "BOOLEAN")]
    Boolean { value: bool },
    #[serde(rename = "LOCAL_INPUT")]
    LocalInput {
        path: String,
        digest: String,
        #[serde(rename = "sizeBytes")]
        size_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileResourceOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<PartialResourceLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PartialOutputLimits>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartialResourceLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_max: Option<CpuMax>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids_max: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time_limit_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartialOutputLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail_max_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail_max_bytes: Option<u32>,
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
    #[serde(rename = "profileAccepted")]
    ProfileAccepted {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ProfileAcceptedPayload,
    },
    #[serde(rename = "profileResult")]
    ProfileResult {
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ProfileTaskPayload,
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
            }
            | Self::ProfileAccepted {
                protocol_version, ..
            }
            | Self::ProfileResult {
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
            | Self::Error { request_id, .. }
            | Self::ProfileAccepted { request_id, .. }
            | Self::ProfileResult { request_id, .. } => request_id,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileAcceptedPayload {
    pub task_id: String,
    pub state: TaskState,
    pub profile: ProfileIdentity,
    pub effective_resources: ProfileEffectiveResources,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileEffectiveResources {
    pub limits: ResourceLimits,
    pub output: OutputLimits,
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

/// Protocol v2 Profile Task의 실행 중 또는 cleanup-confirmed terminal snapshot이다.
#[allow(
    clippy::large_enum_variant,
    reason = "Profile finished snapshots are copied only inside the bounded 1 MiB wire frame and keep the contract's flat JSON shape"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", deny_unknown_fields)]
pub enum ProfileTaskPayload {
    #[serde(rename = "RUNNING")]
    Running {
        #[serde(rename = "taskId")]
        task_id: String,
        profile: ProfileIdentity,
        #[serde(rename = "submittedAt")]
        submitted_at: String,
        #[serde(rename = "startedAt")]
        started_at: String,
    },
    #[serde(rename = "FINISHED")]
    Finished {
        #[serde(rename = "taskId")]
        task_id: String,
        profile: ProfileIdentity,
        #[serde(rename = "profileOutcome")]
        profile_outcome: ProfileOutcome,
        #[serde(rename = "terminationReason")]
        termination_reason: TerminationReason,
        process: ProcessResult,
        timing: TaskTiming,
        usage: TaskUsage,
        output: TaskOutput,
        artifacts: BTreeMap<String, PublishedArtifactPayload>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<ProfileFailurePayload>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileOutcome {
    #[serde(rename = "SUCCEEDED")]
    Succeeded,
    #[serde(rename = "FAILED")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishedArtifactPayload {
    pub kind: PublishedArtifactKind,
    pub path: String,
    pub digest: String,
    pub size_bytes: u64,
    pub media_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublishedArtifactKind {
    #[serde(rename = "LOCAL_FILE")]
    LocalFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileFailurePayload {
    pub code: String,
    pub message: String,
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
    #[serde(rename = "PROFILE_NOT_FOUND")]
    ProfileNotFound,
    #[serde(rename = "INVALID_PROFILE_INPUT")]
    InvalidProfileInput,
    #[serde(rename = "INVALID_ARTIFACT_PATH")]
    InvalidArtifactPath,
    #[serde(rename = "ARTIFACT_DIGEST_MISMATCH")]
    ArtifactDigestMismatch,
    #[serde(rename = "TASK_KIND_MISMATCH")]
    TaskKindMismatch,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;

    fn fixture(name: &str) -> String {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(root.join("../protocol-fixtures/v2").join(name))
            .expect("v2 fixture must be readable")
    }

    fn fixture_envelope(name: &str) -> String {
        let mut value: Value = serde_json::from_str(&fixture(name)).expect("fixture JSON");
        let object = value
            .as_object_mut()
            .expect("protocol fixture must be an object");
        object.remove("expectedError");
        object.remove("targetMustStart");
        serde_json::to_string(&value).expect("fixture envelope JSON")
    }

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

    #[test]
    fn v2_request_fixtures_decode_to_typed_profile_messages() {
        let request: Request = serde_json::from_str(&fixture_envelope("submit-profile-valid.json"))
            .expect("valid Profile request fixture");
        assert!(matches!(
            request,
            Request::SubmitProfile {
                protocol_version: PROFILE_PROTOCOL_VERSION,
                payload: ProfileRequestPayload { ref profile, .. },
                ..
            } if profile.name == "file-copy" && profile.version == "1.0.0"
        ));
        assert!(matches!(
            serde_json::from_str::<Request>(&fixture_envelope("get-profile-result.json")),
            Ok(Request::GetProfileResult {
                protocol_version: PROFILE_PROTOCOL_VERSION,
                ..
            })
        ));
    }

    #[test]
    fn v2_response_fixtures_decode_to_typed_profile_messages() {
        for name in [
            "profile-accepted.json",
            "profile-result-running.json",
            "profile-result-success.json",
            "profile-result-output-contract-failed.json",
            "error-profile-not-found.json",
            "error-artifact-digest-mismatch.json",
        ] {
            assert!(
                serde_json::from_str::<Response>(&fixture(name)).is_ok(),
                "fixture must decode: {name}"
            );
        }
    }
}
