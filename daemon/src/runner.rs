//! 검증된 자원 예산으로 atomic runner를 실행하고 단일 task lifecycle을 완료한다.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::pending;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cancellation::CancellationRuntime;
use crate::cgroup::{CgroupError, CgroupManager, JobCgroup, JobStats};
use crate::deadline::MonotonicDeadline;
use crate::executor::{
    ExecFailure, PreparedCommand, ProcessExit, SpawnOutcome, SpawnedProcess, StartCommitToken,
    WaitOutcome, spawn_in_cgroup,
};
use crate::fail_stop::{CleanupFailureReport, FailStopCoordinator, StartCommitError};
use crate::lifecycle::{
    ControlTrigger, ControlTriggers, ExecutionEvidence, ProcessEvidence, SingleTaskLifecycle,
};
use crate::output::{CaptureLimits, CapturedOutput};
use crate::preflight::VerifiedEnvironment;
use crate::protocol::{CommandSpec, TaskPayload};
use crate::resource_budget::ResourceBudget;
use crate::submit::RunnerPermit;
use crate::{Error, Result};

#[derive(Debug)]
/// cgroup과 출력 reader 정리가 끝난 뒤 Runner만 만들 수 있는 완료 결과다.
pub(crate) struct CompletedTask {
    payload: TaskPayload,
}

impl CompletedTask {
    fn new(payload: TaskPayload) -> Result<Self> {
        if !matches!(payload, TaskPayload::Finished { .. }) {
            return Err(Error::TaskLifecycle(
                "정리가 끝난 lifecycle이 FINISHED 결과를 만들지 않았습니다".to_owned(),
            ));
        }
        Ok(Self { payload })
    }

    pub(crate) fn into_payload(self) -> TaskPayload {
        self.payload
    }
}

#[derive(Debug)]
/// wire 검증이 끝난 작업을 실행 코어에 넘기는 내부 입력이다.
pub(crate) struct TaskRunConfig {
    pub(crate) task_id: String,
    pub(crate) submitted_at: String,
    pub(crate) started_at: String,
    pub(crate) started_monotonic: Instant,
    pub(crate) cleanup_timeout: Duration,
    pub(crate) command: CommandSpec,
    pub(crate) budget: ResourceBudget,
}

#[derive(Debug)]
/// 같은 cgroup manager와 정리 안전 상태를 공유하는 작업 실행 경계다.
pub(crate) struct TaskRunner {
    manager: CgroupManager,
    fail_stop: Arc<FailStopCoordinator>,
}

#[derive(Debug)]
pub(crate) struct TaskRunFailure {
    error: Error,
    capacity_reusable: bool,
    cleanup_complete: bool,
}

impl TaskRunFailure {
    fn with_reusable_capacity(error: Error) -> Self {
        Self {
            error,
            capacity_reusable: true,
            cleanup_complete: true,
        }
    }

    pub(crate) fn capacity_reusable(&self) -> bool {
        self.capacity_reusable
    }

    pub(crate) fn into_error(self) -> Error {
        self.error
    }

    pub(crate) fn cleanup_complete(&self) -> bool {
        self.cleanup_complete
    }
}

impl TaskRunner {
    /// preflight 성공 토큰 없이는 실행기를 만들 수 없다.
    pub(crate) fn initialize(
        environment: VerifiedEnvironment,
        fail_stop: Arc<FailStopCoordinator>,
    ) -> Result<Self> {
        Ok(Self {
            manager: CgroupManager::initialize(environment)?,
            fail_stop,
        })
    }

    /// 전체 정리가 증명된 결과만 FINISHED snapshot으로 바꾼다.
    pub(crate) async fn run_task<F>(
        &self,
        _permit: RunnerPermit,
        config: TaskRunConfig,
        running_sender: tokio::sync::oneshot::Sender<TaskPayload>,
        cancellation: CancellationRuntime,
        finished_time: F,
    ) -> std::result::Result<CompletedTask, TaskRunFailure>
    where
        F: FnOnce() -> (String, Instant),
    {
        if self.fail_stop.is_fail_stopping() {
            return Err(TaskRunFailure {
                error: Error::CleanupUncertain,
                capacity_reusable: false,
                cleanup_complete: true,
            });
        }

        let prepared = prepare_protocol_command(&config.command)
            .map_err(TaskRunFailure::with_reusable_capacity)?;
        let mut lifecycle = SingleTaskLifecycle::running(
            config.task_id.clone(),
            config.submitted_at,
            config.started_at,
            config.started_monotonic,
        );
        let execution = ExecutionConfig {
            job_id: config.task_id,
            limits: config.budget.cgroup_limits(),
            wall_timeout: config.budget.wall_timeout(),
            cleanup_timeout: config.cleanup_timeout,
            capture_limits: config.budget.capture_limits(),
            prepared,
        };

        let cleaned = match execute(
            &self.manager,
            execution,
            cancellation,
            Some(&self.fail_stop),
            || {
                // execve 성공을 확인한 뒤 실제로 완료할 같은 lifecycle의 snapshot만 공개한다.
                let _ = running_sender.send(lifecycle.snapshot());
            },
        )
        .await
        {
            Ok(cleaned) => cleaned,
            Err(failure) => {
                if failure.block_future_runs {
                    self.fail_stop.activate(failure.report.clone());
                }
                return Err(TaskRunFailure {
                    error: *failure.error,
                    capacity_reusable: !failure.block_future_runs,
                    cleanup_complete: failure.cleanup_complete,
                });
            }
        };
        let (finished_at, finished_monotonic) = finished_time();
        let payload = lifecycle
            .complete(cleaned, finished_at, finished_monotonic)
            .cloned()
            .map_err(|error| {
                TaskRunFailure::with_reusable_capacity(Error::TaskLifecycle(error.to_string()))
            })?;
        CompletedTask::new(payload).map_err(TaskRunFailure::with_reusable_capacity)
    }

