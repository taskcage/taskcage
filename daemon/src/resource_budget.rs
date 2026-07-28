//! protocol v1 자원 예산을 실행 코어가 사용하는 검증된 값으로 바꾼다.

use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use thiserror::Error;

use crate::cgroup::{CgroupLimits, CpuLimit, VerifiedCgroupLimits};
use crate::output::CaptureLimits;
use crate::protocol::{OutputLimits, ResourceLimits};

const MAX_OUTPUT_TAIL_BYTES: u32 = 65_536;
const MAX_TOTAL_OUTPUT_BYTES: u32 = 131_072;

#[derive(Debug, Clone)]
pub struct ResourceBudget {
    cgroup_limits: CgroupLimits,
    wall_timeout: Duration,
    wall_time_limit_ms: NonZeroU64,
    stdout_tail_max_bytes: NonZeroUsize,
    stderr_tail_max_bytes: NonZeroUsize,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
/// cgroup 제한의 write/read-back이 모두 끝난 뒤 taskAccepted에 사용할 수 있는 값이다.
pub(crate) struct VerifiedEffectiveLimits {
    limits: ResourceLimits,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
impl VerifiedEffectiveLimits {
    pub(crate) fn into_protocol(self) -> ResourceLimits {
        self.limits
    }

    #[cfg(test)]
    pub(crate) fn for_test(limits: ResourceLimits) -> Self {
        Self { limits }
    }
}

impl ResourceBudget {
    pub fn try_from_protocol(
        limits: ResourceLimits,
        output: OutputLimits,
    ) -> Result<Self, ResourceBudgetError> {
        require_output_nonzero("output.stdoutTailMaxBytes", output.stdout_tail_max_bytes)?;
        require_output_nonzero("output.stderrTailMaxBytes", output.stderr_tail_max_bytes)?;

        let output_total = output
            .stdout_tail_max_bytes
            .checked_add(output.stderr_tail_max_bytes)
            .ok_or(ResourceBudgetError::OutputTotalOverflow {
                stdout: output.stdout_tail_max_bytes,
                stderr: output.stderr_tail_max_bytes,
            })?;
        if output_total > MAX_TOTAL_OUTPUT_BYTES {
            return Err(ResourceBudgetError::OutputTotalTooLarge {
                actual: output_total,
                maximum: MAX_TOTAL_OUTPUT_BYTES,
            });
        }

        let stdout_tail_max_bytes =
            output_tail_bytes("output.stdoutTailMaxBytes", output.stdout_tail_max_bytes)?;
        let stderr_tail_max_bytes =
            output_tail_bytes("output.stderrTailMaxBytes", output.stderr_tail_max_bytes)?;

        let cpu_quota = nonzero_u64("limits.cpuMax.quotaMicros", limits.cpu_max.quota_micros)?;
        let cpu_period = nonzero_u64("limits.cpuMax.periodMicros", limits.cpu_max.period_micros)?;
        let memory_max_bytes = nonzero_u64("limits.memoryMaxBytes", limits.memory_max_bytes)?;
        let max_processes = nonzero_u64("limits.pidsMax", limits.pids_max)?;
        let wall_time_limit_ms = nonzero_u64("limits.wallTimeLimitMs", limits.wall_time_limit_ms)?;

        Ok(Self {
            cgroup_limits: CgroupLimits {
                memory_max_bytes,
                max_processes,
                cpu: CpuLimit {
                    quota_micros: cpu_quota,
                    period_micros: cpu_period,
                },
            },
            wall_timeout: Duration::from_millis(wall_time_limit_ms.get()),
            wall_time_limit_ms,
            stdout_tail_max_bytes,
            stderr_tail_max_bytes,
        })
    }

    pub fn cgroup_limits(&self) -> CgroupLimits {
        self.cgroup_limits
    }

    pub fn wall_timeout(&self) -> Duration {
        self.wall_timeout
    }

    pub fn stdout_tail_max_bytes(&self) -> usize {
        self.stdout_tail_max_bytes.get()
    }

    pub fn stderr_tail_max_bytes(&self) -> usize {
        self.stderr_tail_max_bytes.get()
    }

