//! Backend-independent resource and bounded-output policies.

use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use thiserror::Error;

use crate::output::CaptureLimits;

pub const MAX_OUTPUT_TAIL_BYTES: u32 = 65_536;
pub const MAX_TOTAL_OUTPUT_BYTES: u32 = 131_072;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuPolicy {
    quota_micros: NonZeroU64,
    period_micros: NonZeroU64,
}

impl CpuPolicy {
    pub fn try_new(quota_micros: u64, period_micros: u64) -> Result<Self, PolicyError> {
        Ok(Self {
            quota_micros: nonzero_u64("limits.cpuMax.quotaMicros", quota_micros)?,
            period_micros: nonzero_u64("limits.cpuMax.periodMicros", period_micros)?,
        })
    }

    pub const fn quota_micros(self) -> NonZeroU64 {
        self.quota_micros
    }

    pub const fn period_micros(self) -> NonZeroU64 {
        self.period_micros
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourcePolicy {
    cpu: CpuPolicy,
    memory_max_bytes: NonZeroU64,
    pids_max: NonZeroU64,
    wall_time_limit_ms: NonZeroU64,
}

impl ResourcePolicy {
    pub fn try_new(
        cpu_quota_micros: u64,
        cpu_period_micros: u64,
        memory_max_bytes: u64,
        pids_max: u64,
        wall_time_limit_ms: u64,
    ) -> Result<Self, PolicyError> {
        Ok(Self {
            cpu: CpuPolicy::try_new(cpu_quota_micros, cpu_period_micros)?,
            memory_max_bytes: nonzero_u64("limits.memoryMaxBytes", memory_max_bytes)?,
            pids_max: nonzero_u64("limits.pidsMax", pids_max)?,
            wall_time_limit_ms: nonzero_u64("limits.wallTimeLimitMs", wall_time_limit_ms)?,
        })
    }

    pub const fn cpu(self) -> CpuPolicy {
        self.cpu
    }

    pub const fn memory_max_bytes(self) -> NonZeroU64 {
        self.memory_max_bytes
    }

    pub const fn pids_max(self) -> NonZeroU64 {
        self.pids_max
    }

    pub const fn wall_time_limit_ms(self) -> NonZeroU64 {
        self.wall_time_limit_ms
    }

    pub fn wall_timeout(self) -> Duration {
        Duration::from_millis(self.wall_time_limit_ms.get())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputPolicy {
    stdout_tail_max_bytes: NonZeroUsize,
    stderr_tail_max_bytes: NonZeroUsize,
}

impl OutputPolicy {
    pub fn try_new(
        stdout_tail_max_bytes: u32,
        stderr_tail_max_bytes: u32,
    ) -> Result<Self, PolicyError> {
        require_output_nonzero("output.stdoutTailMaxBytes", stdout_tail_max_bytes)?;
        require_output_nonzero("output.stderrTailMaxBytes", stderr_tail_max_bytes)?;
        let output_total = stdout_tail_max_bytes
            .checked_add(stderr_tail_max_bytes)
            .ok_or(PolicyError::OutputTotalOverflow {
                stdout: stdout_tail_max_bytes,
                stderr: stderr_tail_max_bytes,
            })?;
        if output_total > MAX_TOTAL_OUTPUT_BYTES {
            return Err(PolicyError::OutputTotalTooLarge {
                actual: output_total,
                maximum: MAX_TOTAL_OUTPUT_BYTES,
            });
        }
        Ok(Self {
            stdout_tail_max_bytes: output_tail_bytes(
                "output.stdoutTailMaxBytes",
                stdout_tail_max_bytes,
            )?,
            stderr_tail_max_bytes: output_tail_bytes(
                "output.stderrTailMaxBytes",
                stderr_tail_max_bytes,
            )?,
        })
    }

    pub const fn stdout_tail_max_bytes(self) -> NonZeroUsize {
        self.stdout_tail_max_bytes
    }

    pub const fn stderr_tail_max_bytes(self) -> NonZeroUsize {
        self.stderr_tail_max_bytes
    }

    pub fn capture_limits(self) -> CaptureLimits {
        CaptureLimits::new(self.stdout_tail_max_bytes, self.stderr_tail_max_bytes)
    }
}

/// Validated resource and output policy for one Task execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBudget {
    resources: ResourcePolicy,
    output: OutputPolicy,
}

impl ResourceBudget {
    pub const fn new(resources: ResourcePolicy, output: OutputPolicy) -> Self {
        Self { resources, output }
    }

    pub const fn resources(&self) -> ResourcePolicy {
        self.resources
    }

    pub const fn output(&self) -> OutputPolicy {
        self.output
    }

    pub fn wall_timeout(&self) -> Duration {
        self.resources.wall_timeout()
    }

    pub fn stdout_tail_max_bytes(&self) -> usize {
        self.output.stdout_tail_max_bytes().get()
    }

    pub fn stderr_tail_max_bytes(&self) -> usize {
        self.output.stderr_tail_max_bytes().get()
    }

    pub fn capture_limits(&self) -> CaptureLimits {
        self.output.capture_limits()
    }

    pub fn validate_within_maximum(
        &self,
        maximum_budget: &Self,
    ) -> Result<(), ResourceMaximumViolation> {
        let actual = self.resources;
        let maximum = maximum_budget.resources;
        if u128::from(actual.cpu.quota_micros.get()) * u128::from(maximum.cpu.period_micros.get())
            > u128::from(maximum.cpu.quota_micros.get())
                * u128::from(actual.cpu.period_micros.get())
        {
            return Err(ResourceMaximumViolation::Cpu {
                actual_quota: actual.cpu.quota_micros.get(),
                actual_period: actual.cpu.period_micros.get(),
                maximum_quota: maximum.cpu.quota_micros.get(),
                maximum_period: maximum.cpu.period_micros.get(),
            });
        }
        require_within_maximum(
            "limits.memoryMaxBytes",
            actual.memory_max_bytes.get(),
            maximum.memory_max_bytes.get(),
        )?;
        require_within_maximum(
            "limits.pidsMax",
            actual.pids_max.get(),
            maximum.pids_max.get(),
        )?;
        require_within_maximum(
            "limits.wallTimeLimitMs",
            actual.wall_time_limit_ms.get(),
            maximum.wall_time_limit_ms.get(),
        )?;
        require_within_maximum(
            "output.stdoutTailMaxBytes",
            self.stdout_tail_max_bytes() as u64,
            maximum_budget.output.stdout_tail_max_bytes.get() as u64,
        )?;
        require_within_maximum(
            "output.stderrTailMaxBytes",
            self.stderr_tail_max_bytes() as u64,
            maximum_budget.output.stderr_tail_max_bytes.get() as u64,
        )?;
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("{field} 값은 0보다 커야 합니다")]
    Zero { field: &'static str },
    #[error("{field} 값 {actual}이 문서 상한 {maximum}을 넘었습니다")]
    OutputTailTooLarge {
        field: &'static str,
        actual: u32,
        maximum: u32,
    },
    #[error("stdout과 stderr 출력 상한의 합을 계산할 수 없습니다: {stdout} + {stderr}")]
    OutputTotalOverflow { stdout: u32, stderr: u32 },
    #[error("stdout과 stderr 출력 상한의 합 {actual}이 문서 상한 {maximum}을 넘었습니다")]
    OutputTotalTooLarge { actual: u32, maximum: u32 },
    #[error("{field} 값 {value}을 내부 타입으로 정확히 표현할 수 없습니다")]
    NotRepresentable { field: &'static str, value: u64 },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResourceMaximumViolation {
    #[error(
        "limits.cpuMax 비율 {actual_quota}/{actual_period}이 최대 {maximum_quota}/{maximum_period}를 넘었습니다"
    )]
    Cpu {
        actual_quota: u64,
        actual_period: u64,
        maximum_quota: u64,
        maximum_period: u64,
    },
    #[error("{field} 값 {actual}이 최대 {maximum}을 넘었습니다")]
    Limit {
        field: &'static str,
        actual: u64,
        maximum: u64,
    },
}

fn nonzero_u64(field: &'static str, value: u64) -> Result<NonZeroU64, PolicyError> {
    NonZeroU64::new(value).ok_or(PolicyError::Zero { field })
}

fn require_output_nonzero(field: &'static str, value: u32) -> Result<(), PolicyError> {
    if value == 0 {
        Err(PolicyError::Zero { field })
    } else {
        Ok(())
    }
}

fn output_tail_bytes(field: &'static str, value: u32) -> Result<NonZeroUsize, PolicyError> {
    if value > MAX_OUTPUT_TAIL_BYTES {
        return Err(PolicyError::OutputTailTooLarge {
            field,
            actual: value,
            maximum: MAX_OUTPUT_TAIL_BYTES,
        });
    }
    let value = checked_output_size::<usize>(field, value)?;
    NonZeroUsize::new(value).ok_or(PolicyError::Zero { field })
}

fn require_within_maximum(
    field: &'static str,
    actual: u64,
    maximum: u64,
) -> Result<(), ResourceMaximumViolation> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(ResourceMaximumViolation::Limit {
            field,
            actual,
            maximum,
        })
    }
}

fn checked_output_size<T>(field: &'static str, value: u32) -> Result<T, PolicyError>
where
    T: TryFrom<u32>,
{
    T::try_from(value).map_err(|_| PolicyError::NotRepresentable {
        field,
        value: u64::from(value),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn budget(
        quota: u64,
        period: u64,
        memory: u64,
        pids: u64,
        wall: u64,
        stdout: u32,
        stderr: u32,
    ) -> Result<ResourceBudget, PolicyError> {
        Ok(ResourceBudget::new(
            ResourcePolicy::try_new(quota, period, memory, pids, wall)?,
            OutputPolicy::try_new(stdout, stderr)?,
        ))
    }

    #[test]
    fn validates_positive_resource_and_output_policy() {
        let budget = budget(1, 1, 1, 1, 1, 1, 1).unwrap();
        assert_eq!(budget.resources().cpu().quota_micros().get(), 1);
        assert_eq!(budget.wall_timeout(), Duration::from_millis(1));
        assert_eq!(budget.stdout_tail_max_bytes(), 1);
    }

    #[test]
    fn rejects_zero_and_oversized_output_policy() {
        assert_eq!(
            OutputPolicy::try_new(0, 1),
            Err(PolicyError::Zero {
                field: "output.stdoutTailMaxBytes"
            })
        );
        assert!(matches!(
            OutputPolicy::try_new(65_537, 1),
            Err(PolicyError::OutputTailTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_each_zero_resource_value() {
        for (field, values) in [
            ("limits.cpuMax.quotaMicros", [0, 1, 1, 1, 1]),
            ("limits.cpuMax.periodMicros", [1, 0, 1, 1, 1]),
            ("limits.memoryMaxBytes", [1, 1, 0, 1, 1]),
            ("limits.pidsMax", [1, 1, 1, 0, 1]),
            ("limits.wallTimeLimitMs", [1, 1, 1, 1, 0]),
        ] {
            let [quota, period, memory, pids, wall] = values;
            assert_eq!(
                ResourcePolicy::try_new(quota, period, memory, pids, wall),
                Err(PolicyError::Zero { field })
            );
        }
    }

    #[test]
    fn accepts_documented_output_maxima_and_rejects_total_overflow() {
        let maximum = OutputPolicy::try_new(MAX_OUTPUT_TAIL_BYTES, MAX_OUTPUT_TAIL_BYTES).unwrap();
        assert_eq!(
            maximum.stdout_tail_max_bytes().get(),
            usize::try_from(MAX_OUTPUT_TAIL_BYTES).unwrap()
        );
        assert_eq!(
            OutputPolicy::try_new(65_536, 65_537),
            Err(PolicyError::OutputTotalTooLarge {
                actual: 131_073,
                maximum: MAX_TOTAL_OUTPUT_BYTES,
            })
        );
        assert_eq!(
            OutputPolicy::try_new(u32::MAX, 1),
            Err(PolicyError::OutputTotalOverflow {
                stdout: u32::MAX,
                stderr: 1,
            })
        );
    }

    #[test]
    fn preserves_large_positive_resource_values() {
        let resources =
            ResourcePolicy::try_new(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX).unwrap();
        assert_eq!(resources.cpu().quota_micros().get(), u64::MAX);
        assert_eq!(resources.cpu().period_micros().get(), u64::MAX);
        assert_eq!(resources.memory_max_bytes().get(), u64::MAX);
        assert_eq!(resources.pids_max().get(), u64::MAX);
        assert_eq!(resources.wall_time_limit_ms().get(), u64::MAX);
        assert_eq!(resources.wall_timeout(), Duration::from_millis(u64::MAX));
    }

    #[test]
    fn validation_failure_precedes_adapter_side_effects() {
        let cgroup_creations = Cell::new(0);
        let target_starts = Cell::new(0);
        let result = ResourcePolicy::try_new(0, 1, 1, 1, 1).map(|_| {
            cgroup_creations.set(cgroup_creations.get() + 1);
            target_starts.set(target_starts.get() + 1);
        });

        assert!(result.is_err());
        assert_eq!(cgroup_creations.get(), 0);
        assert_eq!(target_starts.get(), 0);
    }

    #[test]
    fn checked_output_conversion_rejects_unrepresentable_values() {
        assert_eq!(
            checked_output_size::<u16>("output.stdoutTailMaxBytes", 65_536),
            Err(PolicyError::NotRepresentable {
                field: "output.stdoutTailMaxBytes",
                value: 65_536,
            })
        );
    }

    #[test]
    fn compares_cpu_as_an_exact_ratio() {
        let maximum = budget(100_000, 100_000, 100, 10, 100, 10, 10).unwrap();
        let allowed = budget(50_000, 100_000, 100, 10, 100, 10, 10).unwrap();
        let rejected = budget(200_000, 100_000, 100, 10, 100, 10, 10).unwrap();
        assert!(allowed.validate_within_maximum(&maximum).is_ok());
        assert!(matches!(
            rejected.validate_within_maximum(&maximum),
            Err(ResourceMaximumViolation::Cpu { .. })
        ));
    }
}
