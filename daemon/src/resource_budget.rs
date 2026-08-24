//! Backend-independent policy를 daemon의 cgroup adapter에 연결한다.

use taskcage_core::policy::{OutputPolicy, ResourceBudget as CoreResourceBudget, ResourcePolicy};

use crate::cgroup::{CgroupLimits, CpuLimit, VerifiedCgroupLimits};

pub use taskcage_core::policy::PolicyError as ResourceBudgetError;
pub(crate) use taskcage_core::policy::ResourceMaximumViolation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBudget(CoreResourceBudget);

impl ResourceBudget {
    /// Compatibility entry point; wire conversion is centralized in `protocol_mapper`.
    pub fn try_from_protocol(
        limits: crate::protocol::ResourceLimits,
        output: crate::protocol::OutputLimits,
    ) -> Result<Self, ResourceBudgetError> {
        crate::protocol_mapper::resource_budget(&limits, &output)
    }

    pub fn try_new(
        cpu_quota_micros: u64,
        cpu_period_micros: u64,
        memory_max_bytes: u64,
        pids_max: u64,
        wall_time_limit_ms: u64,
        stdout_tail_max_bytes: u32,
        stderr_tail_max_bytes: u32,
    ) -> Result<Self, ResourceBudgetError> {
        Ok(Self(CoreResourceBudget::new(
            ResourcePolicy::try_new(
                cpu_quota_micros,
                cpu_period_micros,
                memory_max_bytes,
                pids_max,
                wall_time_limit_ms,
            )?,
            OutputPolicy::try_new(stdout_tail_max_bytes, stderr_tail_max_bytes)?,
        )))
    }

    pub(crate) const fn as_core(&self) -> &CoreResourceBudget {
        &self.0
    }

    pub fn cgroup_limits(&self) -> CgroupLimits {
        let resources = self.0.resources();
        CgroupLimits {
            memory_max_bytes: resources.memory_max_bytes(),
            max_processes: resources.pids_max(),
            cpu: CpuLimit {
                quota_micros: resources.cpu().quota_micros(),
                period_micros: resources.cpu().period_micros(),
            },
        }
    }

    pub fn wall_timeout(&self) -> std::time::Duration {
        self.0.wall_timeout()
    }

    pub fn stdout_tail_max_bytes(&self) -> usize {
        self.0.stdout_tail_max_bytes()
    }

    pub fn stderr_tail_max_bytes(&self) -> usize {
        self.0.stderr_tail_max_bytes()
    }

    pub fn capture_limits(&self) -> crate::output::CaptureLimits {
        self.0.capture_limits()
    }

    #[cfg(test)]
    pub(crate) fn protocol_limits(&self) -> crate::protocol::ResourceLimits {
        crate::protocol_mapper::resource_limits(self)
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn protocol_output(&self) -> crate::protocol::OutputLimits {
        crate::protocol_mapper::output_limits(self)
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn validate_within_maximum(
        &self,
        maximum: &Self,
    ) -> Result<(), ResourceMaximumViolation> {
        self.0.validate_within_maximum(&maximum.0)
    }

    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    pub(crate) fn verified_effective_limits(
        &self,
        verified: VerifiedCgroupLimits,
    ) -> VerifiedEffectiveLimits {
        let applied = verified.limits();
        let requested = self.0.resources();
        let resources = ResourcePolicy::try_new(
            applied.cpu.quota_micros.get(),
            applied.cpu.period_micros.get(),
            applied.memory_max_bytes.get(),
            applied.max_processes.get(),
            requested.wall_time_limit_ms().get(),
        )
        .expect("verified cgroup values and validated wall time remain positive");
        VerifiedEffectiveLimits { resources }
    }

    #[cfg(test)]
    pub(crate) fn verified_effective_limits_for_test(&self) -> VerifiedEffectiveLimits {
        self.verified_effective_limits(VerifiedCgroupLimits::for_test(self.cgroup_limits()))
    }
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
/// cgroup 제한의 write/read-back이 모두 끝난 뒤 taskAccepted에 사용할 수 있는 값이다.
pub(crate) struct VerifiedEffectiveLimits {
    resources: ResourcePolicy,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
impl VerifiedEffectiveLimits {
    pub(crate) fn into_protocol(self) -> crate::protocol::ResourceLimits {
        crate::protocol_mapper::verified_resource_limits(self)
    }

    pub(crate) const fn resources(&self) -> ResourcePolicy {
        self.resources
    }

    #[cfg(test)]
    pub(crate) fn for_test(limits: crate::protocol::ResourceLimits) -> Self {
        let resources = ResourcePolicy::try_new(
            limits.cpu_max.quota_micros,
            limits.cpu_max.period_micros,
            limits.memory_max_bytes,
            limits.pids_max,
            limits.wall_time_limit_ms,
        )
        .expect("test effective resource DTO must contain positive values");
        Self { resources }
    }
}
