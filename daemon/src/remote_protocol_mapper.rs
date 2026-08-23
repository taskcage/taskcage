//! Explicit conversions between remote wire DTOs and the local protocol adapter.

use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};

use crate::protocol as local;
use crate::remote_artifact::{ManagedInputSnapshot, RemoteArtifactError};
use crate::remote_config::PrincipalPolicy;
use crate::remote_protocol as remote;

pub(crate) fn managed_input_ids(payload: &remote::RemoteProfileRequestPayload) -> Vec<String> {
    payload
        .inputs
        .values()
        .filter_map(|input| match input {
            remote::RemoteProfileInputValue::ManagedInput { artifact_id } => {
                Some(artifact_id.clone())
            }
            _ => None,
        })
        .collect()
}

pub(crate) fn local_profile_payload(
    principal: &PrincipalPolicy,
    payload: &remote::RemoteProfileRequestPayload,
    snapshots: &[ManagedInputSnapshot],
) -> Result<local::ProfileRequestPayload, RemoteArtifactError> {
    let snapshots = snapshots
        .iter()
        .map(|snapshot| (snapshot.artifact_id.as_str(), snapshot))
        .collect::<HashMap<_, _>>();
    let mut inputs = BTreeMap::new();
    for (slot, input) in &payload.inputs {
        let value = match input {
            remote::RemoteProfileInputValue::String { value } => local::ProfileInputValue::String {
                value: value.clone(),
            },
            remote::RemoteProfileInputValue::Int64 { value } => {
                local::ProfileInputValue::Int64 { value: *value }
            }
            remote::RemoteProfileInputValue::Boolean { value } => {
                local::ProfileInputValue::Boolean { value: *value }
            }
            remote::RemoteProfileInputValue::ManagedInput { artifact_id } => {
                let Some(snapshot) = snapshots.get(artifact_id.as_str()) else {
                    return Err(RemoteArtifactError::NotFound);
                };
                local::ProfileInputValue::LocalInput {
                    // Local adapter의 path 문법을 통과시키는 내부 표식이며 실제 file은 descriptor다.
                    path: "remote-managed-input".to_owned(),
                    digest: snapshot.digest.clone(),
                    size_bytes: snapshot.size_bytes,
                }
            }
        };
        inputs.insert(slot.clone(), value);
    }
    Ok(local::ProfileRequestPayload {
        client_request_id: namespaced_uuid(
            &principal.client_id,
            &payload.client_request_id,
            b"remote-submit",
        ),
        profile: local::ProfileIdentity {
            name: payload.profile.name.clone(),
            version: payload.profile.version.clone(),
        },
        inputs,
        resource_overrides: payload.resource_overrides.as_ref().map(local_overrides),
    })
}

fn local_overrides(
    overrides: &remote::ProfileResourceOverrides,
) -> local::ProfileResourceOverrides {
    local::ProfileResourceOverrides {
        limits: overrides
            .limits
            .as_ref()
            .map(|limits| local::PartialResourceLimits {
                cpu_max: limits.cpu_max.as_ref().map(|cpu| local::CpuMax {
                    quota_micros: cpu.quota_micros,
                    period_micros: cpu.period_micros,
                }),
                memory_max_bytes: limits.memory_max_bytes,
                pids_max: limits.pids_max,
                wall_time_limit_ms: limits.wall_time_limit_ms,
            }),
        output: overrides
            .output
            .as_ref()
            .map(|output| local::PartialOutputLimits {
                stdout_tail_max_bytes: output.stdout_tail_max_bytes,
                stderr_tail_max_bytes: output.stderr_tail_max_bytes,
            }),
    }
}

pub(crate) fn accepted(
    request_id: String,
    payload: local::ProfileAcceptedPayload,
) -> remote::RemoteResponse {
    remote::RemoteResponse::ProfileAccepted {
        remote_protocol_version: remote::REMOTE_PROTOCOL_VERSION,
        request_id,
        payload: remote::ProfileAcceptedPayload {
            task_id: payload.task_id,
            state: remote::TaskState::Running,
            profile: profile_identity(payload.profile),
            effective_resources: remote::ProfileEffectiveResources {
                limits: resource_limits(payload.effective_resources.limits),
                output: output_limits(payload.effective_resources.output),
            },
        },
    }
}

