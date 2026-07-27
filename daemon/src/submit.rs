//! protocol v1 submit 검증부터 멱등 예약과 Runner 상태 저장까지 한 경계에서 조정한다.

#[path = "registry.rs"]
mod registry;

#[cfg(any(target_os = "linux", test))]
use std::future::Future;
#[cfg(any(target_os = "linux", test))]
use std::time::{Duration, Instant};

use thiserror::Error;
#[cfg(any(target_os = "linux", test))]
use tokio::sync::oneshot;

#[cfg(any(target_os = "linux", test))]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use self::registry::MonotonicClock;
#[cfg(any(target_os = "linux", test))]
use self::registry::{RegistryClock, SubmitExecutionOwner, SubmitReservation, TaskRegistry};
#[cfg(any(target_os = "linux", test))]
pub(crate) use self::registry::{RegistryError, SubmitFailure, SubmitObservation};
#[cfg(any(target_os = "linux", test))]
use crate::cancellation::{CancellationRuntime, RunningCancellation, cancellation_channel};
#[cfg(any(target_os = "linux", test))]
use crate::capacity::{TaskCapacity, TaskCapacityPermit, TaskCapacitySettings};
#[cfg(target_os = "linux")]
use crate::preflight::VerifiedEnvironment;
#[cfg(any(target_os = "linux", test))]
use crate::protocol::{ErrorCode, ResourceLimits, TaskPayload};
use crate::protocol::{PROTOCOL_VERSION, Request, SubmitTaskPayload};
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};
#[cfg(target_os = "linux")]
use crate::runner::{CompletedTask, TaskRunConfig, TaskRunner};

#[cfg(any(target_os = "linux", test))]
const CAPACITY_EXHAUSTED_MESSAGE: &str = "all task execution slots are in use";

/// 원자적 예약을 얻은 submit 조정 경로만 Runner 호출 권한을 만들 수 있다.
#[cfg(target_os = "linux")]
pub(crate) struct RunnerPermit(());

#[cfg(target_os = "linux")]
impl RunnerPermit {
    fn new() -> Self {
        Self(())
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone)]
pub(crate) struct SubmitMetadata {
    pub(crate) task_id: String,
    pub(crate) submitted_at: String,
    pub(crate) started_at: String,
    pub(crate) started_monotonic: Instant,
    pub(crate) cleanup_timeout: Duration,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmitOutcome {
    pub(crate) request_id: String,
    pub(crate) task_id: String,
    pub(crate) effective_limits: ResourceLimits,
    pub(crate) observation: SubmitObservation,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum SubmitError {
    #[error(transparent)]
    Validation(#[from] SubmitValidationError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("submit 실행 조정 task가 최초 공개 상태를 보내지 못했습니다")]
    CoordinatorStopped,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
struct SubmitExecutionConfig {
    task_id: String,
    submitted_at: String,
    started_at: String,
    started_monotonic: Instant,
    cleanup_timeout: Duration,
    command: crate::protocol::CommandSpec,
    budget: ResourceBudget,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
enum ExecutionCompletion {
    #[cfg(target_os = "linux")]
    Real(CompletedTask),
    #[cfg(test)]
    Test(TaskPayload),
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
struct ExecutionFailure {
    submit: SubmitFailure,
    capacity_reusable: bool,
}

#[cfg(any(target_os = "linux", test))]
impl ExecutionFailure {
    fn new(submit: SubmitFailure, capacity_reusable: bool) -> Self {
        Self {
            submit,
            capacity_reusable,
        }
    }
}

/// UDS handler가 사용할 단일 submit 진입점이다. Registry와 Runner는 외부에 따로 노출하지 않는다.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct SubmitCoordinator {
    registry: TaskRegistry<MonotonicClock>,
    runner: Arc<TaskRunner>,
    capacity: Arc<TaskCapacity>,
}

#[cfg(target_os = "linux")]
impl SubmitCoordinator {
    pub(crate) fn initialize(
        environment: VerifiedEnvironment,
        capacity_settings: TaskCapacitySettings,
    ) -> crate::Result<Self> {
        Ok(Self {
            registry: TaskRegistry::new(),
            runner: Arc::new(TaskRunner::initialize(environment)?),
            capacity: Arc::new(TaskCapacity::new(capacity_settings)),
        })
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "UDS handler가 다음 단계에서 이 단일 진입점만 호출합니다"
        )
    )]
    pub(crate) async fn submit<F>(
        &self,
        request: Request,
        metadata: SubmitMetadata,
        finished_time: F,
    ) -> Result<SubmitOutcome, SubmitError>
    where
        F: FnOnce() -> (String, Instant) + Send + 'static,
    {
        let (request_id, validated) = ValidatedSubmit::try_from_request(request)?;
        self.submit_validated(request_id, validated, metadata, finished_time)
            .await
    }

    pub(crate) async fn submit_validated<F>(
        &self,
        request_id: String,
        validated: ValidatedSubmit,
        metadata: SubmitMetadata,
        finished_time: F,
    ) -> Result<SubmitOutcome, SubmitError>
    where
        F: FnOnce() -> (String, Instant) + Send + 'static,
    {
        let runner = Arc::clone(&self.runner);
        coordinate_validated_submit(
            self.registry.clone(),
            Arc::clone(&self.capacity),
            request_id,
            validated,
            metadata,
            move |config, running_sender, cancellation| async move {
                runner
                    .run_task(
                        RunnerPermit::new(),
                        TaskRunConfig {
                            task_id: config.task_id,
                            submitted_at: config.submitted_at,
                            started_at: config.started_at,
                            started_monotonic: config.started_monotonic,
                            cleanup_timeout: config.cleanup_timeout,
                            command: config.command,
                            budget: config.budget,
                        },
                        running_sender,
                        cancellation,
                        finished_time,
                    )
                    .await
                    .map(ExecutionCompletion::Real)
                    .map_err(|error| {
                        let capacity_reusable = error.capacity_reusable();
                        ExecutionFailure::new(
                            SubmitFailure::new(
                                ErrorCode::InternalError,
                                error.into_error().to_string(),
                            ),
                            capacity_reusable,
                        )
                    })
            },
        )
        .await
    }

    pub(crate) async fn cancel(&self, task_id: &str) -> Result<TaskPayload, RegistryError> {
        let finished = self.registry.request_cancel(task_id)?.wait().await;
        Ok(finished)
    }

    pub(crate) async fn wait_idle(&self) {
        self.capacity.wait_idle().await;
    }

    pub(crate) fn snapshot(&self, task_id: &str) -> Result<Option<TaskPayload>, RegistryError> {
        self.registry.snapshot(task_id)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "동일 submit 응답과 진단 경로가 다음 단계에서 사용합니다"
        )
    )]
    pub(crate) fn snapshot_by_client_request_id(
        &self,
        client_request_id: &str,
    ) -> Result<Option<TaskPayload>, RegistryError> {
        self.registry
            .snapshot_by_client_request_id(client_request_id)
    }
}

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "단위 시험은 raw Request 경계를, production handler는 검증된 경계를 사용합니다"
    )
)]
async fn coordinate_submit<C, E, Fut>(
    registry: TaskRegistry<C>,
    capacity: Arc<TaskCapacity>,
    request: Request,
    metadata: SubmitMetadata,
    executor: E,
) -> Result<SubmitOutcome, SubmitError>
where
    C: RegistryClock + Send + 'static,
    E: FnOnce(SubmitExecutionConfig, oneshot::Sender<TaskPayload>, CancellationRuntime) -> Fut
        + Send
        + 'static,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>> + Send + 'static,
{
    // 검증은 Registry 예약과 Runner side effect보다 먼저 끝낸다.
    let (request_id, validated) = ValidatedSubmit::try_from_request(request)?;
    coordinate_validated_submit(
        registry, capacity, request_id, validated, metadata, executor,
    )
    .await
}

