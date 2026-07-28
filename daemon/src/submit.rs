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
#[cfg(any(target_os = "linux", test))]
use crate::fail_stop::{
    ActiveExecution, CleanupFailureReport, FailStopCoordinator, FailStopSettings,
};
#[cfg(target_os = "linux")]
use crate::preflight::VerifiedEnvironment;
#[cfg(any(target_os = "linux", test))]
use crate::protocol::{ErrorCode, TaskPayload};
use crate::protocol::{PROTOCOL_VERSION, Request, SubmitTaskPayload};
#[cfg(any(target_os = "linux", test))]
use crate::resource_budget::VerifiedEffectiveLimits;
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};
#[cfg(target_os = "linux")]
use crate::runner::{CompletedTask, TaskRunConfig, TaskRunFailureKind, TaskRunner};

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
pub(crate) enum TaskIdSource {
    #[cfg(test)]
    Fixed(String),
    Lazy(Box<dyn FnOnce() -> String + Send + 'static>),
}

#[cfg(any(target_os = "linux", test))]
impl std::fmt::Debug for TaskIdSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(test)]
            Self::Fixed(task_id) => formatter.debug_tuple("Fixed").field(task_id).finish(),
            Self::Lazy(_) => formatter.write_str("Lazy(<task id factory>)"),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
pub(crate) struct SubmitMetadata {
    task_id: Option<TaskIdSource>,
    pub(crate) submitted_at: String,
    start_time: TaskStartTimeSource,
    pub(crate) cleanup_timeout: Duration,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone)]
pub(crate) struct TaskStartTime {
    wall_clock: String,
    monotonic: Instant,
}

#[cfg(any(target_os = "linux", test))]
impl TaskStartTime {
    pub(crate) fn new(wall_clock: String, monotonic: Instant) -> Self {
        Self {
            wall_clock,
            monotonic,
        }
    }

    pub(crate) fn wall_clock(&self) -> &str {
        &self.wall_clock
    }

    pub(crate) fn monotonic(&self) -> Instant {
        self.monotonic
    }
}

#[cfg(any(target_os = "linux", test))]
pub(crate) struct TaskStartTimeSource {
    capture: Option<Box<dyn FnOnce() -> TaskStartTime + Send + 'static>>,
}

#[cfg(any(target_os = "linux", test))]
impl std::fmt::Debug for TaskStartTimeSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TaskStartTimeSource(<lazy clock>)")
    }
}

#[cfg(any(target_os = "linux", test))]
impl TaskStartTimeSource {
    pub(crate) fn new(capture: impl FnOnce() -> TaskStartTime + Send + 'static) -> Self {
        Self {
            capture: Some(Box::new(capture)),
        }
    }

    pub(crate) fn capture(mut self) -> TaskStartTime {
        self.capture
            .take()
            .expect("작업 시작 시각 source는 한 번만 호출할 수 있습니다")()
    }
}

#[cfg(any(target_os = "linux", test))]
impl SubmitMetadata {
    #[cfg(test)]
    pub(crate) fn fixed(
        task_id: String,
        submitted_at: String,
        capture_start_time: impl FnOnce() -> TaskStartTime + Send + 'static,
        cleanup_timeout: Duration,
    ) -> Self {
        Self {
            task_id: Some(TaskIdSource::Fixed(task_id)),
            submitted_at,
            start_time: TaskStartTimeSource::new(capture_start_time),
            cleanup_timeout,
        }
    }

    pub(crate) fn lazy(
        make_task_id: impl FnOnce() -> String + Send + 'static,
        submitted_at: String,
        capture_start_time: impl FnOnce() -> TaskStartTime + Send + 'static,
        cleanup_timeout: Duration,
    ) -> Self {
        Self {
            task_id: Some(TaskIdSource::Lazy(Box::new(make_task_id))),
            submitted_at,
            start_time: TaskStartTimeSource::new(capture_start_time),
            cleanup_timeout,
        }
    }