    pub fn capture_limits(&self) -> CaptureLimits {
        CaptureLimits::new(self.stdout_tail_max_bytes, self.stderr_tail_max_bytes)
    }

    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    pub(crate) fn verified_effective_limits(
        &self,
        verified: VerifiedCgroupLimits,
    ) -> VerifiedEffectiveLimits {
        let limits = verified.limits();
        VerifiedEffectiveLimits {
            limits: ResourceLimits {
                cpu_max: crate::protocol::CpuMax {
                    quota_micros: limits.cpu.quota_micros.get(),
                    period_micros: limits.cpu.period_micros.get(),
                },
                memory_max_bytes: limits.memory_max_bytes.get(),
                pids_max: limits.max_processes.get(),
                wall_time_limit_ms: self.wall_time_limit_ms.get(),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn verified_effective_limits_for_test(&self) -> VerifiedEffectiveLimits {
        self.verified_effective_limits(VerifiedCgroupLimits::for_test(self.cgroup_limits))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResourceBudgetError {
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

fn nonzero_u64(field: &'static str, value: u64) -> Result<NonZeroU64, ResourceBudgetError> {
    NonZeroU64::new(value).ok_or(ResourceBudgetError::Zero { field })
}

fn require_output_nonzero(field: &'static str, value: u32) -> Result<(), ResourceBudgetError> {
    if value == 0 {
        Err(ResourceBudgetError::Zero { field })
    } else {
        Ok(())
    }
}

fn output_tail_bytes(field: &'static str, value: u32) -> Result<NonZeroUsize, ResourceBudgetError> {
    if value > MAX_OUTPUT_TAIL_BYTES {
        return Err(ResourceBudgetError::OutputTailTooLarge {
            field,
            actual: value,
            maximum: MAX_OUTPUT_TAIL_BYTES,
        });
    }
    let value = checked_output_size::<usize>(field, value)?;
    NonZeroUsize::new(value).ok_or(ResourceBudgetError::Zero { field })
}

fn checked_output_size<T>(field: &'static str, value: u32) -> Result<T, ResourceBudgetError>
where
    T: TryFrom<u32>,
{
    T::try_from(value).map_err(|_| ResourceBudgetError::NotRepresentable {
        field,
        value: u64::from(value),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::protocol::CpuMax;

    use super::*;

    fn limits(
        quota_micros: u64,
        period_micros: u64,
        memory_max_bytes: u64,
        pids_max: u64,
        wall_time_limit_ms: u64,
    ) -> ResourceLimits {
        ResourceLimits {
            cpu_max: CpuMax {
                quota_micros,
                period_micros,
            },
            memory_max_bytes,
            pids_max,
            wall_time_limit_ms,
        }
    }

    fn output(stdout_tail_max_bytes: u32, stderr_tail_max_bytes: u32) -> OutputLimits {
        OutputLimits {
            stdout_tail_max_bytes,
            stderr_tail_max_bytes,
        }
    }

    #[test]
    fn accepts_minimum_positive_values_without_defaults() {
        let wire_limits = limits(1, 1, 1, 1, 1);
        let budget = ResourceBudget::try_from_protocol(wire_limits.clone(), output(1, 1)).unwrap();
        let cgroup = budget.cgroup_limits();

        assert_eq!(cgroup.cpu.quota_micros.get(), 1);
        assert_eq!(cgroup.cpu.period_micros.get(), 1);
        assert_eq!(cgroup.memory_max_bytes.get(), 1);
        assert_eq!(cgroup.max_processes.get(), 1);
        assert_eq!(budget.wall_timeout(), Duration::from_millis(1));
        assert_eq!(budget.stdout_tail_max_bytes(), 1);
        assert_eq!(budget.stderr_tail_max_bytes(), 1);
        assert_eq!(
            budget.verified_effective_limits_for_test().into_protocol(),
            wire_limits
        );
    }

    #[test]
    fn accepts_documented_output_maxima_and_total() {
        let budget = ResourceBudget::try_from_protocol(
            limits(1, 1, 1, 1, 1),
            output(MAX_OUTPUT_TAIL_BYTES, MAX_OUTPUT_TAIL_BYTES),
        )
        .unwrap();

        assert_eq!(
            budget.stdout_tail_max_bytes(),
            usize::try_from(MAX_OUTPUT_TAIL_BYTES).unwrap()
        );
        assert_eq!(
            budget.stderr_tail_max_bytes(),
            usize::try_from(MAX_OUTPUT_TAIL_BYTES).unwrap()
        );
    }

    #[test]
    fn rejects_each_zero_resource_value() {
        let cases = [
            ("limits.cpuMax.quotaMicros", limits(0, 1, 1, 1, 1)),
            ("limits.cpuMax.periodMicros", limits(1, 0, 1, 1, 1)),
            ("limits.memoryMaxBytes", limits(1, 1, 0, 1, 1)),
            ("limits.pidsMax", limits(1, 1, 1, 0, 1)),
            ("limits.wallTimeLimitMs", limits(1, 1, 1, 1, 0)),
        ];

        for (field, limits) in cases {
            let error = ResourceBudget::try_from_protocol(limits, output(1, 1)).unwrap_err();
            assert_eq!(error, ResourceBudgetError::Zero { field });
        }
    }

    #[test]
    fn rejects_zero_output_values() {
        for (field, output) in [
            ("output.stdoutTailMaxBytes", output(0, 1)),
            ("output.stderrTailMaxBytes", output(1, 0)),
        ] {
            let error =
                ResourceBudget::try_from_protocol(limits(1, 1, 1, 1, 1), output).unwrap_err();
            assert_eq!(error, ResourceBudgetError::Zero { field });
        }
    }

    #[test]
    fn rejects_output_stream_above_documented_limit() {
        for (field, output) in [
            ("output.stdoutTailMaxBytes", output(65_537, 1)),
            ("output.stderrTailMaxBytes", output(1, 65_537)),
        ] {
            let error =
                ResourceBudget::try_from_protocol(limits(1, 1, 1, 1, 1), output).unwrap_err();
            assert_eq!(
                error,
                ResourceBudgetError::OutputTailTooLarge {
                    field,
                    actual: 65_537,
                    maximum: MAX_OUTPUT_TAIL_BYTES,
                }
            );
        }
    }

    #[test]
    fn rejects_output_total_above_documented_limit() {
        let error =
            ResourceBudget::try_from_protocol(limits(1, 1, 1, 1, 1), output(65_536, 65_537))
                .unwrap_err();
        assert_eq!(
            error,
            ResourceBudgetError::OutputTotalTooLarge {
                actual: 131_073,
                maximum: MAX_TOTAL_OUTPUT_BYTES,
            }
        );
    }

    #[test]
    fn rejects_output_total_arithmetic_overflow() {
        let error = ResourceBudget::try_from_protocol(limits(1, 1, 1, 1, 1), output(u32::MAX, 1))
            .unwrap_err();
        assert_eq!(
            error,
            ResourceBudgetError::OutputTotalOverflow {
                stdout: u32::MAX,
                stderr: 1,
            }
        );
    }

    #[test]
    fn checked_conversion_rejects_unrepresentable_internal_value() {
        assert_eq!(
            checked_output_size::<u16>("output.stdoutTailMaxBytes", 65_536),
            Err(ResourceBudgetError::NotRepresentable {
                field: "output.stdoutTailMaxBytes",
                value: 65_536,
            })
        );
    }

    #[test]
    fn preserves_large_protocol_values_without_adding_policy_maxima() {
        let wire_limits = limits(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        let budget = ResourceBudget::try_from_protocol(wire_limits.clone(), output(1, 1)).unwrap();
        let cgroup = budget.cgroup_limits();

        assert_eq!(cgroup.cpu.quota_micros.get(), u64::MAX);
        assert_eq!(cgroup.cpu.period_micros.get(), u64::MAX);
        assert_eq!(cgroup.memory_max_bytes.get(), u64::MAX);
        assert_eq!(cgroup.max_processes.get(), u64::MAX);
        assert_eq!(budget.wall_timeout(), Duration::from_millis(u64::MAX));
        assert_eq!(
            budget.verified_effective_limits_for_test().into_protocol(),
            wire_limits
        );
    }

    #[test]
    fn validation_failure_happens_before_cgroup_or_target_side_effects() {
        let cgroup_creations = Cell::new(0);
        let target_starts = Cell::new(0);

        let result =
            ResourceBudget::try_from_protocol(limits(0, 1, 1, 1, 1), output(1, 1)).map(|_| {
                cgroup_creations.set(cgroup_creations.get() + 1);
                target_starts.set(target_starts.get() + 1);
            });

        assert!(result.is_err());
        assert_eq!(cgroup_creations.get(), 0);
        assert_eq!(target_starts.get(), 0);
    }

    #[test]
    fn preserves_wire_values_instead_of_using_cli_values() {
        let wire_limits = limits(12_345, 67_890, 98_765, 43_210, 54_321);
        let budget =
            ResourceBudget::try_from_protocol(wire_limits.clone(), output(123, 456)).unwrap();
        let cgroup = budget.cgroup_limits();

        assert_eq!(cgroup.cpu.quota_micros.get(), 12_345);
        assert_eq!(cgroup.cpu.period_micros.get(), 67_890);
        assert_eq!(cgroup.memory_max_bytes.get(), 98_765);
        assert_eq!(cgroup.max_processes.get(), 43_210);
        assert_eq!(budget.wall_timeout(), Duration::from_millis(54_321));
        assert_eq!(budget.stdout_tail_max_bytes(), 123);
        assert_eq!(budget.stderr_tail_max_bytes(), 456);
        assert_eq!(
            budget.verified_effective_limits_for_test().into_protocol(),
            wire_limits
        );
    }
}