    #[cfg(test)]
    pub(crate) fn cleanup_is_uncertain(&self) -> bool {
        self.fail_stop.is_fail_stopping()
    }
}

fn prepare_protocol_command(command: &CommandSpec) -> Result<PreparedCommand> {
    let mut argv = Vec::with_capacity(command.args.len() + 1);
    argv.push(OsString::from(&command.program));
    argv.extend(command.args.iter().map(OsString::from));
    let environment = command
        .environment
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect::<BTreeMap<_, _>>();
    Ok(PreparedCommand::new(
        argv,
        &PathBuf::from(&command.working_directory),
        environment,
    )?)
}

pub(crate) struct ExecutionConfig {
    pub(crate) job_id: String,
    pub(crate) limits: crate::cgroup::CgroupLimits,
    pub(crate) wall_timeout: Duration,
    pub(crate) cleanup_timeout: Duration,
    pub(crate) capture_limits: CaptureLimits,
    pub(crate) prepared: PreparedCommand,
}

#[derive(Debug)]
pub(crate) struct CleanedRun {
    job_id: String,
    pid: i32,
    membership_verified: bool,
    evidence: ExecutionEvidence,
    control: ControlTriggers,
    stats: JobStats,
    output: CapturedOutput,
    daemon_error: bool,
}

impl CleanedRun {
    pub(crate) fn into_lifecycle_parts(
        self,
    ) -> (
        ExecutionEvidence,
        ControlTriggers,
        JobStats,
        CapturedOutput,
        bool,
    ) {
        (
            self.evidence,
            self.control,
            self.stats,
            self.output,
            self.daemon_error,
        )
    }

    pub(crate) fn into_diagnostic_parts(self) -> DiagnosticRun {
        let (exit_code, signal, exec_errno) = match self.evidence {
            ExecutionEvidence::Started(process) => (process.exit_code(), process.signal(), None),
            ExecutionEvidence::StartFailed { errno } => (None, None, Some(errno)),
        };
        DiagnosticRun {
            job_id: self.job_id,
            pid: self.pid,
            membership_verified: self.membership_verified,
            timed_out: self.control.first() == Some(crate::lifecycle::ControlTrigger::TimedOut),
            exit_code,
            signal,
            exec_errno,
            stats: self.stats,
            output: self.output,
        }
    }
}

pub(crate) struct DiagnosticRun {
    pub(crate) job_id: String,
    pub(crate) pid: i32,
    pub(crate) membership_verified: bool,
    pub(crate) timed_out: bool,
    pub(crate) exit_code: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) exec_errno: Option<i32>,
    pub(crate) stats: JobStats,
    pub(crate) output: CapturedOutput,
}

pub(crate) struct CoreFailure {
    error: Box<Error>,
    block_future_runs: bool,
    cleanup_complete: bool,
    report: CleanupFailureReport,
}

impl CoreFailure {
    fn before_job(job_id: &str, error: CgroupError) -> Self {
        let block_future_runs = matches!(&error, CgroupError::CleanupCombined { .. });
        Self {
            error: Box::new(error.into()),
            block_future_runs,
            cleanup_complete: !block_future_runs,
            report: CleanupFailureReport::new(
                job_id,
                "작업 cgroup 생성",
                vec!["작업 cgroup rollback"],
                "생성 실패 정리 결과를 확인함",
            ),
        }
    }

    pub(crate) fn into_error(self) -> Error {
        *self.error
    }
}

