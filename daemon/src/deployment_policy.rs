//! 배포 단위가 허용하는 Task 하나의 최대 자원 예산을 검증한다.

use crate::protocol::{OutputLimits, ResourceLimits};
#[cfg(any(target_os = "linux", test))]
use crate::resource_budget::ResourceMaximumViolation;
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
    pub(crate) fn validate(
        &self,
        requested: &ResourceBudget,
    ) -> Result<(), ResourceMaximumViolation> {
        requested.validate_within_maximum(&self.maximum)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn maximum(&self) -> &ResourceBudget {
        &self.maximum
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
            Err(ResourceMaximumViolation::Cpu { .. })
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
                Err(ResourceMaximumViolation::Limit { field: actual, .. }) if actual == field
            ));
        }
    }
}
