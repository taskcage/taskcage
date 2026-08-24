//! Remote wire DTO와 공통 domain 의미 사이의 명시적 변환이다.

use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};
use taskcage_core::capsule::{
    CpuMaxOverride, ProfileCall, ProfileIdentity as DomainProfileIdentity,
    ProfileResourceOverrides as DomainResourceOverrides, ProfileValue,
};

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
    let client_request_id = namespaced_uuid(
        &principal.client_id,
        &payload.client_request_id,
        b"remote-submit",
    );
    let call = profile_call(payload, snapshots)?;
    Ok(local_profile_payload_from_domain(client_request_id, call))
}

fn profile_call(
    payload: &remote::RemoteProfileRequestPayload,
    snapshots: &[ManagedInputSnapshot],
) -> Result<ProfileCall, RemoteArtifactError> {
    let snapshots = snapshots
        .iter()
        .map(|snapshot| (snapshot.artifact_id.as_str(), snapshot))
        .collect::<HashMap<_, _>>();
    let mut inputs = Vec::with_capacity(payload.inputs.len());
    for (slot, input) in &payload.inputs {
        let value = match input {
            remote::RemoteProfileInputValue::String { value } => {
                ProfileValue::String(value.clone())
            }
            remote::RemoteProfileInputValue::Int64 { value } => ProfileValue::Int64(*value),
            remote::RemoteProfileInputValue::Boolean { value } => ProfileValue::Boolean(*value),
            remote::RemoteProfileInputValue::ManagedInput { artifact_id } => {
                let Some(snapshot) = snapshots.get(artifact_id.as_str()) else {
                    return Err(RemoteArtifactError::NotFound);
                };
                ProfileValue::LocalInput {
                    // Local adapter의 path 문법을 통과시키는 내부 표식이며 실제 file은 descriptor다.
                    path: "remote-managed-input".to_owned(),
                    digest: snapshot.digest.clone(),
                    size_bytes: snapshot.size_bytes,
                }
            }
        };
        inputs.push((slot.clone(), value));
    }
    let call = ProfileCall::new(
        DomainProfileIdentity::new(&payload.profile.name, &payload.profile.version),
        inputs,
    );
    Ok(match payload.resource_overrides.as_ref() {
        Some(overrides) => call.with_resource_overrides(domain_overrides(overrides)),
        None => call,
    })
}

fn local_profile_payload_from_domain(
    client_request_id: String,
    call: ProfileCall,
) -> local::ProfileRequestPayload {
    let (profile, inputs, resource_overrides) = call.into_parts();
    local::ProfileRequestPayload {
        client_request_id,
        profile: local::ProfileIdentity {
            name: profile.name().to_owned(),
            version: profile.version().to_owned(),
        },
        inputs: inputs
            .into_iter()
            .map(|(slot, value)| (slot, local_profile_value(value)))
            .collect::<BTreeMap<_, _>>(),
        resource_overrides: resource_overrides.map(local_overrides),
    }
}

fn local_profile_value(value: ProfileValue) -> local::ProfileInputValue {
    match value {
        ProfileValue::String(value) => local::ProfileInputValue::String { value },
        ProfileValue::Int64(value) => local::ProfileInputValue::Int64 { value },
        ProfileValue::Boolean(value) => local::ProfileInputValue::Boolean { value },
        ProfileValue::LocalInput {
            path,
            digest,
            size_bytes,
        } => local::ProfileInputValue::LocalInput {
            path,
            digest,
            size_bytes,
        },
    }
}

fn domain_overrides(overrides: &remote::ProfileResourceOverrides) -> DomainResourceOverrides {
    let mut domain = DomainResourceOverrides::new();
    if let Some(limits) = overrides.limits.as_ref() {
        if let Some(cpu) = limits.cpu_max.as_ref() {
            domain = domain.with_cpu_max(CpuMaxOverride::new(cpu.quota_micros, cpu.period_micros));
        }
        if let Some(value) = limits.memory_max_bytes {
            domain = domain.with_memory_max_bytes(value);
        }
        if let Some(value) = limits.pids_max {
            domain = domain.with_pids_max(value);
        }
        if let Some(value) = limits.wall_time_limit_ms {
            domain = domain.with_wall_time_limit_ms(value);
        }
    }
    if let Some(output) = overrides.output.as_ref() {
        if let Some(value) = output.stdout_tail_max_bytes {
            domain = domain.with_stdout_tail_max_bytes(value);
        }
        if let Some(value) = output.stderr_tail_max_bytes {
            domain = domain.with_stderr_tail_max_bytes(value);
        }
    }
    domain
}