pub(crate) async fn execute<F>(
    manager: &CgroupManager,
    config: ExecutionConfig,
    cancellation: CancellationRuntime,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
    on_started: F,
) -> std::result::Result<CleanedRun, CoreFailure>
where
    F: FnOnce(),
{
    let ExecutionConfig {
        job_id,
        limits,
        wall_timeout,
        cleanup_timeout,
        capture_limits,
        prepared,
    } = config;
    if let Some(deadline) = fail_stop.and_then(|coordinator| coordinator.deadline()) {
        return Err(fail_stop_before_side_effect(&job_id, deadline));
    }
    let mut job = manager
        .create_job(&job_id, limits)
        .map_err(|error| CoreFailure::before_job(&job_id, error))?;

    if let Some(deadline) = fail_stop.and_then(|coordinator| coordinator.deadline()) {
        return Err(
            cleanup_job_for_fail_stop(job, deadline, &job_id, "cgroup 생성 뒤 fail-stop").await,
        );
    }

    let mut pending = match spawn_in_cgroup(&prepared, job.raw_fd(), capture_limits) {
        Ok(pending) => pending,
        Err(error) => {
            drop(prepared);
            return Err(cleanup_job_after_failure(
                &mut job,
                cleanup_timeout,
                &job_id,
                fail_stop,
                "프로세스 생성",
                error.to_string(),
                false,
            )
            .await);
        }
    };
    if let Some(deadline) = fail_stop.and_then(|coordinator| coordinator.deadline()) {
        drop(prepared);
        return Err(cleanup_pending_job_until(
            &mut job,
            pending,
            deadline,
            &job_id,
            fail_stop,
            "pending clone3 child 단계 fail-stop",
            "다른 작업의 정리 불확실성이 먼저 관찰됐습니다".to_owned(),
        )
        .await);
    }
    match job.contains_pid(pending.pid()) {
        Ok(true) => {}
        Ok(false) => {
            drop(prepared);
            return Err(cleanup_pending_job(
                &mut job,
                pending,
                cleanup_timeout,
                &job_id,
                fail_stop,
                "exec 전 cgroup 소속 재확인",
                "PID가 작업 cgroup에서 확인되지 않았습니다".to_owned(),
            )
            .await);
        }
        Err(error) => {
            drop(prepared);
            return Err(cleanup_pending_job(
                &mut job,
                pending,
                cleanup_timeout,
                &job_id,
                fail_stop,
                "exec 전 cgroup 소속 재확인",
                error.to_string(),
            )
            .await);
        }
    }
    enum StartDecision {
        Committed(StartCommitToken),
        FailStopping(MonotonicDeadline),
        Failed(String),
    }
    let start = match fail_stop {
        Some(coordinator) => match coordinator
            .commit_start(&job_id, || pending.commit_start_signal())
        {
            Ok(token) => StartDecision::Committed(token),
            Err(StartCommitError::FailStopping(deadline)) => StartDecision::FailStopping(deadline),
            Err(StartCommitError::NotActive(task_id)) => StartDecision::Failed(format!(
                "활성 실행 owner가 없는 taskId는 exec를 시작할 수 없습니다: {task_id}"
            )),
            Err(StartCommitError::AlreadyResolved { task_id, state }) => StartDecision::Failed(
                format!("exec 시작이 이미 확정된 taskId입니다: {task_id} ({state})"),
            ),
            Err(StartCommitError::Gate(error)) => StartDecision::Failed(error.to_string()),
        },
        None => match pending.commit_start_signal() {
            Ok(token) => StartDecision::Committed(token),
            Err(error) => StartDecision::Failed(error.to_string()),
        },
    };
    let token = match start {
        StartDecision::Committed(token) => token,
        StartDecision::FailStopping(deadline) => {
            drop(prepared);
            return Err(cleanup_pending_job_until(
                &mut job,
                pending,
                deadline,
                &job_id,
                fail_stop,
                "exec 시작 commit 전 fail-stop",
                "fail-stop 전환이 exec gate보다 먼저 완료됐습니다".to_owned(),
            )
            .await);
        }
        StartDecision::Failed(cause) => {
            drop(prepared);
            return Err(cleanup_pending_job(
                &mut job,
                pending,
                cleanup_timeout,
                &job_id,
                fail_stop,
                "exec 시작 gate commit",
                cause,
            )
            .await);
        }
    };
    let committed = pending.into_start_committed(token);
    let spawn = match committed.wait_for_exec() {
        Ok(spawn) => spawn,
        Err(error) => {
            let isolation_uncertain = matches!(&error, crate::executor::ExecutorError::Wait(_));
            drop(prepared);
            return Err(cleanup_job_after_failure(
                &mut job,
                cleanup_timeout,
                &job_id,
                fail_stop,
                "프로세스 시작",
                error.to_string(),
                isolation_uncertain,
            )
            .await);
        }
    };
    // clone3 자식이 execve를 끝낸 뒤에는 raw argv 포인터를 await 경계에 보관하지 않는다.
    drop(prepared);

    match spawn {
        SpawnOutcome::ExecFailed(failure) => {
            finish_exec_failure(job_id, job, failure, cleanup_timeout, fail_stop).await
        }
        SpawnOutcome::Started(process) => {
            finish_started_process(
                StartedProcessConfig {
                    job_id,
                    wall_timeout,
                    cleanup_timeout,
                },
                job,
                process,
                cancellation,
                fail_stop,
                on_started,
            )
            .await
        }
    }
}

