//! 정리가 끝난 작업 한 건을 protocol v1의 불변 snapshot으로 바꾼다.

use std::time::Instant;

use thiserror::Error;

use crate::cgroup::{JobStats, KernelEvents};
use crate::output::CapturedOutput;
use crate::protocol::{ProcessResult, TaskPayload, TaskTiming, TaskUsage, TerminationReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessEvidence {
    exit_code: Option<i32>,
    signal: Option<i32>,
}

impl ProcessEvidence {
    pub(crate) fn new(exit_code: Option<i32>, signal: Option<i32>) -> Self {
        Self { exit_code, signal }
    }
}

#[cfg(target_os = "linux")]
impl From<crate::executor::ProcessExit> for ProcessEvidence {
    fn from(exit: crate::executor::ProcessExit) -> Self {
        Self::new(exit.exit_code, exit.signal)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionEvidence {
    Started(ProcessEvidence),
    StartFailed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ControlTriggers {
    timed_out: bool,
    cancelled: bool,
}

impl ControlTriggers {
    pub(crate) fn observed(timed_out: bool, cancelled: bool) -> Self {
        Self {
            timed_out,
            cancelled,
        }
    }

    pub(crate) fn none() -> Self {
        Self::observed(false, false)
    }

    pub(crate) fn timed_out() -> Self {
        Self::observed(true, false)
    }

    pub(crate) fn cancelled() -> Self {
        Self::observed(false, true)
    }
}

#[derive(Debug)]
pub(crate) struct TerminationEvidence<'a> {
    execution: ExecutionEvidence,
    control: ControlTriggers,
    event_delta: &'a KernelEvents,
    daemon_error: bool,
}

#[derive(Debug)]
pub(crate) struct CleanedExecution {
    cleanup_complete: bool,
    evidence: ExecutionEvidence,
    control: ControlTriggers,
    stats: JobStats,
    output: CapturedOutput,
    daemon_error: bool,
}

impl CleanedExecution {
    /// process와 job cgroup 정리가 모두 성공한 뒤에만 호출한다.
    pub(crate) fn after_cleanup(
        evidence: ExecutionEvidence,
        control: ControlTriggers,
        stats: JobStats,
        output: CapturedOutput,
        daemon_error: bool,
    ) -> Self {
        Self {
            cleanup_complete: true,
            evidence,
            control,
            stats,
            output,
            daemon_error,
        }
    }
}

#[derive(Debug)]
struct RunningTask {
    task_id: String,
    submitted_at: String,
    started_at: String,
    started_monotonic: Instant,
}

#[derive(Debug)]
enum LifecycleState {
    Running(RunningTask),
    Finished(TaskPayload),
}

#[derive(Debug)]
pub(crate) struct SingleTaskLifecycle {
    state: LifecycleState,
}

impl SingleTaskLifecycle {
    pub(crate) fn running(
        task_id: String,
        submitted_at: String,
        started_at: String,
        started_monotonic: Instant,
    ) -> Self {
        Self {
            state: LifecycleState::Running(RunningTask {
                task_id,
                submitted_at,
                started_at,
                started_monotonic,
            }),
        }
    }

    pub(crate) fn snapshot(&self) -> TaskPayload {
        match &self.state {
            LifecycleState::Running(task) => TaskPayload::Running {
                task_id: task.task_id.clone(),
                submitted_at: task.submitted_at.clone(),
                started_at: task.started_at.clone(),
            },
            LifecycleState::Finished(task) => task.clone(),
        }
    }

    pub(crate) fn complete(
        &mut self,
        completion: CleanedExecution,
        finished_at: String,
        finished_monotonic: Instant,
    ) -> Result<&TaskPayload, LifecycleError> {
        let running = match &self.state {
            LifecycleState::Running(running) => running,
            LifecycleState::Finished(_) => return Err(LifecycleError::AlreadyFinished),
        };

        // FINISHED는 전체 cgroup과 output reader 정리가 확인된 뒤에만 만든다.
        if !completion.cleanup_complete {
            return Err(LifecycleError::CleanupIncomplete);
        }

        let termination_reason = classify_termination(&TerminationEvidence {
            execution: completion.evidence,
            control: completion.control,
            event_delta: &completion.stats.event_delta,
            daemon_error: completion.daemon_error,
        })?;
        if completion.evidence == ExecutionEvidence::StartFailed {
            return Err(LifecycleError::ExecutionStartContractUndecided);
        }

        let process = match completion.evidence {
            ExecutionEvidence::Started(process) => map_process_result(process)?,
            ExecutionEvidence::StartFailed => unreachable!("위에서 시작 실패를 거부했습니다"),
        };
        let wall_time = finished_monotonic
            .checked_duration_since(running.started_monotonic)
            .ok_or(LifecycleError::FinishedBeforeStarted)?;
        let wall_time_ms =
            u64::try_from(wall_time.as_millis()).map_err(|_| LifecycleError::WallTimeOverflow)?;

        let payload = TaskPayload::Finished {
            task_id: running.task_id.clone(),
            termination_reason,
            process,
            timing: TaskTiming {
                submitted_at: running.submitted_at.clone(),
                started_at: running.started_at.clone(),
                finished_at,
                wall_time_ms,
            },
            usage: TaskUsage {
                cpu_time_micros: completion.stats.cpu_usage_micros,
                memory_peak_bytes: completion.stats.memory_peak_bytes,
            },
            output: completion.output.into_task_output(),
        };

        self.state = LifecycleState::Finished(payload);
        match &self.state {
            LifecycleState::Finished(payload) => Ok(payload),
            LifecycleState::Running(_) => unreachable!("방금 FINISHED로 바꿨습니다"),
        }
    }
}

pub(crate) fn classify_termination(
    evidence: &TerminationEvidence<'_>,
) -> Result<TerminationReason, LifecycleError> {
    let memory_exceeded =
        evidence.event_delta.memory_oom > 0 || evidence.event_delta.memory_oom_kill > 0;
    let process_exceeded = evidence.event_delta.pids_max > 0;

    let mut selected = None;
    select_reason(
        &mut selected,
        evidence.control.timed_out,
        TerminationReason::TimedOut,
    )?;
    select_reason(
        &mut selected,
        evidence.control.cancelled,
        TerminationReason::Cancelled,
    )?;
    select_reason(
        &mut selected,
        memory_exceeded,
        TerminationReason::MemoryLimitExceeded,
    )?;
    select_reason(
        &mut selected,
        process_exceeded,
        TerminationReason::ProcessLimitExceeded,
    )?;
    select_reason(
        &mut selected,
        evidence.daemon_error,
        TerminationReason::DaemonError,
    )?;
    select_reason(
        &mut selected,
        evidence.execution == ExecutionEvidence::StartFailed,
        TerminationReason::ExecutionFailed,
    )?;

    Ok(selected.unwrap_or(TerminationReason::Exited))
}

fn select_reason(
    selected: &mut Option<TerminationReason>,
    observed: bool,
    candidate: TerminationReason,
) -> Result<(), LifecycleError> {
    if !observed {
        return Ok(());
    }
    if selected.is_some_and(|reason| reason != candidate) {
        return Err(LifecycleError::AmbiguousTermination);
    }
    *selected = Some(candidate);
    Ok(())
}

fn map_process_result(evidence: ProcessEvidence) -> Result<ProcessResult, LifecycleError> {
    match (evidence.exit_code, evidence.signal) {
        (Some(exit_code), None) => Ok(ProcessResult {
            exit_code: Some(exit_code),
            signal: None,
        }),
        (None, Some(signal)) => Ok(ProcessResult {
            exit_code: None,
            signal: Some(
                linux_signal_name(signal)
                    .ok_or(LifecycleError::UnknownSignal(signal))?
                    .to_owned(),
            ),
        }),
        _ => Err(LifecycleError::InvalidProcessOutcome),
    }
}

fn linux_signal_name(signal: i32) -> Option<&'static str> {
    // 현재 wire fixture가 확정한 문자열만 공개하고 나머지는 계약 결정으로 남긴다.
    (signal == 9).then_some("SIGKILL")
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum LifecycleError {
    #[error("이미 FINISHED인 작업은 다시 완료할 수 없습니다")]
    AlreadyFinished,
    #[error("전체 정리가 끝나기 전에는 FINISHED를 만들 수 없습니다")]
    CleanupIncomplete,
    #[error("여러 종료 원인이 동시에 관찰되어 공개 우선순위 결정이 필요합니다")]
    AmbiguousTermination,
    #[error("exec 시작 실패를 submit 오류와 FINISHED 중 무엇으로 낼지 공개 결정이 필요합니다")]
    ExecutionStartContractUndecided,
    #[error("프로세스 종료 결과에는 exit code와 signal 중 정확히 하나가 있어야 합니다")]
    InvalidProcessOutcome,
    #[error("공개 문자열 규칙이 없는 Linux signal 번호입니다: {0}")]
    UnknownSignal(i32),
    #[error("완료 단조 시간이 시작 단조 시간보다 빠릅니다")]
    FinishedBeforeStarted,
    #[error("wallTimeMs를 u64로 표현할 수 없습니다")]
    WallTimeOverflow,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const TASK_ID: &str = "task-123";
    const SUBMITTED_AT: &str = "2026-02-24T10:00:00.000Z";
    const STARTED_AT: &str = "2026-02-24T10:00:00.100Z";
    const FINISHED_AT: &str = "2026-02-24T10:00:01.100Z";

    fn stats(event_delta: KernelEvents) -> JobStats {
        JobStats {
            cpu_usage_micros: 42,
            memory_current_bytes: 12,
            memory_peak_bytes: 24,
            current_processes: 0,
            peak_processes: Some(2),
            event_delta,
        }
    }

    fn output() -> CapturedOutput {
        CapturedOutput::for_test(b"out".to_vec(), true, b"err".to_vec(), false)
    }

    fn completion(
        evidence: ExecutionEvidence,
        control: ControlTriggers,
        event_delta: KernelEvents,
        daemon_error: bool,
    ) -> CleanedExecution {
        CleanedExecution::after_cleanup(
            evidence,
            control,
            stats(event_delta),
            output(),
            daemon_error,
        )
    }

    fn running(started_monotonic: Instant) -> SingleTaskLifecycle {
        SingleTaskLifecycle::running(
            TASK_ID.to_owned(),
            SUBMITTED_AT.to_owned(),
            STARTED_AT.to_owned(),
            started_monotonic,
        )
    }

    fn started(exit_code: Option<i32>, signal: Option<i32>) -> ExecutionEvidence {
        ExecutionEvidence::Started(ProcessEvidence::new(exit_code, signal))
    }

    fn reason(payload: &TaskPayload) -> TerminationReason {
        match payload {
            TaskPayload::Finished {
                termination_reason, ..
            } => *termination_reason,
            TaskPayload::Running { .. } => panic!("FINISHED snapshot이 필요합니다"),
        }
    }

    #[test]
    fn running_snapshot_uses_caller_identity_and_timestamps() {
        let lifecycle = running(Instant::now());
        assert_eq!(
            lifecycle.snapshot(),
            TaskPayload::Running {
                task_id: TASK_ID.to_owned(),
                submitted_at: SUBMITTED_AT.to_owned(),
                started_at: STARTED_AT.to_owned(),
            }
        );
    }

    #[test]
    fn normal_zero_and_nonzero_exit_are_exited() {
        for exit_code in [0, 23] {
            let start = Instant::now();
            let mut lifecycle = running(start);
            let payload = lifecycle
                .complete(
                    completion(
                        started(Some(exit_code), None),
                        ControlTriggers::none(),
                        KernelEvents::default(),
                        false,
                    ),
                    FINISHED_AT.to_owned(),
                    start + Duration::from_secs(1),
                )
                .unwrap();

            assert_eq!(reason(payload), TerminationReason::Exited);
            match payload {
                TaskPayload::Finished { process, .. } => {
                    assert_eq!(process.exit_code, Some(exit_code));
                    assert_eq!(process.signal, None);
                }
                TaskPayload::Running { .. } => unreachable!(),
            }
        }
    }

    #[test]
    fn timeout_and_cancel_need_explicit_internal_triggers() {
        let cases = [
            (ControlTriggers::timed_out(), TerminationReason::TimedOut),
            (ControlTriggers::cancelled(), TerminationReason::Cancelled),
        ];

        for (control, expected) in cases {
            let start = Instant::now();
            let mut lifecycle = running(start);
            let payload = lifecycle
                .complete(
                    completion(
                        started(None, Some(9)),
                        control,
                        KernelEvents::default(),
                        false,
                    ),
                    FINISHED_AT.to_owned(),
                    start + Duration::from_millis(250),
                )
                .unwrap();
            assert_eq!(reason(payload), expected);
        }
    }

    #[test]
    fn resource_event_deltas_select_memory_or_process_limit() {
        let cases = [
            (
                KernelEvents {
                    memory_oom_kill: 1,
                    ..KernelEvents::default()
                },
                TerminationReason::MemoryLimitExceeded,
            ),
            (
                KernelEvents {
                    pids_max: 1,
                    ..KernelEvents::default()
                },
                TerminationReason::ProcessLimitExceeded,
            ),
        ];

        for (events, expected) in cases {
            let start = Instant::now();
            let mut lifecycle = running(start);
            let payload = lifecycle
                .complete(
                    completion(
                        started(None, Some(9)),
                        ControlTriggers::none(),
                        events,
                        false,
                    ),
                    FINISHED_AT.to_owned(),
                    start + Duration::from_secs(1),
                )
                .unwrap();
            assert_eq!(reason(payload), expected);
        }
    }

    #[test]
    fn daemon_error_is_finished_only_after_cleanup() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        let payload = lifecycle
            .complete(
                completion(
                    started(Some(70), None),
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    true,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(reason(payload), TerminationReason::DaemonError);
    }

    #[test]
    fn exit_137_and_sigkill_alone_do_not_infer_a_limit_or_timeout() {
        for evidence in [started(Some(137), None), started(None, Some(9))] {
            let classification = classify_termination(&TerminationEvidence {
                execution: evidence,
                control: ControlTriggers::none(),
                event_delta: &KernelEvents::default(),
                daemon_error: false,
            });
            assert_eq!(classification, Ok(TerminationReason::Exited));
        }
    }

    #[test]
    fn exec_failure_is_only_a_candidate_until_public_contract_is_decided() {
        let events = KernelEvents::default();
        assert_eq!(
            classify_termination(&TerminationEvidence {
                execution: ExecutionEvidence::StartFailed,
                control: ControlTriggers::none(),
                event_delta: &events,
                daemon_error: false,
            }),
            Ok(TerminationReason::ExecutionFailed)
        );

        let start = Instant::now();
        let mut lifecycle = running(start);
        assert_eq!(
            lifecycle.complete(
                completion(
                    ExecutionEvidence::StartFailed,
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            ),
            Err(LifecycleError::ExecutionStartContractUndecided)
        );
        assert!(matches!(lifecycle.snapshot(), TaskPayload::Running { .. }));
    }

    #[test]
    fn ambiguous_public_priorities_do_not_produce_finished() {
        let cases = [
            (
                ControlTriggers::observed(true, true),
                KernelEvents::default(),
                false,
            ),
            (
                ControlTriggers::none(),
                KernelEvents {
                    memory_oom: 1,
                    memory_oom_kill: 0,
                    pids_max: 1,
                },
                false,
            ),
            (
                ControlTriggers::none(),
                KernelEvents {
                    memory_oom: 1,
                    ..KernelEvents::default()
                },
                true,
            ),
        ];

        for (control, events, daemon_error) in cases {
            let start = Instant::now();
            let mut lifecycle = running(start);
            assert_eq!(
                lifecycle.complete(
                    completion(started(None, Some(9)), control, events, daemon_error),
                    FINISHED_AT.to_owned(),
                    start + Duration::from_secs(1),
                ),
                Err(LifecycleError::AmbiguousTermination)
            );
            assert!(matches!(lifecycle.snapshot(), TaskPayload::Running { .. }));
        }
    }

    #[test]
    fn cleanup_must_be_complete_before_finished() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        let mut completion = completion(
            started(Some(0), None),
            ControlTriggers::none(),
            KernelEvents::default(),
            false,
        );
        completion.cleanup_complete = false;

        assert_eq!(
            lifecycle.complete(
                completion,
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            ),
            Err(LifecycleError::CleanupIncomplete)
        );
        assert!(matches!(lifecycle.snapshot(), TaskPayload::Running { .. }));
    }

    #[test]
    fn finished_snapshot_maps_timing_usage_output_and_signal() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        let payload = lifecycle
            .complete(
                completion(
                    started(None, Some(9)),
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_millis(1_234),
            )
            .unwrap();

        match payload {
            TaskPayload::Finished {
                task_id,
                process,
                timing,
                usage,
                output,
                ..
            } => {
                assert_eq!(task_id, TASK_ID);
                assert_eq!(process.exit_code, None);
                assert_eq!(process.signal.as_deref(), Some("SIGKILL"));
                assert_eq!(timing.submitted_at, SUBMITTED_AT);
                assert_eq!(timing.started_at, STARTED_AT);
                assert_eq!(timing.finished_at, FINISHED_AT);
                assert_eq!(timing.wall_time_ms, 1_234);
                assert_eq!(usage.cpu_time_micros, 42);
                assert_eq!(usage.memory_peak_bytes, 24);
                assert_eq!(output.stdout_tail, "out");
                assert_eq!(output.stderr_tail, "err");
                assert!(output.stdout_truncated);
                assert!(!output.stderr_truncated);
            }
            TaskPayload::Running { .. } => unreachable!(),
        }
    }

    #[test]
    fn second_completion_cannot_overwrite_finished_snapshot() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        lifecycle
            .complete(
                completion(
                    started(Some(0), None),
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            )
            .unwrap();
        let original = lifecycle.snapshot();

        assert_eq!(
            lifecycle.complete(
                completion(
                    started(None, Some(9)),
                    ControlTriggers::timed_out(),
                    KernelEvents::default(),
                    false,
                ),
                "2026-02-24T10:00:02.100Z".to_owned(),
                start + Duration::from_secs(2),
            ),
            Err(LifecycleError::AlreadyFinished)
        );
        assert_eq!(lifecycle.snapshot(), original);
    }

    #[test]
    fn finished_snapshot_exposes_only_existing_protocol_fields() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        lifecycle
            .complete(
                completion(
                    started(Some(0), None),
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            )
            .unwrap();

        let value = serde_json::to_value(lifecycle.snapshot()).unwrap();
        let fields = value.as_object().unwrap();
        let mut names: Vec<_> = fields.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "output",
                "process",
                "state",
                "taskId",
                "terminationReason",
                "timing",
                "usage",
            ]
        );
        assert!(fields.get("cleanupComplete").is_none());
        assert!(fields.get("eventDelta").is_none());
    }

    #[test]
    fn only_events_after_the_baseline_affect_classification() {
        let baseline = KernelEvents {
            memory_oom: 4,
            memory_oom_kill: 3,
            pids_max: 7,
        };
        let current = KernelEvents {
            memory_oom: 4,
            memory_oom_kill: 5,
            pids_max: 7,
        };
        let delta = current.delta_from(&baseline);

        assert_eq!(delta.memory_oom, 0);
        assert_eq!(delta.memory_oom_kill, 2);
        assert_eq!(delta.pids_max, 0);
        assert_eq!(
            classify_termination(&TerminationEvidence {
                execution: started(None, Some(9)),
                control: ControlTriggers::none(),
                event_delta: &delta,
                daemon_error: false,
            }),
            Ok(TerminationReason::MemoryLimitExceeded)
        );
    }

    #[test]
    fn unknown_signal_does_not_create_an_arbitrary_wire_string() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        assert_eq!(
            lifecycle.complete(
                completion(
                    started(None, Some(11)),
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            ),
            Err(LifecycleError::UnknownSignal(11))
        );
        assert!(matches!(lifecycle.snapshot(), TaskPayload::Running { .. }));
    }

    #[test]
    fn finished_monotonic_time_cannot_precede_started_time() {
        let finished = Instant::now();
        let started_at_monotonic = finished + Duration::from_millis(1);
        let mut lifecycle = running(started_at_monotonic);
        assert_eq!(
            lifecycle.complete(
                completion(
                    started(Some(0), None),
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                finished,
            ),
            Err(LifecycleError::FinishedBeforeStarted)
        );
    }
}