fn local_overrides(overrides: DomainResourceOverrides) -> local::ProfileResourceOverrides {
    let cpu_max = overrides.cpu_max();
    let limits = if cpu_max.is_some()
        || overrides.memory_max_bytes().is_some()
        || overrides.pids_max().is_some()
        || overrides.wall_time_limit_ms().is_some()
    {
        Some(local::PartialResourceLimits {
            cpu_max: cpu_max.map(|cpu| local::CpuMax {
                quota_micros: cpu.quota_micros(),
                period_micros: cpu.period_micros(),
            }),
            memory_max_bytes: overrides.memory_max_bytes(),
            pids_max: overrides.pids_max(),
            wall_time_limit_ms: overrides.wall_time_limit_ms(),
        })
    } else {
        None
    };
    let output = if overrides.stdout_tail_max_bytes().is_some()
        || overrides.stderr_tail_max_bytes().is_some()
    {
        Some(local::PartialOutputLimits {
            stdout_tail_max_bytes: overrides.stdout_tail_max_bytes(),
            stderr_tail_max_bytes: overrides.stderr_tail_max_bytes(),
        })
    } else {
        None
    };
    local::ProfileResourceOverrides { limits, output }
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
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn maps_remote_managed_input_into_transport_neutral_profile_call() {
        let payload = remote::RemoteProfileRequestPayload {
            client_request_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            profile: remote::ProfileIdentity {
                name: "file-copy".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: BTreeMap::from([(
                "source".to_owned(),
                remote::RemoteProfileInputValue::ManagedInput {
                    artifact_id: "33333333-3333-4333-8333-333333333333".to_owned(),
                },
            )]),
            resource_overrides: None,
        };
        let snapshots = [ManagedInputSnapshot {
            artifact_id: "33333333-3333-4333-8333-333333333333".to_owned(),
            path: PathBuf::from("/private/task-input"),
            digest: format!("sha256:{}", "a".repeat(64)),
            size_bytes: 42,
            media_type: None,
        }];

        let call = profile_call(&payload, &snapshots).unwrap();

        assert_eq!(call.identity().name(), "file-copy");
        assert!(matches!(
            call.inputs().next(),
            Some((
                "source",
                ProfileValue::LocalInput {
                    path,
                    size_bytes: 42,
                    ..
                }
            )) if path == "remote-managed-input"
        ));
    }

    #[test]
    fn domain_bridge_preserves_every_remote_resource_override() {
        let payload = remote::RemoteProfileRequestPayload {
            client_request_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            profile: remote::ProfileIdentity {
                name: "file-copy".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: BTreeMap::new(),
            resource_overrides: Some(remote::ProfileResourceOverrides {
                limits: Some(remote::PartialResourceLimits {
                    cpu_max: Some(remote::CpuMax {
                        quota_micros: 50_000,
                        period_micros: 100_000,
                    }),
                    memory_max_bytes: Some(64 * 1024 * 1024),
                    pids_max: Some(8),
                    wall_time_limit_ms: Some(5_000),
                }),
                output: Some(remote::PartialOutputLimits {
                    stdout_tail_max_bytes: Some(1_024),
                    stderr_tail_max_bytes: Some(2_048),
                }),
            }),
        };

        let call = profile_call(&payload, &[]).unwrap();
        let local = local_profile_payload_from_domain(
            "33333333-3333-4333-8333-333333333333".to_owned(),
            call,
        );
        let overrides = local.resource_overrides.unwrap();
        let limits = overrides.limits.unwrap();
        let output = overrides.output.unwrap();

        assert_eq!(limits.cpu_max.unwrap().quota_micros, 50_000);
        assert_eq!(limits.memory_max_bytes, Some(64 * 1024 * 1024));
        assert_eq!(limits.pids_max, Some(8));
        assert_eq!(limits.wall_time_limit_ms, Some(5_000));
        assert_eq!(output.stdout_tail_max_bytes, Some(1_024));
        assert_eq!(output.stderr_tail_max_bytes, Some(2_048));
    }

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