async fn finish_exec_failure(
    job_id: String,
    mut job: JobCgroup,
    failure: ExecFailure,
    cleanup_timeout: Duration,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
) -> std::result::Result<CleanedRun, CoreFailure> {
    let deadline = cleanup_deadline(cleanup_timeout, &job_id, "exec 실패 뒤 작업 cgroup 정리")?;
    match finish_job_with_retry(
        &mut job,
        deadline,
        false,
        fail_stop,
        &job_id,
        "exec 실패 뒤 작업 cgroup 정리",
    )
    .await
    {
        Ok((stats, _recovered, _deadline)) => Ok(CleanedRun {
            job_id,
            pid: failure.pid,
            membership_verified: true,
            evidence: ExecutionEvidence::StartFailed {
                errno: failure.errno,
            },
            control: ControlTriggers::none(),
            stats,
            output: failure.output,
            daemon_error: false,
        }),
        Err(error) => Err(uncertain_failure(
            &job_id,
            "exec 실패 뒤 작업 cgroup 정리",
            error,
            vec!["작업 cgroup"],
        )),
    }
}

struct StartedProcessConfig {
    job_id: String,
    wall_timeout: Duration,
    cleanup_timeout: Duration,
}

async fn finish_started_process<F>(
    config: StartedProcessConfig,
    job: JobCgroup,
    process: SpawnedProcess,
    cancellation: CancellationRuntime,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
    on_started: F,
) -> std::result::Result<CleanedRun, CoreFailure>
where
    F: FnOnce(),
{
    let StartedProcessConfig {
        job_id,
        wall_timeout,
        cleanup_timeout,
    } = config;
    on_started();
    enum WaitDecision {
        Cancelled,
        FailStop(MonotonicDeadline),
        Process(std::result::Result<WaitOutcome, crate::executor::ExecutorError>),
    }
    let wait_decision = tokio::select! {
        biased;
        _ = cancellation.cancelled() => WaitDecision::Cancelled,
        deadline = wait_for_fail_stop(fail_stop) => WaitDecision::FailStop(deadline),
        outcome = process.wait_for(wall_timeout) => WaitDecision::Process(outcome),
    };

    let (control, exit, kill_already_sent) = match wait_decision {
        WaitDecision::Process(Err(error)) => {
            let deadline = cleanup_deadline(cleanup_timeout, &job_id, "target 종료 대기")?;
            return cleanup_running_job(
                job,
                process,
                RecoveryContext::new(
                    job_id,
                    deadline,
                    "target 종료 대기",
                    error.to_string(),
                    cancellation.control_snapshot(),
                    true,
                ),
                fail_stop,
            )
            .await;
        }
        WaitDecision::Process(Ok(WaitOutcome::Exited(exit)))
            if cancellation.close_without_control() =>
        {
            (ControlTriggers::none(), exit, false)
        }
        WaitDecision::Process(Ok(WaitOutcome::Exited(exit))) => {
            // cancel이 waitpid와 거의 동시에 먼저 기록됐으면 이미 회수한 child는 다시 기다리지 않는다.
            let kill_already_sent = match job.kill_all() {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(cause = %error, "종료 상태 회수와 경쟁한 cgroup cancel을 정리 경로에서 재시도합니다");
                    false
                }
            };
            (cancellation.control_snapshot(), exit, kill_already_sent)
        }
        WaitDecision::Process(Ok(WaitOutcome::TimedOut)) => {
            cancellation.observe_timeout();
            let deadline = cleanup_deadline(cleanup_timeout, &job_id, "시간 초과 전체 종료")?;
            return finish_controlled_process(
                job_id,
                job,
                process,
                deadline,
                cancellation.control_snapshot(),
                false,
                fail_stop,
            )
            .await;
        }
        WaitDecision::Cancelled => {
            let deadline = cleanup_deadline(cleanup_timeout, &job_id, "취소 전체 종료")?;
            return finish_controlled_process(
                job_id,
                job,
                process,
                deadline,
                cancellation.control_snapshot(),
                false,
                fail_stop,
            )
            .await;
        }
        WaitDecision::FailStop(deadline) => {
            return finish_controlled_process(
                job_id,
                job,
                process,
                deadline,
                cancellation.control_snapshot(),
                true,
                fail_stop,
            )
            .await;
        }
    };

    let deadline = cleanup_deadline(cleanup_timeout, &job_id, "작업 종료 뒤 정리")?;
    finish_cleaned_started(
        job,
        StartedCompletion {
            job_id,
            process,
            exit,
            control,
            membership_verified: true,
            kill_already_sent,
            cleanup_deadline: deadline,
            daemon_error: false,
        },
        fail_stop,
    )
    .await
}

