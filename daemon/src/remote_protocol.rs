//! `docs/remote-protocol-v1.md`에 정의된 Remote Protocol v1 wire 타입이다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const REMOTE_PROTOCOL_VERSION: u32 = 1;
pub const REMOTE_MAX_FRAME_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RemoteRequest {
    #[serde(rename = "authenticate")]
    Authenticate {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: AuthenticatePayload,
    },
    #[serde(rename = "getCapabilities")]
    GetCapabilities {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: EmptyPayload,
    },
    #[serde(rename = "beginArtifactUpload")]
    BeginArtifactUpload {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: BeginArtifactUploadPayload,
    },
    #[serde(rename = "uploadArtifactChunk")]
    UploadArtifactChunk {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: UploadArtifactChunkPayload,
    },
    #[serde(rename = "completeArtifactUpload")]
    CompleteArtifactUpload {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ArtifactIdPayload,
    },
    #[serde(rename = "abortArtifactUpload")]
    AbortArtifactUpload {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ArtifactIdPayload,
    },
    #[serde(rename = "readArtifactChunk")]
    ReadArtifactChunk {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ReadArtifactChunkPayload,
    },
    #[serde(rename = "submitProfile")]
    SubmitProfile {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: RemoteProfileRequestPayload,
    },
    #[serde(rename = "getProfileResult")]
    GetProfileResult {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskIdPayload,
    },
    #[serde(rename = "cancelTask")]
    CancelTask {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskIdPayload,
    },
    /// Remote에서 Raw Command 요청을 명시적으로 거부하기 위해 type만 인식한다.
    #[serde(rename = "submitTask")]
    SubmitTask {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: Value,
    },
}

impl RemoteRequest {
    pub fn remote_protocol_version(&self) -> u32 {
        match self {
            Self::Authenticate {
                remote_protocol_version,
                ..
            }
            | Self::GetCapabilities {
                remote_protocol_version,
                ..
            }
            | Self::BeginArtifactUpload {
                remote_protocol_version,
                ..
            }
            | Self::UploadArtifactChunk {
                remote_protocol_version,
                ..
            }
            | Self::CompleteArtifactUpload {
                remote_protocol_version,
                ..
            }
            | Self::AbortArtifactUpload {
                remote_protocol_version,
                ..
            }
            | Self::ReadArtifactChunk {
                remote_protocol_version,
                ..
            }
            | Self::SubmitProfile {
                remote_protocol_version,
                ..
            }
            | Self::GetProfileResult {
                remote_protocol_version,
                ..
            }
            | Self::CancelTask {
                remote_protocol_version,
                ..
            }
            | Self::SubmitTask {
                remote_protocol_version,
                ..
            } => *remote_protocol_version,
        }
    }

    pub fn request_id(&self) -> &str {
        match self {
            Self::Authenticate { request_id, .. }
            | Self::GetCapabilities { request_id, .. }
            | Self::BeginArtifactUpload { request_id, .. }
            | Self::UploadArtifactChunk { request_id, .. }
            | Self::CompleteArtifactUpload { request_id, .. }
            | Self::AbortArtifactUpload { request_id, .. }
            | Self::ReadArtifactChunk { request_id, .. }
            | Self::SubmitProfile { request_id, .. }
            | Self::GetProfileResult { request_id, .. }
            | Self::CancelTask { request_id, .. }
            | Self::SubmitTask { request_id, .. } => request_id,
        }
    }