#[cfg(any(target_os = "linux", test))]
async fn coordinate_validated_submit<C, E, Fut>(
    registry: TaskRegistry<C>,
    capacity: Arc<TaskCapacity>,
    request_id: String,
    validated: ValidatedSubmit,
    metadata: SubmitMetadata,
    executor: E,
) -> Result<SubmitOutcome, SubmitError>
where
    C: RegistryClock + Send + 'static,
    E: FnOnce(SubmitExecutionConfig, oneshot::Sender<TaskPayload>, CancellationRuntime) -> Fut
        + Send
        + 'static,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>> + Send + 'static,
{
    let effective_limits = validated.payload().limits.clone();
    let reservation = registry.reserve_submit(validated, metadata.task_id.clone())?;

    let (task_id, observation) = match reservation {
        SubmitReservation::Existing(waiter) => {
            let task_id = waiter.task_id().to_owned();
            (task_id, waiter.wait().await)
        }
        SubmitReservation::Owner(owner) => {
            let task_id = owner.task_id().to_owned();
            let Some(capacity_permit) = capacity.try_acquire() else {
                let observation = owner.rollback_before_running(SubmitFailure::new(
                    ErrorCode::CapacityExhausted,
                    CAPACITY_EXHAUSTED_MESSAGE,
                ))?;
                return Ok(SubmitOutcome {
                    request_id,
                    task_id,
                    effective_limits,
                    observation,
                });
            };
            let config = SubmitExecutionConfig {
                task_id: task_id.clone(),
                submitted_at: metadata.submitted_at,
                started_at: metadata.started_at,
                started_monotonic: metadata.started_monotonic,
                cleanup_timeout: metadata.cleanup_timeout,
                command: owner.request().payload().command.clone(),
                budget: owner.request().budget().clone(),
            };
            let (initial_sender, initial_receiver) = oneshot::channel();
            tokio::spawn(run_owner(
                owner,
                config,
                executor,
                initial_sender,
                capacity_permit,
            ));
            let observation = initial_receiver
                .await
                .map_err(|_| SubmitError::CoordinatorStopped)?;
            (task_id, observation)
        }
    };

    Ok(SubmitOutcome {
        request_id,
        task_id,
        effective_limits,
        observation,
    })
}

#[cfg(any(target_os = "linux", test))]
async fn run_owner<C, E, Fut>(
    owner: SubmitExecutionOwner<C>,
    config: SubmitExecutionConfig,
    executor: E,
    initial_sender: oneshot::Sender<SubmitObservation>,
    capacity_permit: TaskCapacityPermit,
) where
    C: RegistryClock,
    E: FnOnce(SubmitExecutionConfig, oneshot::Sender<TaskPayload>, CancellationRuntime) -> Fut,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>>,
{
    let (running_sender, mut running_receiver) = oneshot::channel();
    let (cancellation_runtime, running_cancellation) = cancellation_channel();
    let execution = executor(config, running_sender, cancellation_runtime);
    tokio::pin!(execution);
    tokio::select! {
        biased;
        running = &mut running_receiver => {
            match running {
                Ok(running) => run_after_running(
                    owner,
                    running,
                    execution,
                    running_cancellation,
                    initial_sender,
                    capacity_permit,
                ).await,
                Err(_) => finish_without_running(
                    owner,
                    execution.await,
                    initial_sender,
                    capacity_permit,
                ),
            }
        }
        completion = &mut execution => {
            match running_receiver.try_recv() {
                Ok(running) => finish_after_running(
                    owner,
                    running,
                    running_cancellation,
                    completion,
                    initial_sender,
                    capacity_permit,
                ),
                Err(_) => finish_without_running(
                    owner,
                    completion,
                    initial_sender,
                    capacity_permit,
                ),
            }
        }
    }
}

