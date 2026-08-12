//! 요청 전체 대신 운영에 필요한 안전한 수명주기 필드만 log로 투영한다.

use serde::Serialize;

use crate::protocol::{ErrorCode, Request, Response, TaskPayload, TerminationReason};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestAudit<'a> {
    event: &'static str,
    request_id: &'a str,
    operation: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseAudit<'a> {
    event: &'static str,
    request_id: &'a str,
    operation: &'static str,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    termination_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<&'a str>,
}

pub(crate) fn request_operation(request: &Request) -> &'static str {
    match request {
        Request::GetCapabilities { .. } => "getCapabilities",
        Request::SubmitTask { .. } => "submitTask",
        Request::GetTask { .. } => "getTask",
        Request::CancelTask { .. } => "cancelTask",
        Request::SubmitProfile { .. } => "submitProfile",
        Request::GetProfileResult { .. } => "getProfileResult",
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn log_request(request: &Request) {
    let audit = request_audit(request);
    tracing::info!(
        event = audit.event,
        request_id = audit.request_id,
        operation = audit.operation,
        "protocol request received"
    );
}

#[cfg(target_os = "linux")]
pub(crate) fn log_response(operation: &'static str, response: &Response) {
    let audit = response_audit(operation, response);
    match audit.event {
        "task_finished" | "task_cancelled" => tracing::info!(
            event = audit.event,
            request_id = audit.request_id,
            operation = audit.operation,
            outcome = audit.outcome,
            task_id = audit.task_id.unwrap_or(""),
            termination_reason = audit.termination_reason.unwrap_or(""),
            cleanup_complete = audit.cleanup_complete.unwrap_or(false),
            exit_code = audit.exit_code,
            signal = audit.signal.unwrap_or(""),
            "protocol task result returned"
        ),
        "task_admitted" | "task_observed" => tracing::info!(
            event = audit.event,
            request_id = audit.request_id,
            operation = audit.operation,
            outcome = audit.outcome,
            task_id = audit.task_id.unwrap_or(""),
            "protocol task state returned"
        ),
        "request_rejected" => tracing::info!(
            event = audit.event,
            request_id = audit.request_id,
            operation = audit.operation,
            outcome = audit.outcome,
            error_code = audit.error_code.unwrap_or(""),
            "protocol request rejected"
        ),
        _ => tracing::info!(
            event = audit.event,
            request_id = audit.request_id,
            operation = audit.operation,
            outcome = audit.outcome,
            "protocol request completed"
        ),
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn log_invalid_request(request_id: &str) {
    tracing::info!(
        event = "request_rejected",
        request_id,
        operation = "unknown",
        outcome = "REJECTED",
        error_code = "INVALID_REQUEST",
        "protocol request rejected"
    );
}

pub(crate) fn log_task_finished(payload: &TaskPayload) {
    let TaskPayload::Finished {
        task_id,
        termination_reason,
        process,
        usage,
        ..
    } = payload
    else {
        return;
    };
    tracing::info!(
        event = "task_finished",
        task_id,
        termination_reason = termination_reason_name(*termination_reason),
        cleanup_complete = true,
        exit_code = process.exit_code,
        signal = process.signal.as_deref().unwrap_or(""),
        cpu_time_micros = usage.cpu_time_micros,
        memory_peak_bytes = usage.memory_peak_bytes,
        "task cleanup and termination evidence stored"
    );
}

fn request_audit(request: &Request) -> RequestAudit<'_> {
    RequestAudit {
        event: "request_received",
        request_id: request.request_id(),
        operation: request_operation(request),
    }
}

fn response_audit<'a>(operation: &'static str, response: &'a Response) -> ResponseAudit<'a> {
    let request_id = response.request_id();
    match response {
        Response::Capabilities { payload, .. } => ResponseAudit {
            event: "status_reported",
            request_id,
            operation,
            outcome: if payload.cgroup_v2_ready {
                "READY"
            } else {
                "UNREADY"
            },
            task_id: None,
            error_code: None,
            termination_reason: None,
            cleanup_complete: None,
            exit_code: None,
            signal: None,
        },
        Response::TaskAccepted { payload, .. } => ResponseAudit {
            event: "task_admitted",
            request_id,
            operation,
            outcome: "RUNNING",
            task_id: Some(&payload.task_id),
            error_code: None,
            termination_reason: None,
            cleanup_complete: None,
            exit_code: None,
            signal: None,
        },
        Response::Task {
            payload: TaskPayload::Running { task_id, .. },
            ..
        } => ResponseAudit {
            event: "task_observed",
            request_id,
            operation,
            outcome: "RUNNING",
            task_id: Some(task_id),
            error_code: None,
            termination_reason: None,
            cleanup_complete: None,
            exit_code: None,
            signal: None,
        },
        Response::Task {
            payload:
                TaskPayload::Finished {
                    task_id,
                    termination_reason,
                    process,
                    ..
                },
            ..
        } => ResponseAudit {
            event: "task_finished",
            request_id,
            operation,
            outcome: "FINISHED",
            task_id: Some(task_id),
            error_code: None,
            termination_reason: Some(termination_reason_name(*termination_reason)),
            cleanup_complete: Some(true),
            exit_code: process.exit_code,
            signal: process.signal.as_deref(),
        },
        Response::TaskCancelled { payload, .. } => ResponseAudit {
            event: "task_cancelled",
            request_id,
            operation,
            outcome: "FINISHED",
            task_id: Some(&payload.task_id),
            error_code: None,
            termination_reason: Some(termination_reason_name(payload.termination_reason)),
            cleanup_complete: Some(true),
            exit_code: None,
            signal: None,
        },
        Response::ProfileAccepted { payload, .. } => ResponseAudit {
            event: "task_admitted",
            request_id,
            operation,
            outcome: "RUNNING",
            task_id: Some(&payload.task_id),
            error_code: None,
            termination_reason: None,
            cleanup_complete: None,
            exit_code: None,
            signal: None,
        },
        Response::ProfileResult {
            payload: crate::protocol::ProfileTaskPayload::Running { task_id, .. },
            ..
        } => ResponseAudit {
            event: "task_observed",
            request_id,
            operation,
            outcome: "RUNNING",
            task_id: Some(task_id),
            error_code: None,
            termination_reason: None,
            cleanup_complete: None,
            exit_code: None,
            signal: None,
        },
        Response::ProfileResult {
            payload:
                crate::protocol::ProfileTaskPayload::Finished {
                    task_id,
                    termination_reason,
                    process,
                    ..
                },
            ..
        } => ResponseAudit {
            event: "task_finished",
            request_id,
            operation,
            outcome: "FINISHED",
            task_id: Some(task_id),
            error_code: None,
            termination_reason: Some(termination_reason_name(*termination_reason)),
            cleanup_complete: Some(true),
            exit_code: process.exit_code,
            signal: process.signal.as_deref(),
        },
        Response::Error { payload, .. } => ResponseAudit {
            event: "request_rejected",
            request_id,
            operation,
            outcome: "REJECTED",
            task_id: None,
            error_code: Some(error_code_name(payload.code)),
            termination_reason: None,
            cleanup_complete: None,
            exit_code: None,
            signal: None,
        },
    }
}

fn termination_reason_name(reason: TerminationReason) -> &'static str {
    match reason {
        TerminationReason::Exited => "EXITED",
        TerminationReason::ExecutionFailed => "EXECUTION_FAILED",
        TerminationReason::Cancelled => "CANCELLED",
        TerminationReason::TimedOut => "TIMED_OUT",
        TerminationReason::MemoryLimitExceeded => "MEMORY_LIMIT_EXCEEDED",
        TerminationReason::ProcessLimitExceeded => "PROCESS_LIMIT_EXCEEDED",
        TerminationReason::DaemonError => "DAEMON_ERROR",
    }
}

fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidRequest => "INVALID_REQUEST",
        ErrorCode::UnsupportedProtocolVersion => "UNSUPPORTED_PROTOCOL_VERSION",
        ErrorCode::FrameTooLarge => "FRAME_TOO_LARGE",
        ErrorCode::EnvironmentUnavailable => "ENVIRONMENT_UNAVAILABLE",
        ErrorCode::CapacityExhausted => "CAPACITY_EXHAUSTED",
        ErrorCode::TaskNotFound => "TASK_NOT_FOUND",
        ErrorCode::TaskAlreadyFinished => "TASK_ALREADY_FINISHED",
        ErrorCode::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
        ErrorCode::LimitExceedsPolicy => "LIMIT_EXCEEDS_POLICY",
        ErrorCode::InternalError => "INTERNAL_ERROR",
        ErrorCode::ProfileNotFound => "PROFILE_NOT_FOUND",
        ErrorCode::InvalidProfileInput => "INVALID_PROFILE_INPUT",
        ErrorCode::InvalidArtifactPath => "INVALID_ARTIFACT_PATH",
        ErrorCode::ArtifactDigestMismatch => "ARTIFACT_DIGEST_MISMATCH",
        ErrorCode::TaskKindMismatch => "TASK_KIND_MISMATCH",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::protocol::{
        CommandSpec, CpuMax, OutputLimits, ProcessResult, ResourceLimits, SubmitTaskPayload,
        TaskOutput, TaskTiming, TaskUsage,
    };

    const ARG_SECRET: &str = "ARG_SECRET_7d28";
    const ENV_SECRET: &str = "ENV_SECRET_94f1";
    const PATH_SECRET: &str = "/srv/private/customer-42";
    const OUTPUT_SECRET: &str = "OUTPUT_SECRET_b130";

    #[test]
    fn request_audit_excludes_command_environment_and_paths() {
        let request = Request::SubmitTask {
            protocol_version: 1,
            request_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            payload: SubmitTaskPayload {
                client_request_id: "22222222-2222-2222-2222-222222222222".to_owned(),
                command: CommandSpec {
                    program: format!("{PATH_SECRET}/tool"),
                    args: vec![ARG_SECRET.to_owned()],
                    working_directory: PATH_SECRET.to_owned(),
                    environment: BTreeMap::from([("TOKEN".to_owned(), ENV_SECRET.to_owned())]),
                },
                limits: ResourceLimits {
                    cpu_max: CpuMax {
                        quota_micros: 1,
                        period_micros: 1,
                    },
                    memory_max_bytes: 1,
                    pids_max: 1,
                    wall_time_limit_ms: 1,
                },
                output: OutputLimits {
                    stdout_tail_max_bytes: 1,
                    stderr_tail_max_bytes: 1,
                },
            },
        };

        let encoded = serde_json::to_string(&request_audit(&request)).unwrap();
        assert!(encoded.contains("submitTask"));
        for secret in [ARG_SECRET, ENV_SECRET, PATH_SECRET] {
            assert!(!encoded.contains(secret));
        }
    }

    #[test]
    fn response_audit_excludes_captured_output() {
        let response = Response::Task {
            protocol_version: 1,
            request_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            payload: TaskPayload::Finished {
                task_id: "task-1".to_owned(),
                termination_reason: TerminationReason::Exited,
                process: ProcessResult {
                    exit_code: Some(0),
                    signal: None,
                },
                timing: TaskTiming {
                    submitted_at: String::new(),
                    started_at: String::new(),
                    finished_at: String::new(),
                    wall_time_ms: 1,
                },
                usage: TaskUsage {
                    cpu_time_micros: 1,
                    memory_peak_bytes: 1,
                },
                output: TaskOutput {
                    stdout_tail: OUTPUT_SECRET.to_owned(),
                    stderr_tail: OUTPUT_SECRET.to_owned(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                },
            },
        };

        let encoded = serde_json::to_string(&response_audit("getTask", &response)).unwrap();
        assert!(encoded.contains("EXITED"));
        assert!(encoded.contains("cleanupComplete"));
        assert!(!encoded.contains(OUTPUT_SECRET));
    }
}