async fn finish_controlled_process(
    job_id: String,
    job: JobCgroup,
    process: SpawnedProcess,
    cleanup_deadline: MonotonicDeadline,
    control: ControlTriggers,
    daemon_error: bool,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
) -> std::result::Result<CleanedRun, CoreFailure> {
    let stage = match control.first() {
        Some(ControlTrigger::Cancelled) => "취소 전체 종료",
        Some(ControlTrigger::TimedOut) => "시간 초과 전체 종료",
        None => "제어 원인 없는 전체 종료",
    };
    if let Err(error) = job.kill_all() {
        return cleanup_running_job(
            job,
            process,
            RecoveryContext::new(
                job_id,
                cleanup_deadline,
                stage,
                error.to_string(),
                control,
                true,
            ),
            fail_stop,
        )
        .await;
    }
    let exit = match process.reap_after_kill_until(cleanup_deadline).await {
        Ok(exit) => exit,
        Err(error) => {
            return cleanup_running_job(
                job,
                process,
                RecoveryContext::new(
                    job_id,
                    cleanup_deadline,
                    "제어 종료 상태 회수",
                    error.to_string(),
                    control,
                    true,
                ),
                fail_stop,
            )
            .await;
        }
    };
    finish_cleaned_started(
        job,
        StartedCompletion {
            job_id,
            process,
            exit,
            control,
            membership_verified: true,
            kill_already_sent: true,
            cleanup_deadline,
            daemon_error,
        },
        fail_stop,
    )
    .await
}

struct StartedCompletion {
    job_id: String,
    process: SpawnedProcess,
    exit: ProcessExit,
    control: ControlTriggers,
    membership_verified: bool,
    kill_already_sent: bool,
    cleanup_deadline: MonotonicDeadline,
    daemon_error: bool,
}

async fn finish_cleaned_started(
    mut job: JobCgroup,
    completion: StartedCompletion,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
) -> std::result::Result<CleanedRun, CoreFailure> {
    let StartedCompletion {
        job_id,
        process,
        exit,
        control,
        membership_verified,
        kill_already_sent,
        cleanup_deadline,
        daemon_error,
    } = completion;
    let pid = process.pid();
    // 후손이 출력 FD를 잡고 있을 수 있으므로 cgroup 전체를 먼저 비운 뒤 reader를 회수한다.
    let finish_result = finish_job_with_retry(
        &mut job,
        cleanup_deadline,
        kill_already_sent,
        fail_stop,
        &job_id,
        "작업 cgroup 정리",
    )
    .await;
    let output_deadline = match &finish_result {
        Ok((_, _, effective_deadline)) => *effective_deadline,
        Err(_) => fail_stop
            .and_then(|coordinator| coordinator.deadline())
            .unwrap_or(cleanup_deadline),
    };
    let output_result = process.finish_output_until(output_deadline).await;
    match (finish_result, output_result) {
        (Ok((stats, recovered, _)), Ok(output)) => Ok(CleanedRun {
            job_id,
            pid,
            membership_verified,
            evidence: ExecutionEvidence::Started(ProcessEvidence::from(exit)),
            control,
            stats,
            output,
            daemon_error: daemon_error || recovered,
        }),
        (Err(cgroup_error), Ok(_)) => Err(uncertain_failure(
            &job_id,
            "작업 cgroup 정리",
            cgroup_error,
            vec!["작업 cgroup"],
        )),
        (Ok(_), Err(output_error)) => {
            activate_output_failure(fail_stop, &job_id);
            Err(uncertain_failure(
                &job_id,
                "출력 reader 정리",
                output_error,
                vec!["stdout 또는 stderr reader"],
            ))
        }
        (Err(cgroup_error), Err(output_error)) => Err(CoreFailure {
            error: Box::new(run_failed(
                "작업 cgroup 정리",
                &cgroup_error,
                vec![output_error.to_string()],
            )),
            block_future_runs: true,
            cleanup_complete: false,
            report: CleanupFailureReport::new(
                &job_id,
                "작업 cgroup과 출력 reader 정리",
                vec!["작업 cgroup", "stdout 또는 stderr reader"],
                "작업별 deadline 안의 정리 재시도 실패",
            ),
        }),
    }
}

async fn cleanup_job_after_failure(
    job: &mut JobCgroup,
    timeout: Duration,
    job_id: &str,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
    stage: &'static str,
    cause: String,
    isolation_uncertain: bool,
) -> CoreFailure {
    let Some(deadline) = MonotonicDeadline::from_now(timeout) else {
        return uncertain_failure(job_id, stage, Error::CleanupUncertain, vec!["작업 cgroup"]);
    };
    match finish_job_with_retry(job, deadline, false, fail_stop, job_id, stage).await {
        Ok(_) => CoreFailure {
            error: Box::new(run_failed(stage, &cause, Vec::new())),
            block_future_runs: isolation_uncertain,
            cleanup_complete: !isolation_uncertain,
            report: CleanupFailureReport::new(
                job_id,
                stage,
                vec!["pending child 종료 상태"],
                "cgroup 정리는 확인했지만 child 회수 근거가 부족함",
            ),
        },
        Err(error) => {
            uncertain_failure_with_cause(job_id, stage, &cause, error, vec!["작업 cgroup"])
        }
    }
}

async fn cleanup_pending_job(
    job: &mut JobCgroup,
    pending: crate::executor::PendingProcess,
    timeout: Duration,
    job_id: &str,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
    stage: &'static str,
    cause: String,
) -> CoreFailure {
    let Some(deadline) = MonotonicDeadline::from_now(timeout) else {
        return uncertain_failure(
            job_id,
            stage,
            Error::CleanupUncertain,
            vec!["pending child", "작업 cgroup"],
        );
    };
    cleanup_pending_job_until(job, pending, deadline, job_id, fail_stop, stage, cause).await
}

