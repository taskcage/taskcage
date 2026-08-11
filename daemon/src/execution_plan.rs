//! wire 형식과 실행 코어 사이에서 검증된 명령과 자원 예산을 보존한다.

#![cfg_attr(
    not(any(target_os = "linux", test)),
    allow(
        dead_code,
        reason = "실행 plan 소비자는 Linux Runner이며 다른 플랫폼은 API 검증만 빌드합니다"
    )
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::protocol::CommandSpec;
use crate::resource_budget::ResourceBudget;

#[derive(Debug, Clone)]
/// 실행 출처와 무관하게 Runner가 소비하는 내부 실행 계약이다.
pub(crate) struct ResolvedExecutionPlan {
    command: ResolvedCommand,
    budget: ResourceBudget,
}

impl ResolvedExecutionPlan {
    /// Protocol v1 검증을 통과한 Raw Command를 내부 실행 계약으로 고정한다.
    pub(crate) fn from_validated_raw(command: &CommandSpec, budget: ResourceBudget) -> Self {
        Self {
            command: ResolvedCommand::from_validated_raw(command),
            budget,
        }
    }

    pub(crate) fn budget(&self) -> &ResourceBudget {
        &self.budget
    }

    pub(crate) fn into_parts(self) -> (ResolvedCommand, ResourceBudget) {
        (self.command, self.budget)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 셸이나 PATH 해석 없이 execve에 전달할 명령 값이다.
pub(crate) struct ResolvedCommand {
    executable: ResolvedExecutable,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// 실행 파일을 여는 방식이다. Product Package는 후속 variant로 고정 handle을 전달할 수 있다.
pub(crate) enum ResolvedExecutable {
    RawPath(OsString),
}

impl ResolvedCommand {
    fn from_validated_raw(command: &CommandSpec) -> Self {
        let environment = command
            .environment
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect();

        Self {
            executable: ResolvedExecutable::RawPath(OsString::from(&command.program)),
            arguments: command.args.iter().map(OsString::from).collect(),
            working_directory: PathBuf::from(&command.working_directory),
            environment,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ResolvedExecutable,
        Vec<OsString>,
        PathBuf,
        BTreeMap<OsString, OsString>,
    ) {
        (
            self.executable,
            self.arguments,
            self.working_directory,
            self.environment,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::{CpuMax, OutputLimits, ResourceLimits};

    use super::*;

    fn budget() -> ResourceBudget {
        ResourceBudget::try_from_protocol(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 37_000,
                    period_micros: 91_000,
                },
                memory_max_bytes: 987_654_321,
                pids_max: 23,
                wall_time_limit_ms: 45_678,
            },
            OutputLimits {
                stdout_tail_max_bytes: 12_345,
                stderr_tail_max_bytes: 23_456,
            },
        )
        .expect("시험 자원 예산은 유효해야 합니다")
    }

    #[test]
    fn raw_command_preserves_exact_argv_tokens_without_shell_interpretation() {
        let command = CommandSpec {
            program: "/opt/task cage/bin/tool".to_owned(),
            args: vec![
                "value with spaces".to_owned(),
                "$HOME".to_owned(),
                "; touch /tmp/must-not-exist".to_owned(),
                "$(printf injected)".to_owned(),
                "*.mp4".to_owned(),
                String::new(),
            ],
            working_directory: "/srv/task cage/job 42".to_owned(),
            environment: BTreeMap::new(),
        };

        let (resolved, _) =
            ResolvedExecutionPlan::from_validated_raw(&command, budget()).into_parts();
        let (executable, arguments, _, _) = resolved.into_parts();

        assert_eq!(
            executable,
            ResolvedExecutable::RawPath(OsString::from("/opt/task cage/bin/tool"))
        );
        assert_eq!(
            arguments,
            vec![
                OsString::from("value with spaces"),
                OsString::from("$HOME"),
                OsString::from("; touch /tmp/must-not-exist"),
                OsString::from("$(printf injected)"),
                OsString::from("*.mp4"),
                OsString::new(),
            ]
        );
    }

    #[test]
    fn raw_command_preserves_only_explicit_environment_and_working_directory() {
        let explicit_environment = BTreeMap::from([
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ("TOKEN".to_owned(), "value with spaces;$HOME".to_owned()),
        ]);
        let command = CommandSpec {
            program: "/usr/bin/true".to_owned(),
            args: Vec::new(),
            working_directory: "/srv/task cage/job 42".to_owned(),
            environment: explicit_environment,
        };

        let (resolved, _) =
            ResolvedExecutionPlan::from_validated_raw(&command, budget()).into_parts();
        let (_, _, working_directory, environment) = resolved.into_parts();

        assert_eq!(working_directory, PathBuf::from("/srv/task cage/job 42"));
        assert_eq!(
            environment,
            BTreeMap::from([
                (OsString::from("LANG"), OsString::from("C.UTF-8")),
                (
                    OsString::from("TOKEN"),
                    OsString::from("value with spaces;$HOME")
                ),
            ])
        );
        assert!(!environment.contains_key(&OsString::from("PATH")));
    }

    #[test]
    fn raw_plan_preserves_the_verified_resource_budget() {
        let expected = budget();
        let command = CommandSpec {
            program: "/usr/bin/true".to_owned(),
            args: Vec::new(),
            working_directory: "/".to_owned(),
            environment: BTreeMap::new(),
        };

        let plan = ResolvedExecutionPlan::from_validated_raw(&command, expected.clone());
        let actual = plan.budget();

        assert_eq!(actual.cgroup_limits(), expected.cgroup_limits());
        assert_eq!(actual.wall_timeout(), expected.wall_timeout());
        assert_eq!(
            actual.capture_limits().stdout_tail_max_bytes(),
            expected.capture_limits().stdout_tail_max_bytes()
        );
        assert_eq!(
            actual.capture_limits().stderr_tail_max_bytes(),
            expected.capture_limits().stderr_tail_max_bytes()
        );
    }
}
