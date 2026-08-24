#[cfg(target_os = "linux")]
pub(crate) mod capsule;
pub(crate) mod task;

use taskcage_core::capsule::ProfileCall;

/// Inbound protocol mapper가 만든 transport-neutral Profile submit이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileSubmission {
    client_request_id: String,
    call: ProfileCall,
}

impl ProfileSubmission {
    pub(crate) fn new(client_request_id: String, call: ProfileCall) -> Self {
        Self {
            client_request_id,
            call,
        }
    }

    pub(crate) fn client_request_id(&self) -> &str {
        &self.client_request_id
    }

    pub(crate) fn call(&self) -> &ProfileCall {
        &self.call
    }
}

/// Transport가 안정적인 wire 오류로 변환하는 protocol-neutral use case 분류다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UseCaseErrorCode {
    ArtifactDigestMismatch,
    CapacityExhausted,
    EnvironmentUnavailable,
    IdempotencyConflict,
    InternalError,
    InvalidArtifactPath,
    InvalidProfileInput,
    LimitExceedsPolicy,
    ProfileNotFound,
    TaskAlreadyFinished,
    TaskNotFound,
}