async fn cleanup_pending_job_until(
    job: &mut JobCgroup,
    pending: crate::executor::PendingProcess,
    deadline: MonotonicDeadline,
    job_id: &str,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
    stage: &'static str,
    cause: String,
) -> CoreFailure {
    let mut pending = pending;
    let mut effective_deadline = deadline;
    let mut abort_result = pending.abort_until(deadline).await;
    if abort_result.is_err()
        && let Some(coordinator) = fail_stop
    {
        effective_deadline = coordinator.activate(CleanupFailureReport::new(
            job_id,
            stage,
            vec!["pending child"],
            "process-wide deadline으로 pending child 회수를 재시도함",
        ));
        abort_result = pending.abort_until(effective_deadline).await;
    }
    let finish_result =
        finish_job_with_retry(job, effective_deadline, false, fail_stop, job_id, stage).await;
    let mut cleanup_errors = Vec::new();
    if let Err(error) = &abort_result {
        cleanup_errors.push(error.to_string());
    }
    if let Err(error) = &finish_result {
        cleanup_errors.push(error.to_string());
    }
    let uncertain = abort_result.is_err() || finish_result.is_err();
    CoreFailure {
        error: Box::new(run_failed(stage, &cause, cleanup_errors)),
        block_future_runs: uncertain,
        cleanup_complete: !uncertain,
        report: CleanupFailureReport::new(
            job_id,
            stage,
            vec!["pending child", "작업 cgroup"],
            if uncertain {
                "작업별 deadline 안의 pending child rollback 실패"
            } else {
                "pending child rollback 완료"
            },
        ),
    }
}

struct RecoveryContext {
    job_id: String,
    deadline: MonotonicDeadline,
    stage: &'static str,
    cause: String,
    control: ControlTriggers,
    membership_verified: bool,
}

impl RecoveryContext {
    fn new(
        job_id: String,
        deadline: MonotonicDeadline,
        stage: &'static str,
        cause: impl Into<String>,
        control: ControlTriggers,
        membership_verified: bool,
    ) -> Self {
        Self {
            job_id,
            deadline,
            stage,
            cause: cause.into(),
            control,
            membership_verified,
        }
    }
}

async fn cleanup_running_job(
    mut job: JobCgroup,
    process: SpawnedProcess,
    context: RecoveryContext,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
) -> std::result::Result<CleanedRun, CoreFailure> {
    let RecoveryContext {
        job_id,
        deadline,
        stage,
        cause,
        control,
        membership_verified,
    } = context;
    let pid = process.pid();
    let mut cleanup_errors = Vec::new();
    let mut isolation_uncertain = false;
    let mut effective_deadline = deadline;
    let mut kill_result = job.kill_all();
    if let Err(error) = &kill_result {
        cleanup_errors.push(error.to_string());
        isolation_uncertain = true;
    }
    let mut reap_result = process.reap_after_kill_until(deadline).await;
    if let Err(error) = &reap_result {
        cleanup_errors.push(error.to_string());
        isolation_uncertain = true;
    }
    if isolation_uncertain && let Some(coordinator) = fail_stop {
        effective_deadline = coordinator.activate(CleanupFailureReport::new(
            &job_id,
            stage,
            vec!["direct child", "작업 cgroup"],
            "process-wide deadline으로 whole-cgroup 종료와 child 회수를 재시도함",
        ));
        if kill_result.is_err() {
            kill_result = job.kill_all();
        }
        if reap_result.is_err() {
            reap_result = process.reap_after_kill_until(effective_deadline).await;
        }
    }
    let finish_result = finish_job_with_retry(
        &mut job,
        effective_deadline,
        kill_result.is_ok(),
        fail_stop,
        &job_id,
        stage,
    )
    .await;
    if let Err(error) = &finish_result {
        cleanup_errors.push(error.to_string());
        isolation_uncertain = true;
    }
    let output_deadline = match &finish_result {
        Ok((_, _, finish_deadline)) => *finish_deadline,
        Err(_) => fail_stop
            .and_then(|coordinator| coordinator.deadline())
            .unwrap_or(effective_deadline),
    };
    let output_result = process.finish_output_until(output_deadline).await;
    let output_failed = output_result.is_err();
    if let Err(error) = &output_result {
        cleanup_errors.push(error.to_string());
    }

    match (kill_result, reap_result, finish_result, output_result) {
        (Ok(()), Ok(exit), Ok((stats, _, _)), Ok(output)) => {
            tracing::warn!(stage, cause = %cause, "내부 오류 뒤 안전한 정리를 완료했습니다");
            Ok(CleanedRun {
                job_id,
                pid,
                membership_verified,
                evidence: ExecutionEvidence::Started(ProcessEvidence::from(exit)),
                control,
                stats,
                output,
                daemon_error: true,
            })
        }
        _ => Err(CoreFailure {
            error: Box::new(run_failed(stage, &cause, cleanup_errors)),
            block_future_runs: isolation_uncertain || output_failed,
            cleanup_complete: false,
            report: CleanupFailureReport::new(
                &job_id,
                stage,
                vec!["direct child", "작업 cgroup", "stdout 또는 stderr reader"],
                "작업별 deadline 안의 정리 재시도 실패",
            ),
        }),
    }
}

