//! 정리가 끝난 작업 한 건을 protocol v1의 불변 snapshot으로 바꾼다.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use thiserror::Error;

use crate::cgroup::{JobStats, KernelEvents};
use crate::output::CapturedOutput;
use taskcage_core::task::{
    ProcessResult, TaskResult, TaskSnapshot as TaskPayload, TaskTiming, TaskUsage,
    TerminationReason,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessEvidence {
    exit_code: Option<i32>,
    signal: Option<i32>,
}

impl ProcessEvidence {
    pub(crate) fn new(exit_code: Option<i32>, signal: Option<i32>) -> Self {
        Self { exit_code, signal }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn exit_code(self) -> Option<i32> {
        self.exit_code
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn signal(self) -> Option<i32> {
        self.signal
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
    StartFailed { errno: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlTrigger {
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ControlTriggers {
    first: Option<ControlTrigger>,
}

impl ControlTriggers {
    pub(crate) fn none() -> Self {
        Self { first: None }
    }

    pub(crate) fn timed_out() -> Self {
        Self {
            first: Some(ControlTrigger::TimedOut),
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            first: Some(ControlTrigger::Cancelled),
        }
    }

    pub(crate) fn first(self) -> Option<ControlTrigger> {
        self.first
    }
}

#[derive(Debug, Default)]
pub(crate) struct TerminalTriggerLatch {
    first: AtomicU8,
}

impl TerminalTriggerLatch {
    /// timeout과 향후 cancel 경로가 같은 first-observed 규칙을 사용한다.
    pub(crate) fn observe(&self, trigger: ControlTrigger) -> bool {
        let value = match trigger {
            ControlTrigger::TimedOut => 1,
            ControlTrigger::Cancelled => 2,
        };
        self.first
            .compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn snapshot(&self) -> ControlTriggers {
        match self.first.load(Ordering::Acquire) {
            1 => ControlTriggers::timed_out(),
            2 => ControlTriggers::cancelled(),
            _ => ControlTriggers::none(),
        }
    }

    /// target 종료가 먼저 관찰되면 늦은 timeout과 cancel을 막는다.
    pub(crate) fn close_without_control(&self) -> bool {
        self.first
            .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
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
struct CompletionData {
    evidence: ExecutionEvidence,
    control: ControlTriggers,
    stats: JobStats,
    output: CapturedOutput,
    daemon_error: bool,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestCompletion(CompletionData);

#[cfg(test)]
impl TestCompletion {
    pub(crate) fn new(
        evidence: ExecutionEvidence,
        control: ControlTriggers,
        stats: JobStats,
        output: CapturedOutput,
        daemon_error: bool,
    ) -> Self {
        Self(CompletionData {
            evidence,
            control,
            stats,
            output,
            daemon_error,
        })
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

    #[cfg(target_os = "linux")]
    pub(crate) fn complete(
        &mut self,
        completion: crate::adapters::linux_executor::CleanedRun,
        finished_at: String,
        finished_monotonic: Instant,
    ) -> Result<&TaskPayload, LifecycleError> {
        let (evidence, control, stats, output, daemon_error) = completion.into_lifecycle_parts();
        self.complete_inner(
            CompletionData {
                evidence,
                control,
                stats,
                output,
                daemon_error,
            },
            finished_at,
            finished_monotonic,
        )
    }

    #[cfg(test)]
    pub(crate) fn complete_for_test(
        &mut self,
        completion: TestCompletion,
        finished_at: String,
        finished_monotonic: Instant,
    ) -> Result<&TaskPayload, LifecycleError> {
        self.complete_inner(completion.0, finished_at, finished_monotonic)
    }

    fn complete_inner(
        &mut self,
        completion: CompletionData,
        finished_at: String,
        finished_monotonic: Instant,
    ) -> Result<&TaskPayload, LifecycleError> {
        let running = match &self.state {
            LifecycleState::Running(running) => running,
            LifecycleState::Finished(_) => return Err(LifecycleError::AlreadyFinished),
        };

        let termination_reason = classify_termination(&TerminationEvidence {
            execution: completion.evidence,
            control: completion.control,
            event_delta: &completion.stats.event_delta,
            daemon_error: completion.daemon_error,
        })?;

        let process = match completion.evidence {
            ExecutionEvidence::Started(process) => map_process_result(process)?,
            ExecutionEvidence::StartFailed { .. } => ProcessResult {
                exit_code: None,
                signal: None,
            },
        };
        let wall_time = finished_monotonic
            .checked_duration_since(running.started_monotonic)
            .ok_or(LifecycleError::FinishedBeforeStarted)?;
        let wall_time_ms =
            u64::try_from(wall_time.as_millis()).map_err(|_| LifecycleError::WallTimeOverflow)?;

        let payload = TaskPayload::from_result(TaskResult {
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
            output: crate::output::into_task_output(completion.output),
        });

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
    if let Some(trigger) = evidence.control.first() {
        return Ok(match trigger {
            ControlTrigger::TimedOut => TerminationReason::TimedOut,
            ControlTrigger::Cancelled => TerminationReason::Cancelled,
        });
    }
    // 실제 kill 증거, PID 제한 증거, kill 없는 OOM 통지 순으로 더 강한 근거를 우선한다.
    if evidence.event_delta.memory_oom_kill > 0 {
        return Ok(TerminationReason::MemoryLimitExceeded);
    }
    if evidence.event_delta.pids_max > 0 {
        return Ok(TerminationReason::ProcessLimitExceeded);
    }
    if evidence.event_delta.memory_oom > 0 {
        return Ok(TerminationReason::MemoryLimitExceeded);
    }
    if evidence.daemon_error {
        return Ok(TerminationReason::DaemonError);
    }
    if matches!(evidence.execution, ExecutionEvidence::StartFailed { .. }) {
        return Ok(TerminationReason::ExecutionFailed);
    }
    Ok(TerminationReason::Exited)
}

fn map_process_result(evidence: ProcessEvidence) -> Result<ProcessResult, LifecycleError> {
    match (evidence.exit_code, evidence.signal) {
        (Some(exit_code), None) => Ok(ProcessResult {
            exit_code: Some(exit_code),
            signal: None,
        }),
        (None, Some(signal)) => Ok(ProcessResult {
            exit_code: None,
            signal: Some(linux_signal_name(signal).ok_or(LifecycleError::UnknownSignal(signal))?),
        }),
        _ => Err(LifecycleError::InvalidProcessOutcome),
    }
}

fn linux_signal_name(signal: i32) -> Option<String> {
    let standard = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        16 => "SIGSTKFLT",
        17 => "SIGCHLD",
        18 => "SIGCONT",
        19 => "SIGSTOP",
        20 => "SIGTSTP",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGURG",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGIO",
        30 => "SIGPWR",
        31 => "SIGSYS",
        34 => "SIGRTMIN",
        35..=64 => return Some(format!("SIGRTMIN+{}", signal - 34)),
        _ => return None,
    };
    Some(standard.to_owned())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum LifecycleError {
    #[error("이미 FINISHED인 작업은 다시 완료할 수 없습니다")]
    AlreadyFinished,
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
    ) -> TestCompletion {
        TestCompletion::new(
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

    fn observed(trigger: ControlTrigger) -> ControlTriggers {
        let latch = TerminalTriggerLatch::default();
        assert!(latch.observe(trigger));
        latch.snapshot()
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
                .complete_for_test(
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
            (
                observed(ControlTrigger::Cancelled),
                TerminationReason::Cancelled,
            ),
        ];

        for (control, expected) in cases {
            let start = Instant::now();
            let mut lifecycle = running(start);
            let payload = lifecycle
                .complete_for_test(
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
                .complete_for_test(
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
            .complete_for_test(
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
    fn exec_failure_finishes_without_a_process_result() {
        let events = KernelEvents::default();
        assert_eq!(
            classify_termination(&TerminationEvidence {
                execution: ExecutionEvidence::StartFailed { errno: 2 },
                control: ControlTriggers::none(),
                event_delta: &events,
                daemon_error: false,
            }),
            Ok(TerminationReason::ExecutionFailed)
        );

        let start = Instant::now();
        let mut lifecycle = running(start);
        let payload = lifecycle
            .complete_for_test(
                completion(
                    ExecutionEvidence::StartFailed { errno: 2 },
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(reason(payload), TerminationReason::ExecutionFailed);
        assert!(matches!(
            payload,
            TaskPayload::Finished { process, .. }
                if process.exit_code.is_none() && process.signal.is_none()
        ));
    }

    #[test]
    fn first_control_trigger_cannot_be_overwritten() {
        let control = TerminalTriggerLatch::default();
        assert!(control.observe(ControlTrigger::Cancelled));
        assert!(!control.observe(ControlTrigger::TimedOut));
        assert_eq!(control.snapshot().first(), Some(ControlTrigger::Cancelled));
    }

    #[test]
    fn normal_exit_closes_the_terminal_latch_against_late_control() {
        let control = TerminalTriggerLatch::default();
        assert!(control.close_without_control());
        assert!(!control.observe(ControlTrigger::Cancelled));
        assert!(!control.observe(ControlTrigger::TimedOut));
        assert_eq!(control.snapshot().first(), None);
    }

    #[test]
    fn simultaneous_memory_and_pid_events_use_documented_evidence_priority() {
        let events = KernelEvents {
            memory_oom: 1,
            memory_oom_kill: 0,
            pids_max: 1,
        };
        assert_eq!(
            classify_termination(&TerminationEvidence {
                execution: started(None, Some(9)),
                control: ControlTriggers::none(),
                event_delta: &events,
                daemon_error: false,
            }),
            Ok(TerminationReason::ProcessLimitExceeded)
        );

        let killed = KernelEvents {
            memory_oom_kill: 1,
            ..events
        };
        assert_eq!(
            classify_termination(&TerminationEvidence {
                execution: started(None, Some(9)),
                control: ControlTriggers::none(),
                event_delta: &killed,
                daemon_error: false,
            }),
            Ok(TerminationReason::MemoryLimitExceeded)
        );
    }

    #[test]
    fn finished_snapshot_maps_timing_usage_output_and_signal() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        let payload = lifecycle
            .complete_for_test(
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
            .complete_for_test(
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
            lifecycle.complete_for_test(
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
            .complete_for_test(
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

        let value =
            serde_json::to_value(crate::protocol_mapper::task_snapshot(lifecycle.snapshot()))
                .unwrap();
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
    fn canonical_and_realtime_signal_names_are_stable() {
        for (signal, expected) in [
            (9, "SIGKILL"),
            (11, "SIGSEGV"),
            (15, "SIGTERM"),
            (34, "SIGRTMIN"),
            (64, "SIGRTMIN+30"),
        ] {
            assert_eq!(linux_signal_name(signal).as_deref(), Some(expected));
        }
    }

    #[test]
    fn unknown_signal_does_not_create_an_arbitrary_wire_string() {
        let start = Instant::now();
        let mut lifecycle = running(start);
        assert_eq!(
            lifecycle.complete_for_test(
                completion(
                    started(None, Some(33)),
                    ControlTriggers::none(),
                    KernelEvents::default(),
                    false,
                ),
                FINISHED_AT.to_owned(),
                start + Duration::from_secs(1),
            ),
            Err(LifecycleError::UnknownSignal(33))
        );
        assert!(matches!(lifecycle.snapshot(), TaskPayload::Running { .. }));
    }

    #[test]
    fn finished_monotonic_time_cannot_precede_started_time() {
        let finished = Instant::now();
        let started_at_monotonic = finished + Duration::from_millis(1);
        let mut lifecycle = running(started_at_monotonic);
        assert_eq!(
            lifecycle.complete_for_test(
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