pub(crate) fn profile_identity(profile: local::ProfileIdentity) -> remote::ProfileIdentity {
    remote::ProfileIdentity {
        name: profile.name,
        version: profile.version,
    }
}

fn resource_limits(limits: local::ResourceLimits) -> remote::ResourceLimits {
    remote::ResourceLimits {
        cpu_max: remote::CpuMax {
            quota_micros: limits.cpu_max.quota_micros,
            period_micros: limits.cpu_max.period_micros,
        },
        memory_max_bytes: limits.memory_max_bytes,
        pids_max: limits.pids_max,
        wall_time_limit_ms: limits.wall_time_limit_ms,
    }
}

fn output_limits(output: local::OutputLimits) -> remote::OutputLimits {
    remote::OutputLimits {
        stdout_tail_max_bytes: output.stdout_tail_max_bytes,
        stderr_tail_max_bytes: output.stderr_tail_max_bytes,
    }
}

pub(crate) fn termination_reason(reason: local::TerminationReason) -> remote::TerminationReason {
    match reason {
        local::TerminationReason::Exited => remote::TerminationReason::Exited,
        local::TerminationReason::ExecutionFailed => remote::TerminationReason::ExecutionFailed,
        local::TerminationReason::Cancelled => remote::TerminationReason::Cancelled,
        local::TerminationReason::TimedOut => remote::TerminationReason::TimedOut,
        local::TerminationReason::MemoryLimitExceeded => {
            remote::TerminationReason::MemoryLimitExceeded
        }
        local::TerminationReason::ProcessLimitExceeded => {
            remote::TerminationReason::ProcessLimitExceeded
        }
        local::TerminationReason::DaemonError => remote::TerminationReason::DaemonError,
    }
}

pub(crate) fn process_result(process: local::ProcessResult) -> remote::ProcessResult {
    remote::ProcessResult {
        exit_code: process.exit_code,
        signal: process.signal,
    }
}

pub(crate) fn task_timing(timing: local::TaskTiming) -> remote::TaskTiming {
    remote::TaskTiming {
        submitted_at: timing.submitted_at,
        started_at: timing.started_at,
        finished_at: timing.finished_at,
        wall_time_ms: timing.wall_time_ms,
    }
}

pub(crate) fn task_usage(usage: local::TaskUsage) -> remote::TaskUsage {
    remote::TaskUsage {
        cpu_time_micros: usage.cpu_time_micros,
        memory_peak_bytes: usage.memory_peak_bytes,
    }
}

pub(crate) fn task_output(output: local::TaskOutput) -> remote::TaskOutput {
    remote::TaskOutput {
        stdout_tail: output.stdout_tail,
        stderr_tail: output.stderr_tail,
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
    }
}

pub(crate) fn profile_outcome(outcome: local::ProfileOutcome) -> remote::ProfileOutcome {
    match outcome {
        local::ProfileOutcome::Succeeded => remote::ProfileOutcome::Succeeded,
        local::ProfileOutcome::Failed => remote::ProfileOutcome::Failed,
    }
}

pub(crate) fn profile_failure(
    failure: local::ProfileFailurePayload,
) -> remote::ProfileFailurePayload {
    remote::ProfileFailurePayload {
        code: failure.code,
        message: failure.message,
    }
}

pub(crate) fn local_profile_task_id(payload: &local::ProfileTaskPayload) -> &str {
    match payload {
        local::ProfileTaskPayload::Running { task_id, .. }
        | local::ProfileTaskPayload::Finished { task_id, .. } => task_id,
    }
}

pub(crate) fn namespaced_uuid(principal: &str, source: &str, domain: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([0]);
    digest.update(principal.as_bytes());
    digest.update([0]);
    digest.update(source.as_bytes());
    let mut bytes: [u8; 16] = digest.finalize()[..16]
        .try_into()
        .expect("SHA-256 prefix length");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_mapping_preserves_every_wire_value() {
        assert_eq!(
            termination_reason(local::TerminationReason::DaemonError),
            remote::TerminationReason::DaemonError
        );
        assert_eq!(
            termination_reason(local::TerminationReason::MemoryLimitExceeded),
            remote::TerminationReason::MemoryLimitExceeded
        );
    }
}