async fn finish_job_with_retry(
    job: &mut JobCgroup,
    deadline: MonotonicDeadline,
    kill_already_sent: bool,
    fail_stop: Option<&Arc<FailStopCoordinator>>,
    job_id: &str,
    stage: &'static str,
) -> std::result::Result<(JobStats, bool, MonotonicDeadline), CgroupError> {
    let first = if kill_already_sent {
        job.finish_after_kill_until(deadline).await
    } else {
        job.finish_until(deadline).await
    };
    let first_error = match first {
        Ok(stats) => return Ok((stats, false, deadline)),
        Err(error) => error,
    };
    let Some(coordinator) = fail_stop else {
        return Err(first_error);
    };
    let retry_deadline = coordinator.activate(CleanupFailureReport::new(
        job_id,
        stage,
        vec!["작업 cgroup"],
        "process-wide deadline으로 whole-cgroup 정리를 재시도함",
    ));
    match job.finish_until(retry_deadline).await {
        Ok(stats) => {
            tracing::error!(
                task_id = job_id,
                stage,
                retry = "성공",
                "fail-stop 정리 재시도에서 작업 cgroup을 회수했습니다"
            );
            Ok((stats, true, retry_deadline))
        }
        Err(retry_error) => Err(CgroupError::CleanupCombined {
            primary: first_error.to_string(),
            cleanup: retry_error.to_string(),
        }),
    }
}

fn activate_output_failure(fail_stop: Option<&Arc<FailStopCoordinator>>, job_id: &str) {
    if let Some(coordinator) = fail_stop {
        coordinator.activate(CleanupFailureReport::new(
            job_id,
            "출력 reader 정리",
            vec!["stdout 또는 stderr reader"],
            "reader 취소와 join 뒤 결과 근거를 회수하지 못함",
        ));
    }
}

async fn wait_for_fail_stop(fail_stop: Option<&Arc<FailStopCoordinator>>) -> MonotonicDeadline {
    match fail_stop {
        Some(coordinator) => coordinator.activated().await,
        None => pending().await,
    }
}

fn cleanup_deadline(
    timeout: Duration,
    job_id: &str,
    stage: &'static str,
) -> std::result::Result<MonotonicDeadline, CoreFailure> {
    MonotonicDeadline::from_now(timeout).ok_or_else(|| {
        uncertain_failure(
            job_id,
            stage,
            Error::CleanupUncertain,
            vec!["cleanup deadline"],
        )
    })
}

fn fail_stop_before_side_effect(job_id: &str, _deadline: MonotonicDeadline) -> CoreFailure {
    CoreFailure {
        error: Box::new(Error::CleanupUncertain),
        block_future_runs: true,
        cleanup_complete: true,
        report: CleanupFailureReport::new(
            job_id,
            "실행 전 fail-stop 확인",
            Vec::new(),
            "side effect 없음",
        ),
    }
}

async fn cleanup_job_for_fail_stop(
    mut job: JobCgroup,
    deadline: MonotonicDeadline,
    job_id: &str,
    stage: &'static str,
) -> CoreFailure {
    match job.finish_until(deadline).await {
        Ok(_) => fail_stop_before_side_effect(job_id, deadline),
        Err(error) => uncertain_failure(job_id, stage, error, vec!["작업 cgroup"]),
    }
}

fn uncertain_failure(
    job_id: &str,
    stage: &'static str,
    error: impl std::fmt::Display,
    uncleaned: Vec<&'static str>,
) -> CoreFailure {
    CoreFailure {
        error: Box::new(run_failed(stage, &error, Vec::new())),
        block_future_runs: true,
        cleanup_complete: false,
        report: CleanupFailureReport::new(
            job_id,
            stage,
            uncleaned,
            "작업별 deadline 안의 정리 재시도 실패",
        ),
    }
}

fn uncertain_failure_with_cause(
    job_id: &str,
    stage: &'static str,
    cause: &str,
    cleanup: impl std::fmt::Display,
    uncleaned: Vec<&'static str>,
) -> CoreFailure {
    CoreFailure {
        error: Box::new(run_failed(stage, &cause, vec![cleanup.to_string()])),
        block_future_runs: true,
        cleanup_complete: false,
        report: CleanupFailureReport::new(
            job_id,
            stage,
            uncleaned,
            "작업별 deadline 안의 rollback 실패",
        ),
    }
}