    fn make_task_id(&mut self) -> String {
        match self.task_id.take().expect("task ID source가 있어야 합니다") {
            #[cfg(test)]
            TaskIdSource::Fixed(task_id) => task_id,
            TaskIdSource::Lazy(make_task_id) => make_task_id(),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmitOutcome {
    pub(crate) request_id: String,
    pub(crate) task_id: String,
    pub(crate) effective_limits: Option<VerifiedEffectiveLimits>,
    pub(crate) observation: SubmitObservation,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
pub(crate) struct VerifiedRunningTask {
    snapshot: TaskPayload,
    effective_limits: VerifiedEffectiveLimits,
}

#[cfg(any(target_os = "linux", test))]
impl VerifiedRunningTask {
    pub(crate) fn new(snapshot: TaskPayload, effective_limits: VerifiedEffectiveLimits) -> Self {
        Self {
            snapshot,
            effective_limits,
        }
    }

    fn into_parts(self) -> (TaskPayload, VerifiedEffectiveLimits) {
        (self.snapshot, self.effective_limits)
    }
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
    start_time: TaskStartTimeSource,
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
    cleanup_complete: bool,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
struct OwnerRuntime {
    capacity_permit: TaskCapacityPermit,
    active: ActiveExecution,
    fail_stop: Arc<FailStopCoordinator>,
}

#[cfg(any(target_os = "linux", test))]
impl ExecutionFailure {
    fn new(submit: SubmitFailure, capacity_reusable: bool, cleanup_complete: bool) -> Self {
        Self {
            submit,
            capacity_reusable,
            cleanup_complete,
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
    fail_stop: Arc<FailStopCoordinator>,
}

#[cfg(target_os = "linux")]
impl SubmitCoordinator {
    pub(crate) fn initialize(
        environment: VerifiedEnvironment,
        capacity_settings: TaskCapacitySettings,
        fail_stop: Arc<FailStopCoordinator>,
    ) -> crate::Result<Self> {
        Ok(Self {
            registry: TaskRegistry::new(),
            runner: Arc::new(TaskRunner::initialize(environment, Arc::clone(&fail_stop))?),
            capacity: Arc::new(TaskCapacity::new(capacity_settings)),
            fail_stop,
        })
    }

    #[cfg(test)]
    pub(crate) fn initialize_with_cgroup_create_faults(
        environment: VerifiedEnvironment,
        capacity_settings: TaskCapacitySettings,
        fail_stop: Arc<FailStopCoordinator>,
        faults: Arc<crate::cgroup::CgroupCreateFaults>,
    ) -> crate::Result<Self> {
        Ok(Self {
            registry: TaskRegistry::new(),
            runner: Arc::new(TaskRunner::initialize_with_cgroup_create_faults(
                environment,
                Arc::clone(&fail_stop),
                faults,
            )?),
            capacity: Arc::new(TaskCapacity::new(capacity_settings)),
            fail_stop,
        })
    }

    #[cfg(test)]
    pub(crate) fn initialize_with_cleanup_faults(
        environment: VerifiedEnvironment,
        capacity_settings: TaskCapacitySettings,
        fail_stop: Arc<FailStopCoordinator>,
        faults: Arc<crate::cleanup_fault::CleanupFaults>,
    ) -> crate::Result<Self> {
        Ok(Self {
            registry: TaskRegistry::new(),
            runner: Arc::new(TaskRunner::initialize_with_cleanup_faults(
                environment,
                Arc::clone(&fail_stop),
                faults,
            )?),
            capacity: Arc::new(TaskCapacity::new(capacity_settings)),
            fail_stop,
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
            Arc::clone(&self.fail_stop),
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
                            start_time: config.start_time,
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
                        let cleanup_complete = error.cleanup_complete();
                        let failure = match error.kind() {
                            TaskRunFailureKind::CgroupReadBackMismatch => SubmitFailure::new(
                                ErrorCode::InternalError,
                                "cgroup limit read-back verification failed",
                            ),
                            TaskRunFailureKind::CgroupReadBackRollbackUncertain => {
                                SubmitFailure::new(
                                    ErrorCode::EnvironmentUnavailable,
                                    "cgroup v2 execution environment is unavailable",
                                )
                            }
                            TaskRunFailureKind::Other => SubmitFailure::new(
                                ErrorCode::InternalError,
                                error.into_error().to_string(),
                            ),
                        };
                        ExecutionFailure::new(failure, capacity_reusable, cleanup_complete)
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

    #[cfg(test)]
    pub(crate) fn capacity_is_available_for_test(&self) -> bool {
        self.capacity.try_acquire().is_some()
    }

    #[cfg(test)]
    pub(crate) fn retained_capacity_for_test(&self) -> u32 {
        self.capacity.retained_for_fail_stop()
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
    E: FnOnce(
            SubmitExecutionConfig,
            oneshot::Sender<VerifiedRunningTask>,
            CancellationRuntime,
        ) -> Fut
        + Send
        + 'static,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>> + Send + 'static,
{
    // 검증은 Registry 예약과 Runner side effect보다 먼저 끝낸다.
    let (request_id, validated) = ValidatedSubmit::try_from_request(request)?;
    let fail_stop = FailStopCoordinator::new(
        FailStopSettings::new(Duration::from_secs(30))
            .expect("시험용 fail-stop timeout은 유효해야 합니다"),
    );
    coordinate_validated_submit(
        registry, capacity, fail_stop, request_id, validated, metadata, executor,
    )
    .await
}

#[cfg(any(target_os = "linux", test))]
async fn coordinate_validated_submit<C, E, Fut>(
    registry: TaskRegistry<C>,
    capacity: Arc<TaskCapacity>,
    fail_stop: Arc<FailStopCoordinator>,
    request_id: String,
    validated: ValidatedSubmit,
    metadata: SubmitMetadata,
    executor: E,
) -> Result<SubmitOutcome, SubmitError>
where
    C: RegistryClock + Send + 'static,
    E: FnOnce(
            SubmitExecutionConfig,
            oneshot::Sender<VerifiedRunningTask>,
            CancellationRuntime,
        ) -> Fut
        + Send
        + 'static,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>> + Send + 'static,
{
    if let Some(waiter) = registry.existing_submit(&validated)? {
        let task_id = waiter.task_id().to_owned();
        let observation = waiter.wait().await;
        return Ok(SubmitOutcome {
            request_id,
            effective_limits: registry.effective_limits_for(&task_id, &observation)?,
            task_id,
            observation,
        });
    }
    let mut metadata = metadata;
    let (reservation, active) = {
        let Some(admission) = fail_stop.try_admit() else {
            return Ok(SubmitOutcome {
                request_id,
                task_id: String::new(),
                effective_limits: None,
                observation: SubmitObservation::Failed(SubmitFailure::new(
                    ErrorCode::EnvironmentUnavailable,
                    "cgroup v2 execution environment is unavailable",
                )),
            });
        };
        let reservation = registry.reserve_submit_with(validated, || metadata.make_task_id())?;
        match &reservation {
            SubmitReservation::Existing(_) => (reservation, None),
            SubmitReservation::Owner(owner) => {
                let active = admission
                    .register(owner.task_id().to_owned())
                    .map_err(|error| {
                        SubmitError::Registry(RegistryError::TaskAlreadyExists(error.to_string()))
                    })?;
                (reservation, Some(active))
            }
        }
    };

    let (task_id, observation) = match reservation {
        SubmitReservation::Existing(waiter) => {
            let task_id = waiter.task_id().to_owned();
            (task_id, waiter.wait().await)
        }
        SubmitReservation::Owner(owner) => {
            let task_id = owner.task_id().to_owned();
            let active = active.expect("새 실행 owner에는 활성 실행 소유권이 있어야 합니다");
            let Some(capacity_permit) = capacity.try_acquire() else {
                let observation = owner.rollback_before_running(SubmitFailure::new(
                    ErrorCode::CapacityExhausted,
                    CAPACITY_EXHAUSTED_MESSAGE,
                ))?;
                active.complete();
                return Ok(SubmitOutcome {
                    request_id,
                    task_id,
                    effective_limits: None,
                    observation,
                });
            };
            let config = SubmitExecutionConfig {
                task_id: task_id.clone(),
                submitted_at: metadata.submitted_at,
                start_time: metadata.start_time,
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
                active,
                Arc::clone(&fail_stop),
            ));
            let observation = initial_receiver
                .await
                .map_err(|_| SubmitError::CoordinatorStopped)?;
            (task_id, observation)
        }
    };

    Ok(SubmitOutcome {
        request_id,
        effective_limits: registry.effective_limits_for(&task_id, &observation)?,
        task_id,
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
    active: ActiveExecution,
    fail_stop: Arc<FailStopCoordinator>,
) where
    C: RegistryClock,
    E: FnOnce(
        SubmitExecutionConfig,
        oneshot::Sender<VerifiedRunningTask>,
        CancellationRuntime,
    ) -> Fut,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>>,
{
    let (running_sender, mut running_receiver) = oneshot::channel();
    let (cancellation_runtime, running_cancellation) = cancellation_channel();
    let execution = executor(config, running_sender, cancellation_runtime);
    let runtime = OwnerRuntime {
        capacity_permit,
        active,
        fail_stop,
    };
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
                    runtime,
                ).await,
                Err(_) => finish_without_running(
                    owner,
                    execution.await,
                    initial_sender,
                    runtime,
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
                    runtime,
                ),
                Err(_) => finish_without_running(
                    owner,
                    completion,
                    initial_sender,
                    runtime,
                ),
            }
        }
    }
}

#[cfg(any(target_os = "linux", test))]
async fn run_after_running<C, Fut>(
    owner: SubmitExecutionOwner<C>,
    running: VerifiedRunningTask,
    execution: Fut,
    cancellation: RunningCancellation,
    initial_sender: oneshot::Sender<SubmitObservation>,
    runtime: OwnerRuntime,
) where
    C: RegistryClock,
    Fut: Future<Output = Result<ExecutionCompletion, ExecutionFailure>>,
{
    let OwnerRuntime {
        capacity_permit,
        active,
        fail_stop,
    } = runtime;
    let (running, effective_limits) = running.into_parts();
    let running =
        match owner.publish_running_with_cancellation(running, effective_limits, cancellation) {
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
            } else {
                active.complete();
                finish_capacity(capacity_permit, &fail_stop);
            }
        }
        Err(failure) => {
            report_running_failure(&fail_stop, owner.task_id());
            owner.fail(failure.submit);
            if failure.cleanup_complete {
                active.complete();
            }
            capacity_permit.retain_for_fail_stop();
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn finish_after_running<C>(
    owner: SubmitExecutionOwner<C>,
    running: VerifiedRunningTask,
    cancellation: RunningCancellation,
    completion: Result<ExecutionCompletion, ExecutionFailure>,
    initial_sender: oneshot::Sender<SubmitObservation>,
    runtime: OwnerRuntime,
) where
    C: RegistryClock,
{
    let OwnerRuntime {
        capacity_permit,
        active,
        fail_stop,
    } = runtime;
    let (running, effective_limits) = running.into_parts();
    let running =
        match owner.publish_running_with_cancellation(running, effective_limits, cancellation) {
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
            } else {
                active.complete();
                finish_capacity(capacity_permit, &fail_stop);
            }
        }
        Err(failure) => {
            report_running_failure(&fail_stop, owner.task_id());
            owner.fail(failure.submit);
            if failure.cleanup_complete {
                active.complete();
            }
            capacity_permit.retain_for_fail_stop();
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn finish_without_running<C>(
    owner: SubmitExecutionOwner<C>,
    completion: Result<ExecutionCompletion, ExecutionFailure>,
    initial_sender: oneshot::Sender<SubmitObservation>,
    runtime: OwnerRuntime,
) where
    C: RegistryClock,
{
    let OwnerRuntime {
        capacity_permit,
        active,
        fail_stop,
    } = runtime;
    let observation = match completion {
        Ok(completed) => match finish_owner(owner, completed) {
            Ok(finished) => {
                active.complete();
                SubmitObservation::Task(finished)
            }
            Err(error) => {
                capacity_permit.retain_for_fail_stop();
                return send_initial_failure(initial_sender, error);
            }
        },
        Err(failure) => {
            if failure.capacity_reusable {
                match owner.rollback_before_running(failure.submit) {
                    Ok(observation) => {
                        active.complete();
                        observation
                    }
                    Err(error) => {
                        capacity_permit.retain_for_fail_stop();
                        return send_initial_failure(initial_sender, error);
                    }
                }
            } else {
                capacity_permit.retain_for_fail_stop();
                let observation = owner.fail(failure.submit);
                if failure.cleanup_complete {
                    active.complete();
                }
                return initial_sender.send(observation).ok().unwrap_or(());
            }
        }
    };
    if fail_stop.is_fail_stopping() {
        capacity_permit.retain_for_fail_stop();
    }
    let _ = initial_sender.send(observation);
}

#[cfg(any(target_os = "linux", test))]
fn finish_capacity(capacity_permit: TaskCapacityPermit, fail_stop: &FailStopCoordinator) {
    if fail_stop.is_fail_stopping() {
        capacity_permit.retain_for_fail_stop();
    }
}

#[cfg(any(target_os = "linux", test))]
fn report_running_failure(fail_stop: &FailStopCoordinator, task_id: &str) {
    fail_stop.activate(CleanupFailureReport::new(
        task_id,
        "RUNNING 작업 완료",
        vec!["검증된 FINISHED 결과"],
        "RUNNING snapshot을 유지하고 daemon 종료를 시작함",
    ));
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
    #[cfg(target_os = "linux")]
    use std::path::Path;
    use std::sync::Arc;
    #[cfg(target_os = "linux")]
    use std::sync::Barrier as ThreadBarrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::{Barrier, Notify};
    use tokio::time::{Duration as TokioDuration, timeout};

    #[cfg(target_os = "linux")]
    use crate::cleanup_fault::{CleanupFaultMode, CleanupFaultPoint, CleanupFaults};
    #[cfg(target_os = "linux")]
    use crate::preflight::{CapabilityProbe, SystemProbe};
    use crate::protocol::{
        CommandSpec, CpuMax, OutputLimits, ProcessResult, ResourceLimits, TaskOutput, TaskTiming,
        TaskUsage, TerminationReason,
    };

    use super::*;

    const REQUEST_ID: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER_REQUEST_ID: &str = "12111111-1111-1111-1111-111111111111";
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
        SubmitMetadata::fixed(
            task_id.to_owned(),
            "2026-07-24T10:00:00.000Z".to_owned(),
            || TaskStartTime::new("2026-07-24T10:00:00.010Z".to_owned(), Instant::now()),
            Duration::from_secs(5),
        )
    }

    #[cfg(target_os = "linux")]
    fn cleanup_fault_metadata(task_id: &str) -> SubmitMetadata {
        SubmitMetadata::fixed(
            task_id.to_owned(),
            "2026-07-24T10:00:00.000Z".to_owned(),
            || TaskStartTime::new("2026-07-24T10:00:00.010Z".to_owned(), Instant::now()),
            Duration::from_millis(100),
        )
    }

    #[cfg(target_os = "linux")]
    async fn teardown_persistent_kill_fault(job_path: &Path, child_pid: u32) {
        if let Err(error) = fs::write(job_path.join("cgroup.kill"), "1\n")
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("시험 teardown cgroup.kill이 실패했습니다: {error}");
        }

        timeout(TokioDuration::from_secs(2), async {
            let mut child_reaped = false;
            loop {
                if !child_reaped {
                    let mut status = 0;
                    // 이 PID는 같은 시험 프로세스가 clone3로 만든 direct child다.
                    let waited = unsafe {
                        libc::waitpid(child_pid as libc::pid_t, &mut status, libc::WNOHANG)
                    };
                    child_reaped = waited == child_pid as libc::pid_t
                        || (waited == -1
                            && std::io::Error::last_os_error().raw_os_error()
                                == Some(libc::ECHILD));
                    assert!(
                        waited >= 0 || child_reaped,
                        "시험 teardown waitpid가 실패했습니다"
                    );
                }

                let populated = match fs::read_to_string(job_path.join("cgroup.events")) {
                    Ok(events) => events.lines().any(|line| line == "populated 1"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => {
                        panic!("시험 teardown cgroup.events 읽기가 실패했습니다: {error}")
                    }
                };
                if child_reaped && !populated {
                    if let Err(error) = fs::remove_dir(job_path)
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        panic!("시험 teardown 작업 cgroup 제거가 실패했습니다: {error}");
                    }
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("시험 teardown에서 target과 작업 cgroup을 회수해야 합니다");
    }

    fn metadata_with_start_clock(
        task_id: &str,
        started_at: &str,
        started_monotonic: Instant,
        calls: Arc<AtomicUsize>,
    ) -> SubmitMetadata {
        let started_at = started_at.to_owned();
        SubmitMetadata::fixed(
            task_id.to_owned(),
            "2026-07-24T10:00:00.000Z".to_owned(),
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                TaskStartTime::new(started_at, started_monotonic)
            },
            Duration::from_secs(5),
        )
    }

    fn task_capacity(maximum: u32) -> Arc<TaskCapacity> {
        Arc::new(TaskCapacity::new(
            TaskCapacitySettings::new(maximum).unwrap(),
        ))
    }

    #[test]
    fn task_start_clock_is_lazy_and_captured_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let expected_monotonic = Instant::now();
        let source = TaskStartTimeSource::new({
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                TaskStartTime::new("started".to_owned(), expected_monotonic)
            }
        });

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let captured = source.capture();
        assert_eq!(captured.wall_clock(), "started");
        assert_eq!(captured.monotonic(), expected_monotonic);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn reusable_failure(failure: SubmitFailure) -> ExecutionFailure {
        ExecutionFailure::new(failure, true, true)
    }

    fn retained_failure(failure: SubmitFailure) -> ExecutionFailure {
        ExecutionFailure::new(failure, false, false)
    }

    fn fail_stop_runtime() -> Arc<FailStopCoordinator> {
        FailStopCoordinator::new(FailStopSettings::new(Duration::from_secs(5)).unwrap())
    }

    fn running(task_id: &str) -> TaskPayload {
        TaskPayload::Running {
            task_id: task_id.to_owned(),
            submitted_at: "2026-07-24T10:00:00.000Z".to_owned(),
            started_at: "2026-07-24T10:00:00.010Z".to_owned(),
        }
    }

    fn verified_running(config: &SubmitExecutionConfig) -> VerifiedRunningTask {
        VerifiedRunningTask::new(
            running(&config.task_id),
            config.budget.verified_effective_limits_for_test(),
        )
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
                        running_sender.send(verified_running(&config)).unwrap();
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
                            running_sender.send(verified_running(&config)).unwrap();
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
        assert_eq!(registry.snapshot(TASK_ID), Ok(None));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );
        assert!(capacity.try_acquire().is_some());

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
                running_sender.send(verified_running(&config)).unwrap();
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

    #[tokio::test]
    async fn injected_finished_storage_failure_enters_fail_stop_and_retains_capacity() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let release = Arc::new(Notify::new());
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
            FailStopSettings::new(Duration::from_secs(5)).unwrap(),
            clock,
        );
        let outcome = coordinate_validated_submit(
            registry.clone(),
            Arc::clone(&capacity),
            Arc::clone(&fail_stop),
            REQUEST_ID.to_owned(),
            ValidatedSubmit::try_from_payload(payload()).unwrap(),
            metadata(TASK_ID),
            {
                let release = Arc::clone(&release);
                move |config, running_sender, _cancellation| async move {
                    running_sender.send(verified_running(&config)).unwrap();
                    release.notified().await;
                    Ok(ExecutionCompletion::Test(finished(&config.task_id)))
                }
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));

        registry.poison_state_for_test();
        release.notify_one();
        timeout(TokioDuration::from_secs(1), async {
            while !fail_stop.is_fail_stopping() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("FINISHED 저장 실패가 fail-stop을 시작해야 합니다");

        assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
        assert_eq!(capacity.retained_for_fail_stop(), 1);
        assert!(capacity.try_acquire().is_none());
        assert_eq!(fail_stop.active_count(), 1);
        let report = fail_stop.first_report().unwrap();
        assert_eq!(report.stage, "활성 실행 소유권 종료");
        assert!(!format!("{report:?}").contains("/usr/bin/true"));
        assert!(!format!("{report:?}").contains("LANG"));
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
                    running_sender.send(verified_running(&config)).unwrap();
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
                running_sender.send(verified_running(&config)).unwrap();
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
                    running_sender.send(verified_running(&config)).unwrap();
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
                running_sender.send(verified_running(&config)).unwrap();
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
                running_sender.send(verified_running(&config)).unwrap();
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

    #[tokio::test]
    async fn fail_stop_rejects_a_new_request_before_task_id_registry_capacity_and_executor() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let fail_stop = fail_stop_runtime();
        fail_stop.activate(CleanupFailureReport::new(
            TASK_ID,
            "시험 정리",
            vec!["작업 cgroup"],
            "실패",
        ));
        let task_ids = Arc::new(AtomicUsize::new(0));
        let start_times = Arc::new(AtomicUsize::new(0));
        let executor_starts = Arc::new(AtomicUsize::new(0));
        let task_id_calls = Arc::clone(&task_ids);
        let start_time_calls = Arc::clone(&start_times);
        let executor_calls = Arc::clone(&executor_starts);
        let metadata = SubmitMetadata::lazy(
            move || {
                task_id_calls.fetch_add(1, Ordering::SeqCst);
                TASK_ID.to_owned()
            },
            "submitted".to_owned(),
            move || {
                start_time_calls.fetch_add(1, Ordering::SeqCst);
                TaskStartTime::new("started".to_owned(), Instant::now())
            },
            Duration::from_secs(1),
        );
        let validated = ValidatedSubmit::try_from_payload(payload()).unwrap();

        let outcome = coordinate_validated_submit(
            registry.clone(),
            Arc::clone(&capacity),
            fail_stop,
            REQUEST_ID.to_owned(),
            validated,
            metadata,
            move |_config, _running_sender, _cancellation| async move {
                executor_calls.fetch_add(1, Ordering::SeqCst);
                Ok(ExecutionCompletion::Test(finished(TASK_ID)))
            },
        )
        .await
        .unwrap();

        assert!(matches!(
            outcome.observation,
            SubmitObservation::Failed(SubmitFailure {
                code: ErrorCode::EnvironmentUnavailable,
                ..
            })
        ));
        assert!(outcome.task_id.is_empty());
        assert_eq!(task_ids.load(Ordering::SeqCst), 0);
        assert_eq!(start_times.load(Ordering::SeqCst), 0);
        assert_eq!(executor_starts.load(Ordering::SeqCst), 0);
        assert!(
            registry
                .snapshot_by_client_request_id(CLIENT_REQUEST_ID)
                .unwrap()
                .is_none()
        );
        assert!(capacity.try_acquire().is_some());
    }

    #[tokio::test]
    async fn fail_stop_keeps_existing_idempotency_and_conflict_without_new_execution() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let fail_stop = fail_stop_runtime();
        let validated = ValidatedSubmit::try_from_payload(payload()).unwrap();
        let first = coordinate_validated_submit(
            registry.clone(),
            Arc::clone(&capacity),
            Arc::clone(&fail_stop),
            REQUEST_ID.to_owned(),
            validated.clone(),
            metadata(TASK_ID),
            move |config, running_sender, _cancellation| async move {
                running_sender.send(verified_running(&config)).unwrap();
                Ok(ExecutionCompletion::Test(finished(&config.task_id)))
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            first.observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ));
        tokio::task::yield_now().await;

        fail_stop.activate(CleanupFailureReport::new(
            TASK_ID,
            "시험 정리",
            vec!["작업 cgroup"],
            "실패",
        ));
        let new_task_ids = Arc::new(AtomicUsize::new(0));
        let new_start_times = Arc::new(AtomicUsize::new(0));
        let executor_starts = Arc::new(AtomicUsize::new(0));
        let task_id_calls = Arc::clone(&new_task_ids);
        let start_time_calls = Arc::clone(&new_start_times);
        let executor_calls = Arc::clone(&executor_starts);
        let duplicate = coordinate_validated_submit(
            registry.clone(),
            Arc::clone(&capacity),
            Arc::clone(&fail_stop),
            OTHER_REQUEST_ID.to_owned(),
            validated,
            SubmitMetadata::lazy(
                move || {
                    task_id_calls.fetch_add(1, Ordering::SeqCst);
                    OTHER_TASK_ID.to_owned()
                },
                "submitted".to_owned(),
                move || {
                    start_time_calls.fetch_add(1, Ordering::SeqCst);
                    TaskStartTime::new("started".to_owned(), Instant::now())
                },
                Duration::from_secs(1),
            ),
            move |_config, _running_sender, _cancellation| async move {
                executor_calls.fetch_add(1, Ordering::SeqCst);
                Ok(ExecutionCompletion::Test(finished(OTHER_TASK_ID)))
            },
        )
        .await
        .unwrap();
        assert_eq!(duplicate.task_id, TASK_ID);
        assert!(matches!(duplicate.observation, SubmitObservation::Task(_)));

        let mut changed = payload();
        changed.command.args.push("different".to_owned());
        let conflict = coordinate_validated_submit(
            registry,
            capacity,
            fail_stop,
            REQUEST_ID.to_owned(),
            ValidatedSubmit::try_from_payload(changed).unwrap(),
            metadata(OTHER_TASK_ID),
            move |_config, _running_sender, _cancellation| async move {
                Ok(ExecutionCompletion::Test(finished(OTHER_TASK_ID)))
            },
        )
        .await;
        assert!(matches!(
            conflict,
            Err(SubmitError::Registry(RegistryError::IdempotencyConflict(_)))
        ));
        assert_eq!(new_task_ids.load(Ordering::SeqCst), 0);
        assert_eq!(new_start_times.load(Ordering::SeqCst), 0);
        assert_eq!(executor_starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn idempotent_running_response_reuses_the_original_verified_effective_limits() {
        let registry = TaskRegistry::new();
        let capacity = task_capacity(1);
        let fail_stop = fail_stop_runtime();
        let validated = ValidatedSubmit::try_from_payload(payload()).unwrap();
        let mut applied_limits = validated.payload().limits.clone();
        applied_limits.memory_max_bytes += 1;
        let release = Arc::new(Notify::new());
        let release_owner = Arc::clone(&release);
        let verified_for_owner = VerifiedEffectiveLimits::for_test(applied_limits.clone());

        let first = coordinate_validated_submit(
            registry.clone(),
            Arc::clone(&capacity),
            Arc::clone(&fail_stop),
            REQUEST_ID.to_owned(),
            validated.clone(),
            metadata(TASK_ID),
            move |config, running_sender, _cancellation| async move {
                running_sender
                    .send(VerifiedRunningTask::new(
                        running(&config.task_id),
                        verified_for_owner,
                    ))
                    .unwrap();
                release_owner.notified().await;
                Ok(ExecutionCompletion::Test(finished(&config.task_id)))
            },
        )
        .await
        .unwrap();
        assert_eq!(
            first.effective_limits.unwrap().into_protocol(),
            applied_limits
        );

        let duplicate_executor_calls = Arc::new(AtomicUsize::new(0));
        let duplicate_calls = Arc::clone(&duplicate_executor_calls);
        let duplicate = coordinate_validated_submit(
            registry,
            capacity,
            fail_stop,
            OTHER_REQUEST_ID.to_owned(),
            validated,
            metadata(OTHER_TASK_ID),
            move |_config, _running_sender, _cancellation| async move {
                duplicate_calls.fetch_add(1, Ordering::SeqCst);
                Ok(ExecutionCompletion::Test(finished(OTHER_TASK_ID)))
            },
        )
        .await
        .unwrap();

        assert_eq!(duplicate.task_id, TASK_ID);
        assert_eq!(
            duplicate.effective_limits.unwrap().into_protocol(),
            applied_limits
        );
        assert_eq!(duplicate_executor_calls.load(Ordering::SeqCst), 0);
        release.notify_waiters();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_task_timing_begins_after_exec_gate_commit() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_TASK_TIMING_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let fail_stop = fail_stop_runtime();
        let coordinator = Arc::new(
            SubmitCoordinator::initialize(
                environment,
                TaskCapacitySettings::new(1).unwrap(),
                Arc::clone(&fail_stop),
            )
            .unwrap(),
        );
        let reached = Arc::new(ThreadBarrier::new(2));
        let release = Arc::new(ThreadBarrier::new(2));
        fail_stop.set_before_start_commit_hook({
            let reached = Arc::clone(&reached);
            let release = Arc::clone(&release);
            Arc::new(move || {
                reached.wait();
                release.wait();
            })
        });

        let logical_base = Instant::now();
        let start_calls = Arc::new(AtomicUsize::new(0));
        let finish_calls = Arc::new(AtomicUsize::new(0));
        let metadata = metadata_with_start_clock(
            TASK_ID,
            "2026-07-24T10:00:05.000Z",
            logical_base + Duration::from_secs(5),
            Arc::clone(&start_calls),
        );
        let finish_calls_for_task = Arc::clone(&finish_calls);
        let marker = std::env::temp_dir().join(format!(
            "taskcage-start-timing-{}-{}.marker",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_file(&marker);
        let marker_text = marker.to_string_lossy().into_owned();
        let touch_bin = std::env::var("TASKCAGE_TIMING_MARKER_BIN")
            .expect("timing 통합 시험용 marker 실행 파일 경로가 필요합니다");
        let request_payload = linux_payload(
            CLIENT_REQUEST_ID,
            &touch_bin,
            &[marker_text.as_str()],
            5_000,
        );
        let submitting = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            let request_payload = request_payload.clone();
            async move {
                coordinator
                    .submit(
                        request(PROTOCOL_VERSION, REQUEST_ID, request_payload),
                        metadata,
                        move || {
                            finish_calls_for_task.fetch_add(1, Ordering::SeqCst);
                            (
                                "2026-07-24T10:00:08.000Z".to_owned(),
                                logical_base + Duration::from_secs(8),
                            )
                        },
                    )
                    .await
            }
        });

        tokio::task::spawn_blocking({
            let reached = Arc::clone(&reached);
            move || reached.wait()
        })
        .await
        .unwrap();
        assert_eq!(start_calls.load(Ordering::SeqCst), 0);
        assert_eq!(finish_calls.load(Ordering::SeqCst), 0);
        assert!(
            !marker.exists(),
            "exec gate commit 전 target이 실행됐습니다"
        );
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .unwrap();

        let first = submitting.await.unwrap().unwrap();
        assert!(matches!(
            first.observation,
            SubmitObservation::Task(TaskPayload::Running {
                ref submitted_at,
                ref started_at,
                ..
            }) if submitted_at == "2026-07-24T10:00:00.000Z"
                && started_at == "2026-07-24T10:00:05.000Z"
        ));
        assert_eq!(start_calls.load(Ordering::SeqCst), 1);

        let finished = wait_for_finished(&coordinator, TASK_ID).await;
        assert!(matches!(
            &finished,
            TaskPayload::Finished {
                timing,
                termination_reason: TerminationReason::Exited,
                ..
            } if timing.submitted_at == "2026-07-24T10:00:00.000Z"
                && timing.started_at == "2026-07-24T10:00:05.000Z"
                && timing.finished_at == "2026-07-24T10:00:08.000Z"
                && timing.wall_time_ms == 3_000
        ));
        assert_eq!(start_calls.load(Ordering::SeqCst), 1);
        assert_eq!(finish_calls.load(Ordering::SeqCst), 1);
        assert!(
            marker.exists(),
            "FINISHED 전 target marker가 생성돼야 합니다"
        );

        let duplicate_start_calls = Arc::new(AtomicUsize::new(0));
        let duplicate = coordinator
            .submit(
                request(PROTOCOL_VERSION, OTHER_REQUEST_ID, request_payload),
                metadata_with_start_clock(
                    OTHER_TASK_ID,
                    "duplicate-start-must-not-be-used",
                    logical_base,
                    Arc::clone(&duplicate_start_calls),
                ),
                || panic!("멱등 재전송은 새로운 finished clock을 호출하면 안 됩니다"),
            )
            .await
            .unwrap();
        assert_eq!(duplicate.task_id, TASK_ID);
        assert_eq!(duplicate.observation, SubmitObservation::Task(finished));
        assert_eq!(duplicate_start_calls.load(Ordering::SeqCst), 0);
        fs::remove_file(&marker).unwrap();

        let remaining_jobs = fs::read_dir(jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(remaining_jobs, 0, "작업 cgroup이 남아 있습니다");
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
        let coordinator = SubmitCoordinator::initialize(
            environment,
            TaskCapacitySettings::new(1).unwrap(),
            FailStopCoordinator::new(FailStopSettings::new(Duration::from_secs(5)).unwrap()),
        )
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

        let exec_start = Instant::now();
        let exec_start_calls = Arc::new(AtomicUsize::new(0));
        let exec_finish_calls = Arc::new(AtomicUsize::new(0));
        let exec_finish_calls_for_task = Arc::clone(&exec_finish_calls);
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
                metadata_with_start_clock(
                    EXEC_FAILURE_TASK_ID,
                    "2026-07-24T10:00:05.000Z",
                    exec_start,
                    Arc::clone(&exec_start_calls),
                ),
                move || {
                    exec_finish_calls_for_task.fetch_add(1, Ordering::SeqCst);
                    (
                        "2026-07-24T10:00:05.012Z".to_owned(),
                        exec_start + Duration::from_millis(12),
                    )
                },
            )
            .await
            .unwrap();
        let exec_process = match exec_failed.observation {
            SubmitObservation::Task(finished) => {
                assert!(matches!(
                    &finished,
                    TaskPayload::Finished { timing, .. }
                        if timing.started_at == "2026-07-24T10:00:05.000Z"
                            && timing.finished_at == "2026-07-24T10:00:05.012Z"
                            && timing.wall_time_ms == 12
                ));
                assert_finished_reason(finished, TerminationReason::ExecutionFailed)
            }
            SubmitObservation::Failed(failure) => {
                panic!("exec 시작 실패는 정리된 FINISHED여야 합니다: {failure:?}")
            }
        };
        assert_eq!(exec_process.exit_code, None);
        assert_eq!(exec_process.signal, None);
        assert_eq!(exec_start_calls.load(Ordering::SeqCst), 1);
        assert_eq!(exec_finish_calls.load(Ordering::SeqCst), 1);
        assert!(!coordinator.runner.cleanup_is_uncertain());

        let remaining_jobs = fs::read_dir(jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(remaining_jobs, 0, "작업 cgroup이 남아 있습니다");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn actual_fail_stop_before_exec_commit_rolls_back_without_target_start() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_EXEC_GATE_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let fail_stop = fail_stop_runtime();
        let coordinator = Arc::new(
            SubmitCoordinator::initialize(
                environment,
                TaskCapacitySettings::new(1).unwrap(),
                Arc::clone(&fail_stop),
            )
            .unwrap(),
        );
        let ghost_bin = std::env::var("TASKCAGE_EXEC_GATE_GHOST_BIN")
            .expect("exec gate 통합 시험용 ghost fixture 경로가 필요합니다");
        let marker = std::env::temp_dir().join(format!(
            "taskcage-exec-gate-{}-{}.ready",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let marker_text = marker.to_string_lossy().into_owned();
        let start_calls = Arc::new(AtomicUsize::new(0));
        let reached = Arc::new(ThreadBarrier::new(2));
        let release = Arc::new(ThreadBarrier::new(2));
        fail_stop.set_before_start_commit_hook({
            let reached = Arc::clone(&reached);
            let release = Arc::clone(&release);
            Arc::new(move || {
                reached.wait();
                release.wait();
            })
        });

        let submitting = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            let start_calls_for_submit = Arc::clone(&start_calls);
            async move {
                coordinator
                    .submit(
                        request(
                            PROTOCOL_VERSION,
                            REQUEST_ID,
                            linux_payload(
                                CLIENT_REQUEST_ID,
                                &ghost_bin,
                                &["--hold-parent", &marker_text],
                                60_000,
                            ),
                        ),
                        metadata_with_start_clock(
                            TASK_ID,
                            "must-not-be-created",
                            Instant::now(),
                            start_calls_for_submit,
                        ),
                        || ("finished".to_owned(), Instant::now()),
                    )
                    .await
            }
        });

        reached.wait();
        let job_path = jobs_path.join(format!("job-{TASK_ID}"));
        let pending_pid = fs::read_to_string(job_path.join("cgroup.procs"))
            .unwrap()
            .lines()
            .next()
            .expect("pending child가 작업 cgroup에 있어야 합니다")
            .parse::<u32>()
            .unwrap();
        let deadline = fail_stop.activate(CleanupFailureReport::new(
            TASK_ID,
            "exec gate 경쟁 통합 시험",
            vec!["pending child", "작업 cgroup"],
            "exec gate를 열지 않고 기존 deadline으로 정리",
        ));
        let repeated = fail_stop.activate(CleanupFailureReport::new(
            "later-task",
            "후속 실패",
            vec!["작업 cgroup"],
            "deadline 유지",
        ));
        release.wait();

        let outcome = timeout(TokioDuration::from_secs(10), submitting)
            .await
            .expect("fail-stop deadline 안에 pending 실행을 정리해야 합니다")
            .unwrap()
            .unwrap();
        assert!(matches!(
            outcome.observation,
            SubmitObservation::Failed(SubmitFailure {
                code: ErrorCode::InternalError,
                ..
            })
        ));
        assert_eq!(deadline, repeated);
        assert_eq!(fail_stop.deadline(), Some(deadline));
        assert!(!fail_stop.start_is_committed(TASK_ID));
        assert_eq!(fail_stop.active_count(), 0);
        assert_eq!(start_calls.load(Ordering::SeqCst), 0);
        assert_eq!(coordinator.snapshot(TASK_ID), Ok(None));
        assert_eq!(
            coordinator.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );
        assert_eq!(coordinator.capacity.retained_for_fail_stop(), 1);
        assert!(coordinator.capacity.try_acquire().is_none());
        assert!(!marker.exists(), "target marker가 생성되면 안 됩니다");
        assert!(
            !Path::new(&format!("/proc/{pending_pid}")).exists(),
            "pending child가 남아 있습니다: {pending_pid}"
        );
        assert!(!job_path.exists(), "작업 cgroup이 남아 있습니다");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_fail_stop_cleans_all_active_cgroups_and_blocks_new_execution() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_FAIL_STOP_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let fail_stop = fail_stop_runtime();
        let coordinator = SubmitCoordinator::initialize(
            environment,
            TaskCapacitySettings::new(2).unwrap(),
            Arc::clone(&fail_stop),
        )
        .unwrap();
        let ghost_bin = std::env::var("TASKCAGE_FAIL_STOP_GHOST_BIN")
            .expect("통합 시험용 ghost-tree 절대 경로가 필요합니다");
        let ready_path = std::env::temp_dir().join(format!(
            "taskcage-fail-stop-{}-{}.ready",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let ready_text = ready_path.to_string_lossy().into_owned();

        for (request_id, client_request_id, task_id, program, args) in [
            (
                REQUEST_ID,
                CLIENT_REQUEST_ID,
                TASK_ID,
                "/bin/sleep",
                vec!["30"],
            ),
            (
                OTHER_REQUEST_ID,
                NONZERO_CLIENT_REQUEST_ID,
                NONZERO_TASK_ID,
                ghost_bin.as_str(),
                vec!["--hold-parent", ready_text.as_str()],
            ),
        ] {
            let outcome = coordinator
                .submit(
                    request(
                        PROTOCOL_VERSION,
                        request_id,
                        linux_payload(client_request_id, program, &args, 60_000),
                    ),
                    metadata(task_id),
                    || ("finished".to_owned(), Instant::now()),
                )
                .await
                .unwrap();
            assert!(matches!(
                outcome.observation,
                SubmitObservation::Task(TaskPayload::Running { .. })
            ));
        }
        timeout(TokioDuration::from_secs(5), async {
            while !ready_path.exists() {
                tokio::time::sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("ghost descendant가 실행됐다는 근거가 필요합니다");
        let ghost_pids = fs::read_to_string(&ready_path)
            .unwrap()
            .lines()
            .map(|line| line.split_once('=').unwrap().1.parse::<u32>().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(fail_stop.active_count(), 2);
        assert!(fail_stop.start_is_committed(TASK_ID));
        assert!(fail_stop.start_is_committed(NONZERO_TASK_ID));
        assert!(matches!(
            coordinator.snapshot(TASK_ID).unwrap(),
            Some(TaskPayload::Running { .. })
        ));
        assert!(matches!(
            coordinator.snapshot(NONZERO_TASK_ID).unwrap(),
            Some(TaskPayload::Running { .. })
        ));

        let deadline = fail_stop.activate(CleanupFailureReport::new(
            TASK_ID,
            "통합 시험 정리 불확실성 주입",
            vec!["작업 cgroup"],
            "process-wide 정리 시작",
        ));
        let repeated = fail_stop.activate(CleanupFailureReport::new(
            "later-task",
            "후속 정리 불확실성",
            vec!["작업 cgroup"],
            "기존 deadline 유지",
        ));
        assert_eq!(deadline, repeated);

        timeout(TokioDuration::from_secs(5), async {
            loop {
                let all_finished = [TASK_ID, NONZERO_TASK_ID].iter().all(|task_id| {
                    matches!(
                        coordinator.snapshot(task_id).unwrap(),
                        Some(TaskPayload::Finished {
                            termination_reason: TerminationReason::DaemonError,
                            ..
                        })
                    )
                });
                if all_finished {
                    break;
                }
                tokio::time::sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("fail-stop은 활성 작업 전체를 정리해야 합니다");
        assert_eq!(fail_stop.deadline(), Some(deadline));
        assert_eq!(fail_stop.active_count(), 0);

        let rejected = coordinator
            .submit(
                request(
                    PROTOCOL_VERSION,
                    REQUEST_ID,
                    linux_payload(TIMEOUT_CLIENT_REQUEST_ID, "/bin/true", &[], 5_000),
                ),
                metadata(TIMEOUT_TASK_ID),
                || ("finished".to_owned(), Instant::now()),
            )
            .await
            .unwrap();
        assert!(matches!(
            rejected.observation,
            SubmitObservation::Failed(SubmitFailure {
                code: ErrorCode::EnvironmentUnavailable,
                ..
            })
        ));
        assert!(coordinator.snapshot(TIMEOUT_TASK_ID).unwrap().is_none());
        let remaining_jobs = fs::read_dir(jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(
            remaining_jobs, 0,
            "fail-stop 뒤 작업 cgroup이 남아 있습니다"
        );
        assert!(
            ghost_pids
                .iter()
                .all(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists()),
            "fail-stop 뒤 ghost descendant가 남아 있습니다: {ghost_pids:?}"
        );
        fs::remove_file(ready_path).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn actual_cleanup_fault_reaches_runner_and_submit_state() {
        if std::env::var_os("TASKCAGE_RUN_CLEANUP_FAULT_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let point = match std::env::var("TASKCAGE_CLEANUP_FAULT")
            .expect("실제 cleanup fault 이름이 필요합니다")
            .as_str()
        {
            "pending-clone-abort" => CleanupFaultPoint::PendingCloneAbort,
            "exec-gate-cleanup" => CleanupFaultPoint::ExecGateCleanup,
            "cgroup-kill" => CleanupFaultPoint::CgroupKill,
            "direct-child-reap" => CleanupFaultPoint::DirectChildReap,
            "populated-zero" => CleanupFaultPoint::PopulatedZero,
            "statistics" => CleanupFaultPoint::Statistics,
            "cgroup-removal" => CleanupFaultPoint::CgroupRemoval,
            "stdout-reader" => CleanupFaultPoint::StdoutReader,
            "stderr-reader" => CleanupFaultPoint::StderrReader,
            other => panic!("알 수 없는 cleanup fault입니다: {other}"),
        };
        let mode = match std::env::var("TASKCAGE_CLEANUP_FAULT_MODE")
            .unwrap_or_else(|_| "once".to_owned())
            .as_str()
        {
            "once" => CleanupFaultMode::Once,
            "persistent" => CleanupFaultMode::Persistent,
            other => panic!("알 수 없는 cleanup fault mode입니다: {other}"),
        };
        let faults = Arc::new(CleanupFaults::new(point, mode));
        let clock_calls = Arc::new(AtomicUsize::new(0));
        let fail_stop = FailStopCoordinator::with_test_clock(
            FailStopSettings::new(Duration::from_secs(1)).unwrap(),
            {
                let clock_calls = Arc::clone(&clock_calls);
                Arc::new(move || {
                    clock_calls.fetch_add(1, Ordering::SeqCst);
                    Instant::now()
                })
            },
        );
        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let coordinator = SubmitCoordinator::initialize_with_cleanup_faults(
            environment,
            TaskCapacitySettings::new(1).unwrap(),
            Arc::clone(&fail_stop),
            Arc::clone(&faults),
        )
        .unwrap();
        let marker = std::env::temp_dir().join(format!(
            "taskcage-cleanup-fault-{}-{point:?}",
            std::process::id()
        ));

        if matches!(
            point,
            CleanupFaultPoint::PendingCloneAbort | CleanupFaultPoint::ExecGateCleanup
        ) {
            let touch_bin = std::env::var("TASKCAGE_CLEANUP_FAULT_TOUCH_BIN")
                .expect("cleanup fault target marker 실행 파일이 필요합니다");
            let outcome = coordinator
                .submit(
                    request(
                        PROTOCOL_VERSION,
                        REQUEST_ID,
                        linux_payload(
                            CLIENT_REQUEST_ID,
                            &touch_bin,
                            &[marker.to_string_lossy().as_ref()],
                            30_000,
                        ),
                    ),
                    cleanup_fault_metadata(TASK_ID),
                    || ("finished".to_owned(), Instant::now()),
                )
                .await
                .unwrap();
            assert!(matches!(
                outcome.observation,
                SubmitObservation::Failed(SubmitFailure {
                    code: ErrorCode::InternalError,
                    ..
                })
            ));
            assert!(
                !marker.exists(),
                "exec gate 전에 target이 실행되면 안 됩니다"
            );
            assert_eq!(coordinator.snapshot(TASK_ID), Ok(None));
            assert_eq!(
                faults.attempts(),
                if point == CleanupFaultPoint::PendingCloneAbort {
                    2
                } else {
                    1
                }
            );

            if point == CleanupFaultPoint::PendingCloneAbort {
                assert!(fail_stop.is_fail_stopping());
                assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
                assert_eq!(coordinator.capacity.retained_for_fail_stop(), 1);
            } else {
                assert!(!fail_stop.is_fail_stopping());
                assert_eq!(clock_calls.load(Ordering::SeqCst), 0);
                assert!(coordinator.capacity.try_acquire().is_some());
            }
        } else {
            let (program, args) = if matches!(
                point,
                CleanupFaultPoint::StdoutReader | CleanupFaultPoint::StderrReader
            ) {
                (
                    std::env::var("TASKCAGE_CLEANUP_FAULT_OUTPUT_BIN")
                        .expect("cleanup fault output fixture가 필요합니다"),
                    vec!["both"],
                )
            } else {
                ("/bin/sleep".to_owned(), vec!["30"])
            };
            let outcome = coordinator
                .submit(
                    request(
                        PROTOCOL_VERSION,
                        REQUEST_ID,
                        linux_payload(CLIENT_REQUEST_ID, &program, &args, 60_000),
                    ),
                    cleanup_fault_metadata(TASK_ID),
                    || ("finished".to_owned(), Instant::now()),
                )
                .await
                .unwrap();
            assert!(matches!(
                outcome.observation,
                SubmitObservation::Task(TaskPayload::Running { .. })
            ));
            let job_path = jobs_path.join(format!("job-{TASK_ID}"));
            let child_pid = fs::read_to_string(job_path.join("cgroup.procs"))
                .unwrap()
                .lines()
                .next()
                .expect("실제 target PID가 필요합니다")
                .parse::<u32>()
                .unwrap();
            coordinator.registry.request_cancel(TASK_ID).unwrap();

            timeout(TokioDuration::from_secs(10), async {
                while coordinator.capacity.retained_for_fail_stop() == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("실제 cleanup fault가 fail-stop capacity 보존으로 이어져야 합니다");
            assert!(fail_stop.is_fail_stopping());
            assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
            assert!(faults.attempts() >= 1);
            if mode == CleanupFaultMode::Persistent {
                faults.disable();
            }

            let recovered = mode == CleanupFaultMode::Once
                && matches!(
                    point,
                    CleanupFaultPoint::CgroupKill
                        | CleanupFaultPoint::DirectChildReap
                        | CleanupFaultPoint::PopulatedZero
                        | CleanupFaultPoint::CgroupRemoval
                );
            let snapshot = coordinator.snapshot(TASK_ID).unwrap().unwrap();
            if recovered {
                assert!(matches!(
                    snapshot,
                    TaskPayload::Finished {
                        termination_reason: TerminationReason::Cancelled,
                        ..
                    }
                ));
                assert!(
                    faults.attempts() >= 2,
                    "첫 실패 뒤 실제 재시도가 필요합니다"
                );
            } else {
                assert!(matches!(snapshot, TaskPayload::Running { .. }));
            }

            let rejected = coordinator
                .submit(
                    request(
                        PROTOCOL_VERSION,
                        OTHER_REQUEST_ID,
                        linux_payload(NONZERO_CLIENT_REQUEST_ID, "/bin/true", &[], 5_000),
                    ),
                    metadata(NONZERO_TASK_ID),
                    || ("finished".to_owned(), Instant::now()),
                )
                .await
                .unwrap();
            assert!(matches!(
                rejected.observation,
                SubmitObservation::Failed(SubmitFailure {
                    code: ErrorCode::EnvironmentUnavailable,
                    ..
                })
            ));
            assert_eq!(coordinator.snapshot(NONZERO_TASK_ID), Ok(None));

            if point == CleanupFaultPoint::CgroupKill && mode == CleanupFaultMode::Persistent {
                teardown_persistent_kill_fault(&job_path, child_pid).await;
            }

            timeout(TokioDuration::from_secs(2), async {
                while job_path.exists() || Path::new(&format!("/proc/{child_pid}")).exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("시험 종료 전에 target과 작업 cgroup을 회수해야 합니다");
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_actual_cleanup_faults_share_one_fail_stop_deadline() {
        if std::env::var_os("TASKCAGE_RUN_CLEANUP_FAULT_CONCURRENT").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let faults = Arc::new(CleanupFaults::new_pair(
            CleanupFaultPoint::StdoutReader,
            CleanupFaultPoint::StderrReader,
            CleanupFaultMode::Persistent,
        ));
        let clock_calls = Arc::new(AtomicUsize::new(0));
        let fail_stop = FailStopCoordinator::with_test_clock(
            FailStopSettings::new(Duration::from_secs(1)).unwrap(),
            {
                let clock_calls = Arc::clone(&clock_calls);
                Arc::new(move || {
                    clock_calls.fetch_add(1, Ordering::SeqCst);
                    Instant::now()
                })
            },
        );
        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let coordinator = SubmitCoordinator::initialize_with_cleanup_faults(
            environment,
            TaskCapacitySettings::new(2).unwrap(),
            Arc::clone(&fail_stop),
            Arc::clone(&faults),
        )
        .unwrap();

        for (request_id, client_request_id, task_id) in [
            (REQUEST_ID, CLIENT_REQUEST_ID, TASK_ID),
            (OTHER_REQUEST_ID, NONZERO_CLIENT_REQUEST_ID, OTHER_TASK_ID),
        ] {
            let outcome = coordinator
                .submit(
                    request(
                        PROTOCOL_VERSION,
                        request_id,
                        linux_payload(client_request_id, "/bin/sleep", &["30"], 60_000),
                    ),
                    cleanup_fault_metadata(task_id),
                    || ("finished".to_owned(), Instant::now()),
                )
                .await
                .unwrap();
            assert!(matches!(
                outcome.observation,
                SubmitObservation::Task(TaskPayload::Running { .. })
            ));
        }

        let resources = [TASK_ID, OTHER_TASK_ID].map(|task_id| {
            let job_path = jobs_path.join(format!("job-{task_id}"));
            let pid = fs::read_to_string(job_path.join("cgroup.procs"))
                .unwrap()
                .lines()
                .next()
                .expect("동시 실행 target PID가 필요합니다")
                .parse::<u32>()
                .unwrap();
            (job_path, pid)
        });
        let first_cancel = coordinator.registry.request_cancel(TASK_ID).unwrap();
        let second_cancel = coordinator.registry.request_cancel(OTHER_TASK_ID).unwrap();
        drop((first_cancel, second_cancel));

        timeout(TokioDuration::from_secs(5), async {
            while coordinator.capacity.retained_for_fail_stop() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("두 실제 cleanup 실패가 모두 capacity를 보존해야 합니다");
        assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fail_stop.active_count(), 2);
        assert!(faults.attempts_for(CleanupFaultPoint::StdoutReader) >= 2);
        assert!(faults.attempts_for(CleanupFaultPoint::StderrReader) >= 2);
        for task_id in [TASK_ID, OTHER_TASK_ID] {
            assert!(matches!(
                coordinator.snapshot(task_id).unwrap(),
                Some(TaskPayload::Running { .. })
            ));
        }
        faults.disable();

        timeout(TokioDuration::from_secs(2), async {
            while resources
                .iter()
                .any(|(path, pid)| path.exists() || Path::new(&format!("/proc/{pid}")).exists())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("동시 fault 시험 뒤 target과 작업 cgroup을 회수해야 합니다");
    }
}