#[cfg(any(target_os = "linux", test))]
async fn run_after_running<C, Fut>(
    owner: SubmitExecutionOwner<C>,
    running: TaskPayload,
    execution: Fut,
    cancellation: RunningCancellation,
    initial_sender: oneshot::Sender<SubmitObservation>,
    capacity_permit: TaskCapacityPermit,
) where
    C: RegistryClock,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>>,
{
    let running = match owner.publish_running_with_cancellation(running, cancellation) {
        Ok(running) => running,
        Err(error) => {
            let failure = SubmitFailure::new(ErrorCode::InternalError, error.to_string());
            let observation = owner.fail(failure);
            let _ = initial_sender.send(observation);
            capacity_permit.retain_for_fail_stop();
            let _ = execution.await;
            return;
        }
    };
    // 빠른 종료가 뒤따라도 최초 submit 호출에는 RUNNING을 고정해 전달한다.
    let _ = initial_sender.send(SubmitObservation::Task(running));
    match execution.await {
        Ok(completed) => {
            if finish_owner(owner, completed).is_err() {
                capacity_permit.retain_for_fail_stop();
            }
        }
        Err(failure) => {
            owner.fail(failure.submit);
            capacity_permit.retain_for_fail_stop();
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn finish_after_running<C>(
    owner: SubmitExecutionOwner<C>,
    running: TaskPayload,
    cancellation: RunningCancellation,
    completion: Result<ExecutionCompletion, ExecutionFailure>,
    initial_sender: oneshot::Sender<SubmitObservation>,
    capacity_permit: TaskCapacityPermit,
) where
    C: RegistryClock,
{
    let running = match owner.publish_running_with_cancellation(running, cancellation) {
        Ok(running) => running,
        Err(error) => {
            let failure = SubmitFailure::new(ErrorCode::InternalError, error.to_string());
            let observation = owner.fail(failure);
            let _ = initial_sender.send(observation);
            capacity_permit.retain_for_fail_stop();
            return;
        }
    };
    let _ = initial_sender.send(SubmitObservation::Task(running));
    match completion {
        Ok(completed) => {
            if finish_owner(owner, completed).is_err() {
                capacity_permit.retain_for_fail_stop();
            }
        }
        Err(failure) => {
            owner.fail(failure.submit);
            capacity_permit.retain_for_fail_stop();
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn finish_without_running<C>(
    owner: SubmitExecutionOwner<C>,
    completion: Result<ExecutionCompletion, ExecutionFailure>,
    initial_sender: oneshot::Sender<SubmitObservation>,
    capacity_permit: TaskCapacityPermit,
) where
    C: RegistryClock,
{
    let observation = match completion {
        Ok(completed) => match finish_owner(owner, completed) {
            Ok(finished) => SubmitObservation::Task(finished),
            Err(error) => {
                capacity_permit.retain_for_fail_stop();
                return send_initial_failure(initial_sender, error);
            }
        },
        Err(failure) => {
            if failure.capacity_reusable {
                match owner.rollback_before_running(failure.submit) {
                    Ok(observation) => observation,
                    Err(error) => {
                        capacity_permit.retain_for_fail_stop();
                        return send_initial_failure(initial_sender, error);
                    }
                }
            } else {
                capacity_permit.retain_for_fail_stop();
                owner.fail(failure.submit)
            }
        }
    };
    let _ = initial_sender.send(observation);
}

#[cfg(any(target_os = "linux", test))]
fn send_initial_failure(initial_sender: oneshot::Sender<SubmitObservation>, error: RegistryError) {
    let _ = initial_sender.send(SubmitObservation::Failed(SubmitFailure::new(
        ErrorCode::InternalError,
        error.to_string(),
    )));
}

#[cfg(any(target_os = "linux", test))]
fn finish_owner<C>(
    owner: SubmitExecutionOwner<C>,
    completion: ExecutionCompletion,
) -> Result<TaskPayload, RegistryError>
where
    C: RegistryClock,
{
    match completion {
        #[cfg(target_os = "linux")]
        ExecutionCompletion::Real(completed) => owner.finish(completed),
        #[cfg(test)]
        ExecutionCompletion::Test(finished) => owner.finish_for_test(finished),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedSubmit {
    payload: SubmitTaskPayload,
    budget: ResourceBudget,
}

impl ValidatedSubmit {
    pub(crate) fn try_from_request(
        request: Request,
    ) -> Result<(String, Self), SubmitValidationError> {
        let Request::SubmitTask {
            protocol_version,
            request_id,
            payload,
        } = request
        else {
            return Err(SubmitValidationError::NotSubmitTask);
        };
        if protocol_version != PROTOCOL_VERSION {
            return Err(SubmitValidationError::UnsupportedProtocolVersion(
                protocol_version,
            ));
        }
        validate_uuid("requestId", &request_id)?;
        let submit = Self::try_from_payload(payload)?;
        Ok((request_id, submit))
    }

    pub(crate) fn try_from_payload(
        payload: SubmitTaskPayload,
    ) -> Result<Self, SubmitValidationError> {
        validate_uuid("clientRequestId", &payload.client_request_id)?;
        validate_command(&payload)?;
        let budget =
            ResourceBudget::try_from_protocol(payload.limits.clone(), payload.output.clone())?;
        Ok(Self { payload, budget })
    }

    pub(crate) fn payload(&self) -> &SubmitTaskPayload {
        &self.payload
    }

    pub(crate) fn budget(&self) -> &ResourceBudget {
        &self.budget
    }
}

fn validate_uuid(field: &'static str, value: &str) -> Result<(), SubmitValidationError> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if valid {
        Ok(())
    } else {
        Err(SubmitValidationError::InvalidUuid { field })
    }
}

fn validate_command(payload: &SubmitTaskPayload) -> Result<(), SubmitValidationError> {
    let command = &payload.command;
    if !command.program.starts_with('/') {
        return Err(SubmitValidationError::ProgramNotAbsolute);
    }
    if !command.working_directory.starts_with('/') {
        return Err(SubmitValidationError::WorkingDirectoryNotAbsolute);
    }
    reject_nul("command.program", &command.program)?;
    reject_nul("command.workingDirectory", &command.working_directory)?;
    for argument in &command.args {
        reject_nul("command.args", argument)?;
    }
    for (key, value) in &command.environment {
        if key.is_empty() || key.contains('=') {
            return Err(SubmitValidationError::InvalidEnvironmentKey);
        }
        reject_nul("command.environment key", key)?;
        reject_nul("command.environment value", value)?;
    }
    Ok(())
}

fn reject_nul(field: &'static str, value: &str) -> Result<(), SubmitValidationError> {
    if value.as_bytes().contains(&0) {
        Err(SubmitValidationError::NulByte { field })
    } else {
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum SubmitValidationError {
    #[error("submitTask 요청이 아닙니다")]
    NotSubmitTask,
    #[error("지원하지 않는 protocolVersion입니다: {0}")]
    UnsupportedProtocolVersion(u32),
    #[error("{field} 값은 UUID여야 합니다")]
    InvalidUuid { field: &'static str },
    #[error("command.program은 절대 경로여야 합니다")]
    ProgramNotAbsolute,
    #[error("command.workingDirectory는 절대 경로여야 합니다")]
    WorkingDirectoryNotAbsolute,
    #[error("환경 변수 이름은 비어 있거나 '=' 문자를 포함할 수 없습니다")]
    InvalidEnvironmentKey,
    #[error("{field} 값에 NUL 문자가 있습니다")]
    NulByte { field: &'static str },
    #[error(transparent)]
    ResourceBudget(#[from] ResourceBudgetError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    #[cfg(target_os = "linux")]
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::{Barrier, Notify};
    use tokio::time::{Duration as TokioDuration, timeout};

    #[cfg(target_os = "linux")]
    use crate::preflight::{CapabilityProbe, SystemProbe};
    use crate::protocol::{
        CommandSpec, CpuMax, OutputLimits, ProcessResult, ResourceLimits, TaskOutput, TaskTiming,
        TaskUsage, TerminationReason,
    };

    use super::*;

    const REQUEST_ID: &str = "11111111-1111-1111-1111-111111111111";
    const CLIENT_REQUEST_ID: &str = "22222222-2222-2222-2222-222222222222";
    const TASK_ID: &str = "33333333-3333-3333-3333-333333333333";
    const OTHER_TASK_ID: &str = "44444444-4444-4444-4444-444444444444";
    const NONZERO_CLIENT_REQUEST_ID: &str = "55555555-5555-5555-5555-555555555555";
    const NONZERO_TASK_ID: &str = "66666666-6666-6666-6666-666666666666";
    const TIMEOUT_CLIENT_REQUEST_ID: &str = "77777777-7777-7777-7777-777777777777";
    const TIMEOUT_TASK_ID: &str = "88888888-8888-8888-8888-888888888888";
    const EXEC_FAILURE_CLIENT_REQUEST_ID: &str = "99999999-9999-9999-9999-999999999999";
    const EXEC_FAILURE_TASK_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const CAPACITY_CLIENT_REQUEST_ID: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    const CAPACITY_TASK_ID: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";

    fn payload() -> SubmitTaskPayload {
        SubmitTaskPayload {
            client_request_id: CLIENT_REQUEST_ID.to_owned(),
            command: CommandSpec {
                program: "/usr/bin/true".to_owned(),
                args: vec!["argument".to_owned()],
                working_directory: "/tmp".to_owned(),
                environment: BTreeMap::from([("LANG".to_owned(), "C.UTF-8".to_owned())]),
            },
            limits: ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 1,
                    period_micros: 1,
                },
                memory_max_bytes: 1,
                pids_max: 1,
                wall_time_limit_ms: 1,
            },
            output: OutputLimits {
                stdout_tail_max_bytes: 1,
                stderr_tail_max_bytes: 1,
            },
        }
    }

    fn payload_for(client_request_id: &str) -> SubmitTaskPayload {
        let mut actual = payload();
        actual.client_request_id = client_request_id.to_owned();
        actual
    }

    fn request(protocol_version: u32, request_id: &str, payload: SubmitTaskPayload) -> Request {
        Request::SubmitTask {
            protocol_version,
            request_id: request_id.to_owned(),
            payload,
        }
    }

    fn metadata(task_id: &str) -> SubmitMetadata {
        SubmitMetadata {
            task_id: task_id.to_owned(),
            submitted_at: "2026-07-24T10:00:00.000Z".to_owned(),
            started_at: "2026-07-24T10:00:00.010Z".to_owned(),
            started_monotonic: Instant::now(),
            cleanup_timeout: Duration::from_secs(5),
        }
    }

    fn task_capacity(maximum: u32) -> Arc<TaskCapacity> {
        Arc::new(TaskCapacity::new(
            TaskCapacitySettings::new(maximum).unwrap(),
        ))
    }

    fn reusable_failure(failure: SubmitFailure) -> ExecutionFailure {
        ExecutionFailure::new(failure, true)
    }

    fn retained_failure(failure: SubmitFailure) -> ExecutionFailure {
        ExecutionFailure::new(failure, false)
    }

    fn running(task_id: &str) -> TaskPayload {
        TaskPayload::Running {
            task_id: task_id.to_owned(),
            submitted_at: "2026-07-24T10:00:00.000Z".to_owned(),
            started_at: "2026-07-24T10:00:00.010Z".to_owned(),
        }
    }

    fn finished(task_id: &str) -> TaskPayload {
        TaskPayload::Finished {
            task_id: task_id.to_owned(),
            termination_reason: TerminationReason::Exited,
            process: ProcessResult {
                exit_code: Some(0),
                signal: None,
            },
            timing: TaskTiming {
                submitted_at: "2026-07-24T10:00:00.000Z".to_owned(),
                started_at: "2026-07-24T10:00:00.010Z".to_owned(),
                finished_at: "2026-07-24T10:00:01.000Z".to_owned(),
                wall_time_ms: 990,
            },
            usage: TaskUsage {
                cpu_time_micros: 1,
                memory_peak_bytes: 1,
            },
            output: TaskOutput {
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
        }
    }

    fn cancelled(task_id: &str) -> TaskPayload {
        let mut payload = finished(task_id);
        let TaskPayload::Finished {
            termination_reason, ..
        } = &mut payload
        else {
            unreachable!()
        };
        *termination_reason = TerminationReason::Cancelled;
        payload
    }

    #[cfg(target_os = "linux")]
    fn linux_payload(
        client_request_id: &str,
        program: &str,
        args: &[&str],
        wall_time_limit_ms: u64,
    ) -> SubmitTaskPayload {
        let mut actual = payload();
        actual.client_request_id = client_request_id.to_owned();
        actual.command.program = program.to_owned();
        actual.command.args = args.iter().map(|value| (*value).to_owned()).collect();
        actual.command.working_directory = "/".to_owned();
        actual.command.environment.clear();
        actual.limits.cpu_max.quota_micros = 50_000;
        actual.limits.cpu_max.period_micros = 100_000;
        actual.limits.memory_max_bytes = 64 * 1024 * 1024;
        actual.limits.pids_max = 8;
        actual.limits.wall_time_limit_ms = wall_time_limit_ms;
        actual.output.stdout_tail_max_bytes = 1_024;
        actual.output.stderr_tail_max_bytes = 1_024;
        actual
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_finished(coordinator: &SubmitCoordinator, task_id: &str) -> TaskPayload {
        timeout(TokioDuration::from_secs(5), async {
            loop {
                let snapshot = coordinator.snapshot(task_id).unwrap();
                if let Some(finished @ TaskPayload::Finished { .. }) = snapshot {
                    return finished;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("실제 Runner 정리 뒤 FINISHED가 저장돼야 합니다")
    }

    #[cfg(target_os = "linux")]
    fn assert_finished_reason(payload: TaskPayload, expected: TerminationReason) -> ProcessResult {
        match payload {
            TaskPayload::Finished {
                termination_reason,
                process,
                ..
            } => {
                assert_eq!(termination_reason, expected);
                process
            }
            TaskPayload::Running { .. } => panic!("FINISHED 결과가 필요합니다"),
        }
    }

    #[test]
    fn fixture_is_validated_without_changing_the_typed_payload() {
        let fixture = include_str!("../../protocol-fixtures/v1/submit-task-valid.json");
        let request: Request = serde_json::from_str(fixture).unwrap();
        let expected = match &request {
            Request::SubmitTask { payload, .. } => payload.clone(),
            _ => unreachable!(),
        };

        let (_, validated) = ValidatedSubmit::try_from_request(request).unwrap();

        assert_eq!(validated.payload(), &expected);
    }

    #[test]
    fn protocol_version_is_rejected_before_a_submit_can_be_reserved() {
        assert_eq!(
            ValidatedSubmit::try_from_request(request(2, REQUEST_ID, payload())).unwrap_err(),
            SubmitValidationError::UnsupportedProtocolVersion(2)
        );
    }

    #[test]
    fn validates_identifiers_paths_environment_and_nul_bytes() {
        let mut cases = Vec::new();

        let mut invalid_client = payload();
        invalid_client.client_request_id = "not-a-uuid".to_owned();
        cases.push((
            invalid_client,
            SubmitValidationError::InvalidUuid {
                field: "clientRequestId",
            },
        ));

        let mut relative_program = payload();
        relative_program.command.program = "usr/bin/true".to_owned();
        cases.push((relative_program, SubmitValidationError::ProgramNotAbsolute));

        let mut relative_directory = payload();
        relative_directory.command.working_directory = "tmp".to_owned();
        cases.push((
            relative_directory,
            SubmitValidationError::WorkingDirectoryNotAbsolute,
        ));

        let mut invalid_environment = payload();
        invalid_environment
            .command
            .environment
            .insert("BAD=KEY".to_owned(), "value".to_owned());
        cases.push((
            invalid_environment,
            SubmitValidationError::InvalidEnvironmentKey,
        ));

        let mut nul_argument = payload();
        nul_argument.command.args = vec!["bad\0argument".to_owned()];
        cases.push((
            nul_argument,
            SubmitValidationError::NulByte {
                field: "command.args",
            },
        ));

        for (payload, expected) in cases {
            assert_eq!(
                ValidatedSubmit::try_from_payload(payload).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn invalid_resource_budget_is_rejected_before_reservation() {
        let mut invalid = payload();
        invalid.limits.memory_max_bytes = 0;

        assert!(matches!(
            ValidatedSubmit::try_from_payload(invalid),
            Err(SubmitValidationError::ResourceBudget(
                ResourceBudgetError::Zero {
                    field: "limits.memoryMaxBytes"
                }
            ))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_identical_requests_use_the_coordinator_and_start_once() {
        const CALLS: usize = 12;
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let start = Arc::new(Barrier::new(CALLS));
        let release = Arc::new(Notify::new());
        let executor_starts = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for index in 0..CALLS {
            let registry = registry.clone();
            let capacity = Arc::clone(&capacity);
            let start = Arc::clone(&start);
            let release = Arc::clone(&release);
            let executor_starts = Arc::clone(&executor_starts);
            handles.push(tokio::spawn(async move {
                start.wait().await;
                coordinate_submit(
                    registry,
                    capacity,
                    request(PROTOCOL_VERSION, REQUEST_ID, payload()),
                    metadata(&format!("33333333-3333-3333-3333-{index:012}")),
                    move |config, running_sender, _cancellation| async move {
                        executor_starts.fetch_add(1, Ordering::SeqCst);
                        running_sender.send(running(&config.task_id)).unwrap();
                        release.notified().await;
                        Ok(ExecutionCompletion::Test(finished(&config.task_id)))
                    },
                )
                .await
            }));
        }

        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(handle.await.unwrap().unwrap());
        }
        assert_eq!(executor_starts.load(Ordering::SeqCst), 1);
        assert!(outcomes.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(matches!(
            outcomes[0].observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));

        let task_id = outcomes[0].task_id.clone();
        release.notify_waiters();
        let stored = timeout(TokioDuration::from_secs(2), async {
            loop {
                let snapshot = registry.snapshot(&task_id).unwrap();
                if matches!(snapshot, Some(TaskPayload::Finished { .. })) {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("coordinator가 FINISHED를 저장해야 합니다");
        assert_eq!(stored, Some(finished(&task_id)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn conflicting_coordinator_call_has_zero_executor_side_effects() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let start = Arc::new(Barrier::new(2));
        let counters = Arc::new([
            [
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ],
            [
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ],
        ]);
        let mut handles = Vec::new();

        for index in 0..2 {
            let registry = registry.clone();
            let capacity = Arc::clone(&capacity);
            let start = Arc::clone(&start);
            let counters = Arc::clone(&counters);
            handles.push(tokio::spawn(async move {
                let mut candidate = payload();
                if index == 1 {
                    candidate.command.args.push("conflict".to_owned());
                }
                start.wait().await;
                let result = coordinate_submit(
                    registry,
                    capacity,
                    request(PROTOCOL_VERSION, REQUEST_ID, candidate),
                    metadata(&format!("44444444-4444-4444-4444-{index:012}")),
                    {
                        let counters = Arc::clone(&counters);
                        move |config, running_sender, _cancellation| async move {
                            for counter in &counters[index] {
                                counter.fetch_add(1, Ordering::SeqCst);
                            }
                            running_sender.send(running(&config.task_id)).unwrap();
                            Ok(ExecutionCompletion::Test(finished(&config.task_id)))
                        }
                    },
                )
                .await;
                (index, result)
            }));
        }

        let mut conflict_index = None;
        for handle in handles {
            let (index, result) = handle.await.unwrap();
            match result {
                Ok(outcome) => assert!(matches!(
                    outcome.observation,
                    SubmitObservation::Task(TaskPayload::Running { .. })
                )),
                Err(SubmitError::Registry(RegistryError::IdempotencyConflict(_))) => {
                    conflict_index = Some(index);
                }
                Err(error) => panic!("예상하지 못한 submit 오류: {error}"),
            }
        }

        let conflict_index = conflict_index.expect("서로 다른 payload 하나는 충돌해야 합니다");
        for counter in &counters[conflict_index] {
            assert_eq!(counter.load(Ordering::SeqCst), 0);
        }
        for side_effect in 0..3 {
            assert_eq!(
                counters
                    .iter()
                    .map(|counter| counter[side_effect].load(Ordering::SeqCst))
                    .sum::<usize>(),
                1
            );
        }
    }

    #[tokio::test]
    async fn failure_before_running_rolls_back_and_allows_a_new_owner() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let executor_starts = Arc::new(AtomicUsize::new(0));
        let first_starts = Arc::clone(&executor_starts);
        let expected = SubmitFailure::new(ErrorCode::InternalError, "runner start failed");
        let first = coordinate_submit(
            registry.clone(),
            Arc::clone(&capacity),
            request(PROTOCOL_VERSION, REQUEST_ID, payload()),
            metadata(TASK_ID),
            {
                let expected = expected.clone();
                move |_config, _running_sender, _cancellation| async move {
                    first_starts.fetch_add(1, Ordering::SeqCst);
                    Err::<ExecutionCompletion, _>(reusable_failure(expected))
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(first.observation, SubmitObservation::Failed(expected));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );

        let mut changed = payload();
        changed.command.args.push("retry".to_owned());
        let retry_starts = Arc::clone(&executor_starts);
        let retry = coordinate_submit(
            registry.clone(),
            capacity,
            request(PROTOCOL_VERSION, REQUEST_ID, changed),
            metadata(OTHER_TASK_ID),
            move |config, running_sender, _cancellation| async move {
                retry_starts.fetch_add(1, Ordering::SeqCst);
                running_sender.send(running(&config.task_id)).unwrap();
                Ok(ExecutionCompletion::Test(finished(&config.task_id)))
            },
        )
        .await
        .unwrap();

        assert_eq!(retry.task_id, OTHER_TASK_ID);
        assert!(matches!(
            retry.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));
        assert_eq!(executor_starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn capacity_rejects_n_plus_one_without_side_effects_and_reuses_finished_slots() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(2);
        let release = Arc::new(Notify::new());
        let side_effects = Arc::new([
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        ]);

        for (client_request_id, task_id) in [
            (CLIENT_REQUEST_ID, TASK_ID),
            (NONZERO_CLIENT_REQUEST_ID, NONZERO_TASK_ID),
        ] {
            let release = Arc::clone(&release);
            let side_effects = Arc::clone(&side_effects);
            let outcome = coordinate_submit(
                registry.clone(),
                Arc::clone(&capacity),
                request(PROTOCOL_VERSION, REQUEST_ID, payload_for(client_request_id)),
                metadata(task_id),
                move |config, running_sender, _cancellation| async move {
                    for counter in side_effects.iter() {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    running_sender.send(running(&config.task_id)).unwrap();
                    release.notified().await;
                    Ok(ExecutionCompletion::Test(finished(&config.task_id)))
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                outcome.observation,
                SubmitObservation::Task(TaskPayload::Running { .. })
            ));
        }

        let rejected_side_effects = Arc::clone(&side_effects);
        let rejected = timeout(
            TokioDuration::from_millis(100),
            coordinate_submit(
                registry.clone(),
                Arc::clone(&capacity),
                request(
                    PROTOCOL_VERSION,
                    REQUEST_ID,
                    payload_for(TIMEOUT_CLIENT_REQUEST_ID),
                ),
                metadata(TIMEOUT_TASK_ID),
                move |_config, _running_sender, _cancellation| async move {
                    for counter in rejected_side_effects.iter() {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(ExecutionCompletion::Test(finished(TIMEOUT_TASK_ID)))
                },
            ),
        )
        .await
        .expect("capacity 거절은 대기하지 않아야 합니다")
        .unwrap();

        assert_eq!(
            rejected.observation,
            SubmitObservation::Failed(SubmitFailure::new(
                ErrorCode::CapacityExhausted,
                CAPACITY_EXHAUSTED_MESSAGE,
            ))
        );
        assert_eq!(registry.snapshot(TIMEOUT_TASK_ID), Ok(None));
        assert_eq!(
            registry.snapshot_by_client_request_id(TIMEOUT_CLIENT_REQUEST_ID),
            Ok(None)
        );
        for counter in side_effects.iter() {
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        }

        release.notify_waiters();
        timeout(TokioDuration::from_secs(2), async {
            loop {
                let first_finished = matches!(
                    registry.snapshot(TASK_ID).unwrap(),
                    Some(TaskPayload::Finished { .. })
                );
                let second_finished = matches!(
                    registry.snapshot(NONZERO_TASK_ID).unwrap(),
                    Some(TaskPayload::Finished { .. })
                );
                if first_finished && second_finished {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("정리된 FINISHED 저장 뒤 슬롯이 반환돼야 합니다");

        let retried_side_effects = Arc::clone(&side_effects);
        let retried = coordinate_submit(
            registry.clone(),
            capacity,
            request(
                PROTOCOL_VERSION,
                REQUEST_ID,
                payload_for(TIMEOUT_CLIENT_REQUEST_ID),
            ),
            metadata(TIMEOUT_TASK_ID),
            move |config, running_sender, _cancellation| async move {
                for counter in retried_side_effects.iter() {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                running_sender.send(running(&config.task_id)).unwrap();
                Ok(ExecutionCompletion::Test(finished(&config.task_id)))
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            retried.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));
        for counter in side_effects.iter() {
            assert_eq!(counter.load(Ordering::SeqCst), 3);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancellation_keeps_capacity_until_cleanup_and_finished_storage() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let cleanup_started = Arc::new(Notify::new());
        let cleanup_release = Arc::new(Notify::new());
        let cancel_actions = Arc::new(AtomicUsize::new(0));

        let first = coordinate_submit(
            registry.clone(),
            Arc::clone(&capacity),
            request(PROTOCOL_VERSION, REQUEST_ID, payload()),
            metadata(TASK_ID),
            {
                let cleanup_started = Arc::clone(&cleanup_started);
                let cleanup_release = Arc::clone(&cleanup_release);
                let cancel_actions = Arc::clone(&cancel_actions);
                move |config, running_sender, cancellation| async move {
                    running_sender.send(running(&config.task_id)).unwrap();
                    cancellation.cancelled().await;
                    cancel_actions.fetch_add(1, Ordering::SeqCst);
                    cleanup_started.notify_one();
                    cleanup_release.notified().await;
                    Ok(ExecutionCompletion::Test(cancelled(&config.task_id)))
                }
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            first.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));

        let first_cancel = registry.request_cancel(TASK_ID).unwrap();
        let second_cancel = registry.request_cancel(TASK_ID).unwrap();
        timeout(TokioDuration::from_secs(1), cleanup_started.notified())
            .await
            .expect("cancel trigger 뒤 내부 cleanup이 시작돼야 합니다");
        assert_eq!(cancel_actions.load(Ordering::SeqCst), 1);

        let mut first_cancel_wait = Box::pin(first_cancel.wait());
        assert!(
            timeout(TokioDuration::from_millis(10), &mut first_cancel_wait)
                .await
                .is_err()
        );
        assert!(matches!(
            registry.snapshot(TASK_ID).unwrap(),
            Some(TaskPayload::Running { .. })
        ));

        let rejected_starts = Arc::new(AtomicUsize::new(0));
        let rejected_counter = Arc::clone(&rejected_starts);
        let rejected = coordinate_submit(
            registry.clone(),
            Arc::clone(&capacity),
            request(
                PROTOCOL_VERSION,
                REQUEST_ID,
                payload_for(NONZERO_CLIENT_REQUEST_ID),
            ),
            metadata(NONZERO_TASK_ID),
            move |_config, _running_sender, _cancellation| async move {
                rejected_counter.fetch_add(1, Ordering::SeqCst);
                Ok(ExecutionCompletion::Test(finished(NONZERO_TASK_ID)))
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            rejected.observation,
            SubmitObservation::Failed(SubmitFailure {
                code: ErrorCode::CapacityExhausted,
                ..
            })
        ));
        assert_eq!(rejected_starts.load(Ordering::SeqCst), 0);

        cleanup_release.notify_one();
        let expected = cancelled(TASK_ID);
        assert_eq!(first_cancel_wait.await, expected);
        assert_eq!(second_cancel.wait().await, expected);
        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected)));

        let reused_starts = Arc::new(AtomicUsize::new(0));
        let reused_counter = Arc::clone(&reused_starts);
        let reused = coordinate_submit(
            registry,
            capacity,
            request(
                PROTOCOL_VERSION,
                REQUEST_ID,
                payload_for(NONZERO_CLIENT_REQUEST_ID),
            ),
            metadata(NONZERO_TASK_ID),
            move |config, running_sender, _cancellation| async move {
                reused_counter.fetch_add(1, Ordering::SeqCst);
                running_sender.send(running(&config.task_id)).unwrap();
                Ok(ExecutionCompletion::Test(finished(&config.task_id)))
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            reused.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));
        assert_eq!(reused_starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cleanup_uncertainty_after_running_retains_the_slot() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let fail_now = Arc::new(Notify::new());
        let executor_starts = Arc::new(AtomicUsize::new(0));
        let first_starts = Arc::clone(&executor_starts);
        let first_fail = Arc::clone(&fail_now);

        let first = coordinate_submit(
            registry.clone(),
            Arc::clone(&capacity),
            request(PROTOCOL_VERSION, REQUEST_ID, payload()),
            metadata(TASK_ID),
            move |config, running_sender, _cancellation| async move {
                first_starts.fetch_add(1, Ordering::SeqCst);
                running_sender.send(running(&config.task_id)).unwrap();
                first_fail.notified().await;
                Err(retained_failure(SubmitFailure::new(
                    ErrorCode::InternalError,
                    "cleanup uncertain",
                )))
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            first.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));

        fail_now.notify_one();
        timeout(TokioDuration::from_secs(1), async {
            while capacity.retained_for_fail_stop() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("정리 불확실 슬롯이 fail-stop용으로 보존돼야 합니다");

        let second_starts = Arc::clone(&executor_starts);
        let second = coordinate_submit(
            registry.clone(),
            capacity,
            request(
                PROTOCOL_VERSION,
                REQUEST_ID,
                payload_for(NONZERO_CLIENT_REQUEST_ID),
            ),
            metadata(NONZERO_TASK_ID),
            move |_config, _running_sender, _cancellation| async move {
                second_starts.fetch_add(1, Ordering::SeqCst);
                Ok(ExecutionCompletion::Test(finished(NONZERO_TASK_ID)))
            },
        )
        .await
        .unwrap();

        assert_eq!(
            second.observation,
            SubmitObservation::Failed(SubmitFailure::new(
                ErrorCode::CapacityExhausted,
                CAPACITY_EXHAUSTED_MESSAGE,
            ))
        );
        assert_eq!(executor_starts.load(Ordering::SeqCst), 1);
        assert!(matches!(
            registry.snapshot(TASK_ID).unwrap(),
            Some(TaskPayload::Running { .. })
        ));
        assert_eq!(registry.snapshot(NONZERO_TASK_ID), Ok(None));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_submit_coordinator_runs_once_and_finishes_after_cleanup() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_SUBMIT_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let coordinator =
            SubmitCoordinator::initialize(environment, TaskCapacitySettings::new(1).unwrap())
                .unwrap();
        let actual_payload = linux_payload(CLIENT_REQUEST_ID, "/bin/sleep", &["2"], 5_000);

        let started = Instant::now();
        let first = coordinator
            .submit(
                request(PROTOCOL_VERSION, REQUEST_ID, actual_payload.clone()),
                metadata(TASK_ID),
                move || {
                    (
                        "2026-07-24T10:00:01.000Z".to_owned(),
                        started + Duration::from_secs(1),
                    )
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            first.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));

        let capacity_rejected = coordinator
            .submit(
                request(
                    PROTOCOL_VERSION,
                    REQUEST_ID,
                    linux_payload(CAPACITY_CLIENT_REQUEST_ID, "/bin/true", &[], 5_000),
                ),
                metadata(CAPACITY_TASK_ID),
                move || ("2026-07-24T10:00:01.500Z".to_owned(), Instant::now()),
            )
            .await
            .unwrap();
        assert_eq!(
            capacity_rejected.observation,
            SubmitObservation::Failed(SubmitFailure::new(
                ErrorCode::CapacityExhausted,
                CAPACITY_EXHAUSTED_MESSAGE,
            ))
        );
        assert_eq!(coordinator.snapshot(CAPACITY_TASK_ID), Ok(None));
        assert_eq!(
            coordinator.snapshot_by_client_request_id(CAPACITY_CLIENT_REQUEST_ID),
            Ok(None)
        );

        let second = coordinator
            .submit(
                request(PROTOCOL_VERSION, REQUEST_ID, actual_payload),
                metadata(OTHER_TASK_ID),
                move || ("2026-07-24T10:00:02.000Z".to_owned(), Instant::now()),
            )
            .await
            .unwrap();
        assert_eq!(second.task_id, first.task_id);
        assert_eq!(coordinator.snapshot(OTHER_TASK_ID), Ok(None));

        let finished = wait_for_finished(&coordinator, TASK_ID).await;
        let first_process = assert_finished_reason(finished.clone(), TerminationReason::Exited);
        assert_eq!(first_process.exit_code, Some(0));
        assert_eq!(
            coordinator.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(Some(finished))
        );

        let nonzero = coordinator
            .submit(
                request(
                    PROTOCOL_VERSION,
                    REQUEST_ID,
                    linux_payload(NONZERO_CLIENT_REQUEST_ID, "/bin/false", &[], 5_000),
                ),
                metadata(NONZERO_TASK_ID),
                move || ("2026-07-24T10:00:03.000Z".to_owned(), Instant::now()),
            )
            .await
            .unwrap();
        assert!(matches!(
            nonzero.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));
        let nonzero_process = assert_finished_reason(
            wait_for_finished(&coordinator, NONZERO_TASK_ID).await,
            TerminationReason::Exited,
        );
        assert_eq!(nonzero_process.exit_code, Some(1));

        let timed_out = coordinator
            .submit(
                request(
                    PROTOCOL_VERSION,
                    REQUEST_ID,
                    linux_payload(TIMEOUT_CLIENT_REQUEST_ID, "/bin/sleep", &["30"], 100),
                ),
                metadata(TIMEOUT_TASK_ID),
                move || ("2026-07-24T10:00:04.000Z".to_owned(), Instant::now()),
            )
            .await
            .unwrap();
        assert!(matches!(
            timed_out.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));
        let timeout_process = assert_finished_reason(
            wait_for_finished(&coordinator, TIMEOUT_TASK_ID).await,
            TerminationReason::TimedOut,
        );
        assert_eq!(timeout_process.exit_code, None);
        assert_eq!(timeout_process.signal.as_deref(), Some("SIGKILL"));

        let exec_failed = coordinator
            .submit(
                request(
                    PROTOCOL_VERSION,
                    REQUEST_ID,
                    linux_payload(
                        EXEC_FAILURE_CLIENT_REQUEST_ID,
                        "/definitely/missing/taskcage-target",
                        &[],
                        5_000,
                    ),
                ),
                metadata(EXEC_FAILURE_TASK_ID),
                move || ("2026-07-24T10:00:05.000Z".to_owned(), Instant::now()),
            )
            .await
            .unwrap();
        let exec_process = match exec_failed.observation {
            SubmitObservation::Task(finished) => {
                assert_finished_reason(finished, TerminationReason::ExecutionFailed)
            }
            SubmitObservation::Failed(failure) => {
                panic!("exec 시작 실패는 정리된 FINISHED여야 합니다: {failure:?}")
            }
        };
        assert_eq!(exec_process.exit_code, None);
        assert_eq!(exec_process.signal, None);
        assert!(!coordinator.runner.cleanup_is_uncertain());

        let remaining_jobs = fs::read_dir(jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(remaining_jobs, 0, "작업 cgroup이 남아 있습니다");
    }
}
