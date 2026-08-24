//! Local wire DTO와 backend-independent domain 값 사이의 명시적 변환이다.

use taskcage_core::capsule::{
    CpuMaxOverride, ProfileCall, ProfileIdentity, ProfileResourceOverrides, ProfileValue,
};
use taskcage_core::task::{
    ProcessResult as DomainProcessResult, TaskOutput as DomainTaskOutput, TaskSnapshot,
    TaskTiming as DomainTaskTiming, TaskUsage as DomainTaskUsage,
    TerminationReason as DomainTerminationReason,
};

use crate::protocol::{
    CpuMax, OutputLimits, ProcessResult, ProfileInputValue, ProfileRequestPayload,
    ProfileResourceOverrides as WireProfileResourceOverrides, ResourceLimits, TaskOutput,
    TaskPayload, TaskTiming, TaskUsage, TerminationReason,
};
use crate::resource_budget::{ResourceBudget, ResourceBudgetError, VerifiedEffectiveLimits};

pub(crate) fn profile_call(request: &ProfileRequestPayload) -> ProfileCall {
    let mut call = ProfileCall::new(
        ProfileIdentity::new(
            request.profile.name.clone(),
            request.profile.version.clone(),
        ),
        request
            .inputs
            .iter()
            .map(|(name, value)| (name.clone(), profile_value(value))),
    );
    if let Some(overrides) = request.resource_overrides.as_ref() {
        call = call.with_resource_overrides(profile_resource_overrides(overrides));
    }
    call
}

fn profile_value(value: &ProfileInputValue) -> ProfileValue {
    match value {
        ProfileInputValue::String { value } => ProfileValue::String(value.clone()),
        ProfileInputValue::Int64 { value } => ProfileValue::Int64(*value),
        ProfileInputValue::Boolean { value } => ProfileValue::Boolean(*value),
        ProfileInputValue::LocalInput {
            path,
            digest,
            size_bytes,
        } => ProfileValue::LocalInput {
            path: path.clone(),
            digest: digest.clone(),
            size_bytes: *size_bytes,
        },
    }
}