    pub fn operation(&self) -> &'static str {
        match self {
            Self::Authenticate { .. } => "authenticate",
            Self::GetCapabilities { .. } => "getCapabilities",
            Self::BeginArtifactUpload { .. } => "beginArtifactUpload",
            Self::UploadArtifactChunk { .. } => "uploadArtifactChunk",
            Self::CompleteArtifactUpload { .. } => "completeArtifactUpload",
            Self::AbortArtifactUpload { .. } => "abortArtifactUpload",
            Self::ReadArtifactChunk { .. } => "readArtifactChunk",
            Self::SubmitProfile { .. } => "submitProfile",
            Self::GetProfileResult { .. } => "getProfileResult",
            Self::CancelTask { .. } => "cancelTask",
            Self::SubmitTask { .. } => "submitTask",
        }
    }

    pub fn validate_envelope(&self) -> Result<(), RequestValidationError> {
        if self.remote_protocol_version() != REMOTE_PROTOCOL_VERSION {
            return Err(RequestValidationError::UnsupportedVersion(
                self.remote_protocol_version(),
            ));
        }
        if !is_uuid(self.request_id()) {
            return Err(RequestValidationError::InvalidRequestId);
        }
        let payload_ids_are_valid = match self {
            Self::BeginArtifactUpload { payload, .. } => is_uuid(&payload.client_artifact_id),
            Self::UploadArtifactChunk { payload, .. } => is_uuid(&payload.artifact_id),
            Self::CompleteArtifactUpload { payload, .. }
            | Self::AbortArtifactUpload { payload, .. } => is_uuid(&payload.artifact_id),
            Self::ReadArtifactChunk { payload, .. } => is_uuid(&payload.artifact_id),
            Self::SubmitProfile { payload, .. } => {
                is_uuid(&payload.client_request_id)
                    && payload.inputs.values().all(|input| match input {
                        RemoteProfileInputValue::ManagedInput { artifact_id } => {
                            is_uuid(artifact_id)
                        }
                        RemoteProfileInputValue::String { .. }
                        | RemoteProfileInputValue::Int64 { .. }
                        | RemoteProfileInputValue::Boolean { .. } => true,
                    })
            }
            Self::GetProfileResult { payload, .. } | Self::CancelTask { payload, .. } => {
                is_uuid(&payload.task_id)
            }
            Self::SubmitTask { payload, .. } => payload.is_object(),
            Self::Authenticate { .. } | Self::GetCapabilities { .. } => true,
        };
        if !payload_ids_are_valid {
            return Err(RequestValidationError::InvalidPayload);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestValidationError {
    UnsupportedVersion(u32),
    InvalidRequestId,
    InvalidPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct EmptyPayload {}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticatePayload {
    pub client_id: String,
    pub secret: String,
}

impl std::fmt::Debug for AuthenticatePayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatePayload")
            .field("client_id", &self.client_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl AuthenticatePayload {
    pub fn validate(&self) -> bool {
        valid_client_id(&self.client_id) && (1..=4_096).contains(&self.secret.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeginArtifactUploadPayload {
    pub client_artifact_id: String,
    pub digest: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadArtifactChunkPayload {
    pub artifact_id: String,
    pub offset: u64,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadArtifactChunkPayload {
    pub artifact_id: String,
    pub offset: u64,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactIdPayload {
    pub artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskIdPayload {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteProfileRequestPayload {
    pub client_request_id: String,
    pub profile: ProfileIdentity,
    pub inputs: BTreeMap<String, RemoteProfileInputValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_overrides: Option<ProfileResourceOverrides>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum RemoteProfileInputValue {
    #[serde(rename = "STRING")]
    String { value: String },
    #[serde(rename = "INT64")]
    Int64 { value: i64 },
    #[serde(rename = "BOOLEAN")]
    Boolean { value: bool },
    #[serde(rename = "MANAGED_INPUT")]
    ManagedInput {
        #[serde(rename = "artifactId")]
        artifact_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileIdentity {
    pub name: String,
    pub version: String,
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

#[allow(
    clippy::large_enum_variant,
    reason = "Remote responses keep the approved flat JSON fixture shape"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RemoteResponse {
    #[serde(rename = "authenticated")]
    Authenticated {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: AuthenticatedPayload,
    },
    #[serde(rename = "capabilities")]
    Capabilities {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: CapabilitiesPayload,
    },
    #[serde(rename = "artifactUploadStarted")]
    ArtifactUploadStarted {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ArtifactUploadStartedPayload,
    },
    #[serde(rename = "artifactChunkAccepted")]
    ArtifactChunkAccepted {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ArtifactChunkAcceptedPayload,
    },
    #[serde(rename = "artifactUploaded")]
    ArtifactUploaded {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ArtifactUploadedPayload,
    },
    #[serde(rename = "artifactUploadAborted")]
    ArtifactUploadAborted {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ArtifactIdPayload,
    },
    #[serde(rename = "artifactChunk")]
    ArtifactChunk {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ArtifactChunkPayload,
    },
    #[serde(rename = "profileAccepted")]
    ProfileAccepted {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ProfileAcceptedPayload,
    },
    #[serde(rename = "profileResult")]
    ProfileResult {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ProfileTaskPayload,
    },
    #[serde(rename = "taskCancelled")]
    TaskCancelled {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: TaskCancelledPayload,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "remoteProtocolVersion")]
        remote_protocol_version: u32,
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ErrorPayload,
    },
}

impl RemoteResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Authenticated { request_id, .. }
            | Self::Capabilities { request_id, .. }
            | Self::ArtifactUploadStarted { request_id, .. }
            | Self::ArtifactChunkAccepted { request_id, .. }
            | Self::ArtifactUploaded { request_id, .. }
            | Self::ArtifactUploadAborted { request_id, .. }
            | Self::ArtifactChunk { request_id, .. }
            | Self::ProfileAccepted { request_id, .. }
            | Self::ProfileResult { request_id, .. }
            | Self::TaskCancelled { request_id, .. }
            | Self::Error { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticatedPayload {
    pub principal: String,
    pub session_expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilitiesPayload {
    pub daemon_version: String,
    pub remote_protocol_versions: Vec<u32>,
    pub max_frame_bytes: u32,
    pub artifact_modes: Vec<ArtifactMode>,
    pub max_artifact_bytes: u64,
    pub max_artifact_chunk_bytes: u32,
    pub artifact_retention_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactMode {
    #[serde(rename = "MANAGED_TRANSFER")]
    ManagedTransfer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactUploadStartedPayload {
    pub artifact_id: String,
    pub state: ArtifactUploadState,
    pub next_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactUploadState {
    #[serde(rename = "UPLOADING")]
    Uploading,
    #[serde(rename = "UPLOADED")]
    Uploaded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactChunkAcceptedPayload {
    pub artifact_id: String,
    pub next_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactUploadedPayload {
    pub artifact_id: String,
    pub digest: String,
    pub size_bytes: u64,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactChunkPayload {
    pub artifact_id: String,
    pub offset: u64,
    pub data_base64: String,
    pub next_offset: u64,
    pub finished: bool,
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

#[allow(
    clippy::large_enum_variant,
    reason = "Remote finished snapshots keep the contract's flat JSON shape"
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
        artifacts: BTreeMap<String, ManagedOutputArtifactPayload>,
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
pub struct ManagedOutputArtifactPayload {
    pub kind: ManagedOutputKind,
    pub artifact_id: String,
    pub digest: String,
    pub size_bytes: u64,
    pub media_type: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedOutputKind {
    #[serde(rename = "MANAGED_OUTPUT")]
    ManagedOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileFailurePayload {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskCancelledPayload {
    pub task_id: String,
    pub state: TaskState,
    pub termination_reason: TerminationReason,
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
    #[serde(rename = "TIMED_OUT")]
    TimedOut,
    #[serde(rename = "CANCELLED")]
    Cancelled,
    #[serde(rename = "MEMORY_LIMIT_EXCEEDED")]
    MemoryLimitExceeded,
    #[serde(rename = "PROCESS_LIMIT_EXCEEDED")]
    ProcessLimitExceeded,
    #[serde(rename = "DAEMON_ERROR")]
    DaemonError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskTiming {
    pub submitted_at: String,
    pub started_at: String,
    pub finished_at: String,
    pub wall_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskUsage {
    pub cpu_time_micros: u64,
    pub memory_peak_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskOutput {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorPayload {
    pub code: RemoteErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteErrorCode {
    #[serde(rename = "INVALID_REQUEST")]
    InvalidRequest,
    #[serde(rename = "UNSUPPORTED_REMOTE_PROTOCOL_VERSION")]
    UnsupportedRemoteProtocolVersion,
    #[serde(rename = "AUTHENTICATION_FAILED")]
    AuthenticationFailed,
    #[serde(rename = "AUTHENTICATION_REQUIRED")]
    AuthenticationRequired,
    #[serde(rename = "AUTHORIZATION_DENIED")]
    AuthorizationDenied,
    #[serde(rename = "INVALID_ARTIFACT_UPLOAD")]
    InvalidArtifactUpload,
    #[serde(rename = "ARTIFACT_UPLOAD_LIMIT_EXCEEDED")]
    ArtifactUploadLimitExceeded,
    #[serde(rename = "ARTIFACT_UPLOAD_QUOTA_EXHAUSTED")]
    ArtifactUploadQuotaExhausted,
    #[serde(rename = "ARTIFACT_DIGEST_MISMATCH")]
    ArtifactDigestMismatch,
    #[serde(rename = "ARTIFACT_NOT_FOUND")]
    ArtifactNotFound,
    #[serde(rename = "ARTIFACT_IN_USE")]
    ArtifactInUse,
    #[serde(rename = "CAPACITY_EXHAUSTED")]
    CapacityExhausted,
    #[serde(rename = "TASK_NOT_FOUND")]
    TaskNotFound,
    #[serde(rename = "TASK_ALREADY_FINISHED")]
    TaskAlreadyFinished,
    #[serde(rename = "PROFILE_NOT_FOUND")]
    ProfileNotFound,
    #[serde(rename = "INVALID_PROFILE_INPUT")]
    InvalidProfileInput,
    #[serde(rename = "IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[serde(rename = "LIMIT_EXCEEDS_POLICY")]
    LimitExceedsPolicy,
    #[serde(rename = "ENVIRONMENT_UNAVAILABLE")]
    EnvironmentUnavailable,
    #[serde(rename = "INTERNAL_ERROR")]
    InternalError,
}

pub fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub fn valid_client_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}
