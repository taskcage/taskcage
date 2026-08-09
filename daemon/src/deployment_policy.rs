//! 배포 단위가 허용하는 Task 하나의 최대 자원 예산을 검증한다.

#[cfg(any(target_os = "linux", test))]
use thiserror::Error;

use crate::protocol::{OutputLimits, ResourceLimits};
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};

#[derive(Debug, Clone)]
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) struct DeploymentResourcePolicy {
    maximum: ResourceBudget,
}

impl DeploymentResourcePolicy {
    pub(crate) fn try_new(
        maximum_limits: ResourceLimits,
        maximum_output: OutputLimits,
    ) -> Result<Self, ResourceBudgetError> {
        Ok(Self {
            maximum: ResourceBudget::try_from_protocol(maximum_limits, maximum_output)?,
        })
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn validate(&self, requested: &ResourceBudget) -> Result<(), PolicyViolation> {
        let actual = requested.cgroup_limits();
        let maximum = self.maximum.cgroup_limits();
        if !cpu_ratio_within_maximum(
            actual.cpu.quota_micros.get(),
            actual.cpu.period_micros.get(),
            maximum.cpu.quota_micros.get(),
            maximum.cpu.period_micros.get(),
        ) {
            return Err(PolicyViolation::Cpu {
                actual_quota: actual.cpu.quota_micros.get(),
                actual_period: actual.cpu.period_micros.get(),
                maximum_quota: maximum.cpu.quota_micros.get(),
                maximum_period: maximum.cpu.period_micros.get(),
            });
        }
        require_within(
            "limits.memoryMaxBytes",
            actual.memory_max_bytes.get(),
            maximum.memory_max_bytes.get(),
        )?;
        require_within(
            "limits.pidsMax",
            actual.max_processes.get(),
            maximum.max_processes.get(),
        )?;
        require_within(
            "limits.wallTimeLimitMs",
            requested.wall_time_limit_ms(),
            self.maximum.wall_time_limit_ms(),
        )?;
        require_within(
            "output.stdoutTailMaxBytes",
            requested.stdout_tail_max_bytes() as u64,
            self.maximum.stdout_tail_max_bytes() as u64,
        )?;
        require_within(
            "output.stderrTailMaxBytes",
            requested.stderr_tail_max_bytes() as u64,
            self.maximum.stderr_tail_max_bytes() as u64,
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::try_new(
            ResourceLimits {
                cpu_max: crate::protocol::CpuMax {
                    quota_micros: u64::MAX,
                    period_micros: 1,
                },
                memory_max_bytes: u64::MAX,
                pids_max: u64::MAX,
                wall_time_limit_ms: u64::MAX,
            },
            OutputLimits {
                stdout_tail_max_bytes: 65_536,
                stderr_tail_max_bytes: 65_536,
            },
        )
        .expect("test deployment policy must be valid")
    }
}

#[cfg(any(target_os = "linux", test))]
fn cpu_ratio_within_maximum(
    actual_quota: u64,
    actual_period: u64,
    maximum_quota: u64,
    maximum_period: u64,
) -> bool {
    u128::from(actual_quota) * u128::from(maximum_period)
        <= u128::from(maximum_quota) * u128::from(actual_period)
}

#[cfg(any(target_os = "linux", test))]
fn require_within(field: &'static str, actual: u64, maximum: u64) -> Result<(), PolicyViolation> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(PolicyViolation::Limit {
            field,
            actual,
            maximum,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[cfg(any(target_os = "linux", test))]
pub(crate) enum PolicyViolation {
    #[error(
        "limits.cpuMax 비율 {actual_quota}/{actual_period}이 deployment 최대 {maximum_quota}/{maximum_period}를 넘었습니다"
    )]
    Cpu {
        actual_quota: u64,
        actual_period: u64,
        maximum_quota: u64,
        maximum_period: u64,
    },
    #[error("{field} 값 {actual}이 deployment 최대 {maximum}을 넘었습니다")]
    Limit {
        field: &'static str,
        actual: u64,
        maximum: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CpuMax;

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

    fn output(stdout: u32, stderr: u32) -> OutputLimits {
        OutputLimits {
            stdout_tail_max_bytes: stdout,
            stderr_tail_max_bytes: stderr,
        }
    }

    fn policy() -> DeploymentResourcePolicy {
        DeploymentResourcePolicy::try_new(
            limits(200_000, 100_000, 2_147_483_648, 128, 900_000),
            output(65_536, 65_536),
        )
        .unwrap()
    }

    fn budget(limits: ResourceLimits, output: OutputLimits) -> ResourceBudget {
        ResourceBudget::try_from_protocol(limits, output).unwrap()
    }

    #[test]
    fn accepts_values_equal_to_every_maximum() {
        let requested = budget(
            limits(200_000, 100_000, 2_147_483_648, 128, 900_000),
            output(65_536, 65_536),
        );
        assert_eq!(policy().validate(&requested), Ok(()));
    }

    #[test]
    fn compares_cpu_as_an_exact_ratio_without_floating_point() {
        let equal_ratio = budget(limits(400_000, 200_000, 1, 1, 1), output(1, 1));
        assert_eq!(policy().validate(&equal_ratio), Ok(()));

        let above = budget(limits(400_001, 200_000, 1, 1, 1), output(1, 1));
        assert!(matches!(
            policy().validate(&above),
            Err(PolicyViolation::Cpu { .. })
        ));
    }

    #[test]
    fn rejects_each_non_cpu_value_above_its_maximum() {
        let cases = [
            (
                "limits.memoryMaxBytes",
                budget(limits(1, 1, 2_147_483_649, 1, 1), output(1, 1)),
            ),
            (
                "limits.pidsMax",
                budget(limits(1, 1, 1, 129, 1), output(1, 1)),
            ),
            (
                "limits.wallTimeLimitMs",
                budget(limits(1, 1, 1, 1, 900_001), output(1, 1)),
            ),
            (
                "output.stdoutTailMaxBytes",
                budget(limits(1, 1, 1, 1, 1), output(65_536, 1)),
            ),
            (
                "output.stderrTailMaxBytes",
                budget(limits(1, 1, 1, 1, 1), output(1, 65_536)),
            ),
        ];

        let lower_output_policy = DeploymentResourcePolicy::try_new(
            limits(200_000, 100_000, 2_147_483_648, 128, 900_000),
            output(65_535, 65_535),
        )
        .unwrap();
        for (field, requested) in cases {
            let selected = if field.starts_with("output.") {
                &lower_output_policy
            } else {
                &policy()
            };
            assert!(matches!(
                selected.validate(&requested),
                Err(PolicyViolation::Limit { field: actual, .. }) if actual == field
            ));
        }
    }
}