fn profile_resource_overrides(
    overrides: &WireProfileResourceOverrides,
) -> ProfileResourceOverrides {
    let mut domain = ProfileResourceOverrides::new();
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

pub(crate) fn resource_budget(
    limits: &ResourceLimits,
    output: &OutputLimits,
) -> Result<ResourceBudget, ResourceBudgetError> {
    ResourceBudget::try_new(
        limits.cpu_max.quota_micros,
        limits.cpu_max.period_micros,
        limits.memory_max_bytes,
        limits.pids_max,
        limits.wall_time_limit_ms,
        output.stdout_tail_max_bytes,
        output.stderr_tail_max_bytes,
    )
}

pub(crate) fn resource_limits(budget: &ResourceBudget) -> ResourceLimits {
    resource_policy(budget.as_core().resources())
}

pub(crate) fn output_limits(budget: &ResourceBudget) -> OutputLimits {
    let output = budget.as_core().output();
    OutputLimits {
        stdout_tail_max_bytes: output.stdout_tail_max_bytes().get() as u32,
        stderr_tail_max_bytes: output.stderr_tail_max_bytes().get() as u32,
    }
}

pub(crate) fn verified_resource_limits(verified: VerifiedEffectiveLimits) -> ResourceLimits {
    resource_policy(verified.resources())
}

fn resource_policy(policy: taskcage_core::policy::ResourcePolicy) -> ResourceLimits {
    ResourceLimits {
        cpu_max: CpuMax {
            quota_micros: policy.cpu().quota_micros().get(),
            period_micros: policy.cpu().period_micros().get(),
        },
        memory_max_bytes: policy.memory_max_bytes().get(),
        pids_max: policy.pids_max().get(),
        wall_time_limit_ms: policy.wall_time_limit_ms().get(),
    }
}

pub(crate) fn task_snapshot(snapshot: TaskSnapshot) -> TaskPayload {
    match snapshot {
        TaskSnapshot::Running {
            task_id,
            submitted_at,
            started_at,
        } => TaskPayload::Running {
            task_id,
            submitted_at,
            started_at,
        },
        TaskSnapshot::Finished {
            task_id,
            termination_reason,
            process,
            timing,
            usage,
            output,
        } => TaskPayload::Finished {
            task_id,
            termination_reason: termination_reason_to_protocol(termination_reason),
            process: process_result(process),
            timing: task_timing(timing),
            usage: task_usage(usage),
            output: task_output(output),
        },
    }
}

#[cfg(test)]
pub(crate) fn task_snapshot_from_protocol(payload: &TaskPayload) -> TaskSnapshot {
    match payload {
        TaskPayload::Running {
            task_id,
            submitted_at,
            started_at,
        } => TaskSnapshot::Running {
            task_id: task_id.clone(),
            submitted_at: submitted_at.clone(),
            started_at: started_at.clone(),
        },
        TaskPayload::Finished {
            task_id,
            termination_reason,
            process,
            timing,
            usage,
            output,
        } => TaskSnapshot::Finished {
            task_id: task_id.clone(),
            termination_reason: termination_reason_from_protocol(*termination_reason),
            process: DomainProcessResult {
                exit_code: process.exit_code,
                signal: process.signal.clone(),
            },
            timing: DomainTaskTiming {
                submitted_at: timing.submitted_at.clone(),
                started_at: timing.started_at.clone(),
                finished_at: timing.finished_at.clone(),
                wall_time_ms: timing.wall_time_ms,
            },
            usage: DomainTaskUsage {
                cpu_time_micros: usage.cpu_time_micros,
                memory_peak_bytes: usage.memory_peak_bytes,
            },
            output: DomainTaskOutput {
                stdout_tail: output.stdout_tail.clone(),
                stderr_tail: output.stderr_tail.clone(),
                stdout_truncated: output.stdout_truncated,
                stderr_truncated: output.stderr_truncated,
            },
        },
    }
}

#[cfg(test)]
const fn termination_reason_from_protocol(reason: TerminationReason) -> DomainTerminationReason {
    match reason {
        TerminationReason::Exited => DomainTerminationReason::Exited,
        TerminationReason::ExecutionFailed => DomainTerminationReason::ExecutionFailed,
        TerminationReason::Cancelled => DomainTerminationReason::Cancelled,
        TerminationReason::TimedOut => DomainTerminationReason::TimedOut,
        TerminationReason::MemoryLimitExceeded => DomainTerminationReason::MemoryLimitExceeded,
        TerminationReason::ProcessLimitExceeded => DomainTerminationReason::ProcessLimitExceeded,
        TerminationReason::DaemonError => DomainTerminationReason::DaemonError,
    }
}

pub(crate) const fn termination_reason_to_protocol(
    reason: DomainTerminationReason,
) -> TerminationReason {
    match reason {
        DomainTerminationReason::Exited => TerminationReason::Exited,
        DomainTerminationReason::ExecutionFailed => TerminationReason::ExecutionFailed,
        DomainTerminationReason::Cancelled => TerminationReason::Cancelled,
        DomainTerminationReason::TimedOut => TerminationReason::TimedOut,
        DomainTerminationReason::MemoryLimitExceeded => TerminationReason::MemoryLimitExceeded,
        DomainTerminationReason::ProcessLimitExceeded => TerminationReason::ProcessLimitExceeded,
        DomainTerminationReason::DaemonError => TerminationReason::DaemonError,
    }
}

pub(crate) fn process_result(process: DomainProcessResult) -> ProcessResult {
    ProcessResult {
        exit_code: process.exit_code,
        signal: process.signal,
    }
}

pub(crate) fn task_timing(timing: DomainTaskTiming) -> TaskTiming {
    TaskTiming {
        submitted_at: timing.submitted_at,
        started_at: timing.started_at,
        finished_at: timing.finished_at,
        wall_time_ms: timing.wall_time_ms,
    }
}

pub(crate) fn task_usage(usage: DomainTaskUsage) -> TaskUsage {
    TaskUsage {
        cpu_time_micros: usage.cpu_time_micros,
        memory_peak_bytes: usage.memory_peak_bytes,
    }
}

pub(crate) fn task_output(output: DomainTaskOutput) -> TaskOutput {
    TaskOutput {
        stdout_tail: output.stdout_tail,
        stderr_tail: output.stderr_tail,
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::protocol::{ProfileIdentity as WireProfileIdentity, ProfileInputValue};

    #[test]
    fn maps_profile_dto_without_leaking_wire_types_into_the_verifier() {
        let request = ProfileRequestPayload {
            client_request_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            profile: WireProfileIdentity {
                name: "file-copy".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: BTreeMap::from([(
                "priority".to_owned(),
                ProfileInputValue::Int64 { value: 50 },
            )]),
            resource_overrides: None,
        };

        let call = profile_call(&request);
        assert_eq!(call.identity().name(), "file-copy");
        assert!(matches!(
            call.inputs().next(),
            Some(("priority", ProfileValue::Int64(50)))
        ));
    }
}