fn run_failed(
    stage: &'static str,
    cause: &dyn std::fmt::Display,
    cleanup_errors: Vec<String>,
) -> Error {
    Error::RunFailed {
        stage,
        cause: cause.to_string(),
        cleanup_errors,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::capacity::{TaskCapacity, TaskCapacitySettings};

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum InjectedCleanupFault {
        PendingCloneAbort,
        ExecGateCleanup,
        CgroupKill,
        DirectChildReap,
        PopulatedZero,
        Statistics,
        CgroupRemoval,
        StdoutReader,
        StderrReader,
    }

    impl InjectedCleanupFault {
        fn stage(self) -> &'static str {
            match self {
                Self::PendingCloneAbort => "pending clone3 child 중단",
                Self::ExecGateCleanup => "exec gate 실패 뒤 정리",
                Self::CgroupKill => "cgroup.kill",
                Self::DirectChildReap => "direct child 회수",
                Self::PopulatedZero => "populated 0 확인",
                Self::Statistics => "통계 수집",
                Self::CgroupRemoval => "작업 cgroup 제거",
                Self::StdoutReader => "stdout reader 회수",
                Self::StderrReader => "stderr reader 회수",
            }
        }

        fn uncleaned(self) -> Vec<&'static str> {
            match self {
                Self::PendingCloneAbort => vec!["pending child", "작업 cgroup"],
                Self::ExecGateCleanup => vec!["exec gate child", "작업 cgroup"],
                Self::CgroupKill | Self::PopulatedZero | Self::Statistics | Self::CgroupRemoval => {
                    vec!["작업 cgroup"]
                }
                Self::DirectChildReap => vec!["direct child"],
                Self::StdoutReader => vec!["stdout reader"],
                Self::StderrReader => vec!["stderr reader"],
            }
        }
    }

    const CLEANUP_FAULTS: [InjectedCleanupFault; 9] = [
        InjectedCleanupFault::PendingCloneAbort,
        InjectedCleanupFault::ExecGateCleanup,
        InjectedCleanupFault::CgroupKill,
        InjectedCleanupFault::DirectChildReap,
        InjectedCleanupFault::PopulatedZero,
        InjectedCleanupFault::Statistics,
        InjectedCleanupFault::CgroupRemoval,
        InjectedCleanupFault::StdoutReader,
        InjectedCleanupFault::StderrReader,
    ];

    #[test]
    fn injected_runtime_cleanup_faults_fail_stop_without_finished_or_permit_reuse() {
        for fault in CLEANUP_FAULTS {
            let clock_calls = Arc::new(AtomicUsize::new(0));
            let base = Instant::now();
            let clock = {
                let clock_calls = Arc::clone(&clock_calls);
                Arc::new(move || {
                    clock_calls.fetch_add(1, Ordering::SeqCst);
                    base
                })
            };
            let fail_stop = FailStopCoordinator::with_test_clock(
                crate::fail_stop::FailStopSettings::new(Duration::from_secs(5)).unwrap(),
                clock,
            );
            let active = fail_stop
                .try_admit()
                .unwrap()
                .register(format!("task-{fault:?}"))
                .unwrap();
            let capacity = Arc::new(TaskCapacity::new(TaskCapacitySettings::new(1).unwrap()));
            let permit = capacity.try_acquire().unwrap();
            let lifecycle = SingleTaskLifecycle::running(
                format!("task-{fault:?}"),
                "submitted".to_owned(),
                "started".to_owned(),
                base,
            );
            let failure = uncertain_failure(
                &format!("task-{fault:?}"),
                fault.stage(),
                "injected secret command and environment",
                fault.uncleaned(),
            );

            assert!(failure.block_future_runs);
            assert!(!failure.cleanup_complete);
            assert!(!failure.report.stage.contains("secret"));
            assert!(
                failure
                    .report
                    .uncleaned
                    .iter()
                    .all(|value| !value.contains("secret"))
            );
            let deadline = fail_stop.activate(failure.report);
            let repeated = fail_stop.activate(CleanupFailureReport::new(
                "later-task",
                "later failure",
                vec!["작업 cgroup"],
                "retry",
            ));
            permit.retain_for_fail_stop();
            drop(active);

            assert_eq!(deadline, repeated);
            assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
            assert!(fail_stop.try_admit().is_none());
            assert_eq!(fail_stop.active_count(), 1);
            assert_eq!(capacity.retained_for_fail_stop(), 1);
            assert!(capacity.try_acquire().is_none());
            assert!(matches!(lifecycle.snapshot(), TaskPayload::Running { .. }));
        }
    }

    #[test]
    fn concurrent_cleanup_faults_keep_every_unresolved_execution_active() {
        let fail_stop = FailStopCoordinator::new(
            crate::fail_stop::FailStopSettings::new(Duration::from_secs(5)).unwrap(),
        );
        let active = (0..3)
            .map(|index| {
                fail_stop
                    .try_admit()
                    .unwrap()
                    .register(format!("task-{index}"))
                    .unwrap()
            })
            .collect::<Vec<_>>();

        for (index, fault) in CLEANUP_FAULTS.into_iter().take(3).enumerate() {
            let failure = uncertain_failure(
                &format!("task-{index}"),
                fault.stage(),
                "injected",
                fault.uncleaned(),
            );
            fail_stop.activate(failure.report);
        }
        drop(active);

        assert!(fail_stop.is_fail_stopping());
        assert_eq!(fail_stop.active_count(), 3);
        assert!(fail_stop.try_admit().is_none());
    }
}
