//! 검증된 자원 예산으로 atomic runner를 실행하고 단일 task lifecycle을 완료한다.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::cancellation::CancellationRuntime;
use crate::cgroup::{CgroupError, CgroupManager, JobCgroup, JobStats};
use crate::executor::{
    ExecFailure, PreparedCommand, ProcessExit, SpawnOutcome, SpawnedProcess, WaitOutcome,
    spawn_in_cgroup,
};
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
    cleanup_uncertain: AtomicBool,
}

#[derive(Debug)]
pub(crate) struct TaskRunFailure {
    error: Error,
    capacity_reusable: bool,
}

impl TaskRunFailure {
    fn with_reusable_capacity(error: Error) -> Self {
        Self {
            error,
            capacity_reusable: true,
        }
    }

    pub(crate) fn capacity_reusable(&self) -> bool {
        self.capacity_reusable
    }

    pub(crate) fn into_error(self) -> Error {
        self.error
    }
}

impl TaskRunner {
    /// preflight 성공 토큰 없이는 실행기를 만들 수 없다.
    pub(crate) fn initialize(environment: VerifiedEnvironment) -> Result<Self> {
        Ok(Self {
            manager: CgroupManager::initialize(environment)?,
            cleanup_uncertain: AtomicBool::new(false),
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
        if self.cleanup_uncertain.load(Ordering::Acquire) {
            return Err(TaskRunFailure::with_reusable_capacity(
                Error::CleanupUncertain,
            ));
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

        let cleaned = match execute(&self.manager, execution, cancellation, || {
            // execve 성공을 확인한 뒤 실제로 완료할 같은 lifecycle의 snapshot만 공개한다.
            let _ = running_sender.send(lifecycle.snapshot());
        })
        .await
        {
            Ok(cleaned) => cleaned,
            Err(failure) => {
                if failure.block_future_runs {
                    self.cleanup_uncertain.store(true, Ordering::Release);
                }
                return Err(TaskRunFailure {
                    error: failure.error,
                    capacity_reusable: !failure.block_future_runs,
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
        self.cleanup_uncertain.load(Ordering::Acquire)
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
    error: Error,
    block_future_runs: bool,
}

impl CoreFailure {
    fn before_job(error: CgroupError) -> Self {
        let block_future_runs = matches!(&error, CgroupError::CleanupCombined { .. });
        Self {
            error: error.into(),
            block_future_runs,
        }
    }

    pub(crate) fn into_error(self) -> Error {
        self.error
    }
}

pub(crate) async fn execute<F>(
    manager: &CgroupManager,
    config: ExecutionConfig,
    cancellation: CancellationRuntime,
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
    let job = manager
        .create_job(&job_id, limits)
        .map_err(CoreFailure::before_job)?;

    let pending = match spawn_in_cgroup(&prepared, job.raw_fd(), capture_limits) {
        Ok(pending) => pending,
        Err(error) => {
            drop(prepared);
            return Err(cleanup_job_after_failure(
                job,
                cleanup_timeout,
                "프로세스 생성",
                error.to_string(),
                false,
            )
            .await);
        }
    };
    match job.contains_pid(pending.pid()) {
        Ok(true) => {}
        Ok(false) => {
            drop(prepared);
            return Err(cleanup_pending_job(
                job,
                pending,
                cleanup_timeout,
                "exec 전 cgroup 소속 재확인",
                "PID가 작업 cgroup에서 확인되지 않았습니다".to_owned(),
            )
            .await);
        }
        Err(error) => {
            drop(prepared);
            return Err(cleanup_pending_job(
                job,
                pending,
                cleanup_timeout,
                "exec 전 cgroup 소속 재확인",
                error.to_string(),
            )
            .await);
        }
    }
    let spawn = match pending.start() {
        Ok(spawn) => spawn,
        Err(error) => {
            let isolation_uncertain = matches!(&error, crate::executor::ExecutorError::Wait(_));
            drop(prepared);
            return Err(cleanup_job_after_failure(
                job,
                cleanup_timeout,
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
            finish_exec_failure(job_id, job, failure, cleanup_timeout).await
        }
        SpawnOutcome::Started(process) => {
            finish_started_process(
                job_id,
                job,
                process,
                wall_timeout,
                cleanup_timeout,
                cancellation,
                on_started,
            )
            .await
        }
    }
}

async fn finish_exec_failure(
    job_id: String,
    job: JobCgroup,
    failure: ExecFailure,
    cleanup_timeout: Duration,
) -> std::result::Result<CleanedRun, CoreFailure> {
    match job.finish(cleanup_timeout).await {
        Ok(stats) => Ok(CleanedRun {
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
        Err(error) => Err(CoreFailure {
            error: run_failed("exec 실패 뒤 작업 cgroup 정리", &error, Vec::new()),
            block_future_runs: true,
        }),
    }
}

async fn finish_started_process<F>(
    job_id: String,
    job: JobCgroup,
    process: SpawnedProcess,
    wall_timeout: Duration,
    cleanup_timeout: Duration,
    cancellation: CancellationRuntime,
    on_started: F,
) -> std::result::Result<CleanedRun, CoreFailure>
where
    F: FnOnce(),
{
    on_started();
    let wait_outcome = tokio::select! {
        biased;
        _ = cancellation.cancelled() => None,
        outcome = process.wait_for(wall_timeout) => Some(outcome),
    };

    let (control, exit, kill_already_sent) = match wait_outcome {
        Some(Err(error)) => {
            return cleanup_running_job(
                job,
                process,
                RecoveryContext::new(
                    job_id,
                    cleanup_timeout,
                    "target 종료 대기",
                    error.to_string(),
                    cancellation.control_snapshot(),
                    true,
                ),
            )
            .await;
        }
        Some(Ok(WaitOutcome::Exited(exit))) if cancellation.close_without_control() => {
            (ControlTriggers::none(), exit, false)
        }
        Some(Ok(WaitOutcome::Exited(exit))) => {
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
        Some(Ok(WaitOutcome::TimedOut)) => {
            cancellation.observe_timeout();
            return finish_controlled_process(
                job_id,
                job,
                process,
                cleanup_timeout,
                cancellation.control_snapshot(),
            )
            .await;
        }
        None => {
            return finish_controlled_process(
                job_id,
                job,
                process,
                cleanup_timeout,
                cancellation.control_snapshot(),
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
            kill_already_sent,
            cleanup_timeout,
        },
    )
    .await
}

async fn finish_controlled_process(
    job_id: String,
    job: JobCgroup,
    process: SpawnedProcess,
    cleanup_timeout: Duration,
    control: ControlTriggers,
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
                cleanup_timeout,
                stage,
                error.to_string(),
                control,
                true,
            ),
        )
        .await;
    }
    let exit = match process.reap_after_kill(cleanup_timeout).await {
        Ok(exit) => exit,
        Err(error) => {
            return cleanup_running_job(
                job,
                process,
                RecoveryContext::new(
                    job_id,
                    cleanup_timeout,
                    "제어 종료 상태 회수",
                    error.to_string(),
                    control,
                    true,
                ),
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
            cleanup_timeout,
        },
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
    cleanup_timeout: Duration,
}

async fn finish_cleaned_started(
    job: JobCgroup,
    completion: StartedCompletion,
) -> std::result::Result<CleanedRun, CoreFailure> {
    let StartedCompletion {
        job_id,
        process,
        exit,
        control,
        membership_verified,
        kill_already_sent,
        cleanup_timeout,
    } = completion;
    let pid = process.pid();
    // 후손이 출력 FD를 잡고 있을 수 있으므로 cgroup 전체를 먼저 비운 뒤 reader를 회수한다.
    let finish_result = if kill_already_sent {
        job.finish_after_kill(cleanup_timeout).await
    } else {
        job.finish(cleanup_timeout).await
    };
    let output_result = process.finish_output(cleanup_timeout).await;
    match (finish_result, output_result) {
        (Ok(stats), Ok(output)) => Ok(CleanedRun {
            job_id,
            pid,
            membership_verified,
            evidence: ExecutionEvidence::Started(ProcessEvidence::from(exit)),
            control,
            stats,
            output,
            daemon_error: false,
        }),
        (Err(cgroup_error), Ok(_)) => Err(CoreFailure {
            error: run_failed("작업 cgroup 정리", &cgroup_error, Vec::new()),
            block_future_runs: true,
        }),
        (Ok(_), Err(output_error)) => Err(CoreFailure {
            error: run_failed("출력 reader 정리", &output_error, Vec::new()),
            block_future_runs: false,
        }),
        (Err(cgroup_error), Err(output_error)) => Err(CoreFailure {
            error: run_failed(
                "작업 cgroup 정리",
                &cgroup_error,
                vec![output_error.to_string()],
            ),
            block_future_runs: true,
        }),
    }
}

async fn cleanup_job_after_failure(
    job: JobCgroup,
    timeout: Duration,
    stage: &'static str,
    cause: String,
    isolation_uncertain: bool,
) -> CoreFailure {
    match job.finish(timeout).await {
        Ok(_) => CoreFailure {
            error: run_failed(stage, &cause, Vec::new()),
            block_future_runs: isolation_uncertain,
        },
        Err(error) => CoreFailure {
            error: run_failed(stage, &cause, vec![error.to_string()]),
            block_future_runs: true,
        },
    }
}

async fn cleanup_pending_job(
    job: JobCgroup,
    pending: crate::executor::PendingProcess,
    timeout: Duration,
    stage: &'static str,
    cause: String,
) -> CoreFailure {
    let abort_result = pending.abort();
    let finish_result = job.finish(timeout).await;
    let mut cleanup_errors = Vec::new();
    if let Err(error) = &abort_result {
        cleanup_errors.push(error.to_string());
    }
    if let Err(error) = &finish_result {
        cleanup_errors.push(error.to_string());
    }
    CoreFailure {
        error: run_failed(stage, &cause, cleanup_errors),
        block_future_runs: abort_result.is_err() || finish_result.is_err(),
    }
}

struct RecoveryContext {
    job_id: String,
    timeout: Duration,
    stage: &'static str,
    cause: String,
    control: ControlTriggers,
    membership_verified: bool,
}

impl RecoveryContext {
    fn new(
        job_id: String,
        timeout: Duration,
        stage: &'static str,
        cause: impl Into<String>,
        control: ControlTriggers,
        membership_verified: bool,
    ) -> Self {
        Self {
            job_id,
            timeout,
            stage,
            cause: cause.into(),
            control,
            membership_verified,
        }
    }
}

async fn cleanup_running_job(
    job: JobCgroup,
    process: SpawnedProcess,
    context: RecoveryContext,
) -> std::result::Result<CleanedRun, CoreFailure> {
    let RecoveryContext {
        job_id,
        timeout,
        stage,
        cause,
        control,
        membership_verified,
    } = context;
    let pid = process.pid();
    let mut cleanup_errors = Vec::new();
    let mut isolation_uncertain = false;
    let kill_result = job.kill_all();
    if let Err(error) = &kill_result {
        cleanup_errors.push(error.to_string());
        isolation_uncertain = true;
    }
    let reap_result = process.reap_after_kill(timeout).await;
    if let Err(error) = &reap_result {
        cleanup_errors.push(error.to_string());
        isolation_uncertain = true;
    }
    let finish_result = if kill_result.is_ok() {
        job.finish_after_kill(timeout).await
    } else {
        job.finish(timeout).await
    };
    if let Err(error) = &finish_result {
        cleanup_errors.push(error.to_string());
        isolation_uncertain = true;
    }
    let output_result = process.finish_output(timeout).await;
    if let Err(error) = &output_result {
        cleanup_errors.push(error.to_string());
    }

    match (kill_result, reap_result, finish_result, output_result) {
        (Ok(()), Ok(exit), Ok(stats), Ok(output)) => {
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
            error: run_failed(stage, &cause, cleanup_errors),
            block_future_runs: isolation_uncertain,
        }),
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
