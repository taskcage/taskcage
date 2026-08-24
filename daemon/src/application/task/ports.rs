//! Task use case가 infrastructure에 요구하는 좁은 port다.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use taskcage_core::task::TaskSnapshot;
use tokio::sync::oneshot;

use super::cancellation::{CancellationRuntime, RunningCancellation};
use crate::application::UseCaseErrorCode;
use crate::execution_plan::ResolvedExecutionPlan;
use crate::resource_budget::VerifiedEffectiveLimits;

use super::completion;
use super::submit::{SubmitContext, SubmitError, SubmitOutcome, ValidatedSubmit};

pub(crate) const REGISTRY_CAPACITY_EXHAUSTED_MESSAGE: &str =
    "task registry retention capacity is exhausted";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum RegistryError {
    #[error("새 작업은 RUNNING snapshot으로 등록해야 합니다")]
    RunningSnapshotRequired,
    #[error("작업 완료에는 FINISHED snapshot이 필요합니다")]
    FinishedSnapshotRequired,
    #[error("taskId가 이미 등록되어 있습니다: {0}")]
    TaskAlreadyExists(String),
    #[error("같은 clientRequestId에 다른 submit payload가 사용됐습니다: {0}")]
    IdempotencyConflict(String),
    #[error("작업을 찾을 수 없습니다: {0}")]
    TaskNotFound(String),
    #[error("완료된 작업 결과는 바꿀 수 없습니다: {0}")]
    TaskAlreadyFinished(String),
    #[error("완료 결과의 taskId가 예약과 다릅니다: expected={expected}, actual={actual}")]
    TaskIdMismatch { expected: String, actual: String },
    #[error("Task Registry 상태 잠금을 사용할 수 없습니다")]
    StateUnavailable,
    #[error("RUNNING 작업에 적용 확인된 effectiveLimits가 없습니다: {0}")]
    VerifiedEffectiveLimitsRequired(String),
    #[error("{REGISTRY_CAPACITY_EXHAUSTED_MESSAGE}")]
    CapacityExhausted,
}

impl RegistryError {
    pub(crate) fn error_code(&self) -> Option<UseCaseErrorCode> {
        match self {
            Self::IdempotencyConflict(_) => Some(UseCaseErrorCode::IdempotencyConflict),
            Self::TaskNotFound(_) => Some(UseCaseErrorCode::TaskNotFound),
            Self::TaskAlreadyFinished(_) => Some(UseCaseErrorCode::TaskAlreadyFinished),
            Self::StateUnavailable => Some(UseCaseErrorCode::InternalError),
            Self::VerifiedEffectiveLimitsRequired(_) => Some(UseCaseErrorCode::InternalError),
            Self::CapacityExhausted => Some(UseCaseErrorCode::CapacityExhausted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmitFailure {
    pub(crate) code: UseCaseErrorCode,
    pub(crate) message: String,
}

impl SubmitFailure {
    pub(crate) fn new(code: UseCaseErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubmitObservation {
    Task(TaskSnapshot),
    Failed(SubmitFailure),
}

/// 멱등 예약과 capacity 확보를 통과한 submit use case만 실행 port를 열 수 있다.
#[derive(Debug)]
pub(crate) struct RunnerPermit(());

impl RunnerPermit {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskStartTime {
    wall_clock: String,
    monotonic: Instant,
}

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

pub(crate) struct TaskStartTimeSource {
    capture: Option<Box<dyn FnOnce() -> TaskStartTime + Send + 'static>>,
}

impl std::fmt::Debug for TaskStartTimeSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TaskStartTimeSource(<lazy clock>)")
    }
}

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

#[derive(Debug)]
pub(crate) struct VerifiedRunningTask {
    snapshot: TaskSnapshot,
    effective_limits: VerifiedEffectiveLimits,
}

impl VerifiedRunningTask {
    pub(crate) fn new(snapshot: TaskSnapshot, effective_limits: VerifiedEffectiveLimits) -> Self {
        Self {
            snapshot,
            effective_limits,
        }
    }

    pub(super) fn into_parts(self) -> (TaskSnapshot, VerifiedEffectiveLimits) {
        (self.snapshot, self.effective_limits)
    }
}

#[derive(Debug)]
pub(crate) struct TaskRunConfig {
    pub(crate) task_id: String,
    pub(crate) submitted_at: String,
    pub(crate) start_time: TaskStartTimeSource,
    pub(crate) cleanup_timeout: Duration,
    pub(crate) plan: ResolvedExecutionPlan,
}

#[derive(Debug)]
pub(crate) struct CompletedTask {
    payload: TaskSnapshot,
}

impl CompletedTask {
    pub(crate) fn new(payload: TaskSnapshot) -> Result<Self, completion::CompletionError> {
        completion::require_finished(payload).map(|payload| Self { payload })
    }

    pub(crate) fn into_payload(self) -> TaskSnapshot {
        self.payload
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskRunFailureKind {
    CgroupReadBackMismatch,
    CgroupReadBackRollbackUncertain,
    Other,
}

#[derive(Debug)]
pub(crate) struct TaskRunFailure {
    message: String,
    kind: TaskRunFailureKind,
    capacity_reusable: bool,
    cleanup_complete: bool,
}

impl TaskRunFailure {
    pub(crate) fn new(
        error: impl std::fmt::Display,
        kind: TaskRunFailureKind,
        capacity_reusable: bool,
        cleanup_complete: bool,
    ) -> Self {
        Self {
            message: error.to_string(),
            kind,
            capacity_reusable,
            cleanup_complete,
        }
    }

    pub(crate) fn with_reusable_capacity(error: impl std::fmt::Display) -> Self {
        Self::new(error, TaskRunFailureKind::Other, true, true)
    }

    pub(crate) fn capacity_reusable(&self) -> bool {
        self.capacity_reusable
    }

    pub(crate) fn into_message(self) -> String {
        self.message
    }

    pub(crate) fn kind(&self) -> TaskRunFailureKind {
        self.kind
    }

    pub(crate) fn cleanup_complete(&self) -> bool {
        self.cleanup_complete
    }
}

pub(crate) type FinishedTime = Box<dyn FnOnce() -> (String, Instant) + Send + 'static>;

/// Linux adapter는 cleanup이 확인된 FINISHED 또는 cleanup 상태가 명시된 실패만 반환한다.
pub(crate) trait TaskExecutionPort: std::fmt::Debug + Send + Sync {
    fn execute_task(
        &self,
        permit: RunnerPermit,
        config: TaskRunConfig,
        running_sender: oneshot::Sender<VerifiedRunningTask>,
        cancellation: CancellationRuntime,
        finished_time: FinishedTime,
    ) -> Pin<Box<dyn Future<Output = Result<CompletedTask, TaskRunFailure>> + Send + '_>>;

    #[cfg(test)]
    fn cleanup_is_uncertain(&self) -> bool;
}

pub(crate) trait TaskQueryPort {
    fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, RegistryError>;

    fn snapshot_by_client_request_id(
        &self,
        client_request_id: &str,
    ) -> Result<Option<TaskSnapshot>, RegistryError>;

    fn has_client_request_id(&self, client_request_id: &str) -> Result<bool, RegistryError>;
}

pub(crate) trait TaskCancellationPort {
    fn cancel_and_wait(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<TaskSnapshot, RegistryError>> + Send;
}

/// Protocol adapter가 Task application에 호출할 수 있는 use case 표면이다.
pub(crate) trait TaskUseCases {
    fn submit_validated(
        &self,
        request_id: String,
        validated: ValidatedSubmit,
        context: SubmitContext,
    ) -> impl Future<Output = Result<SubmitOutcome, SubmitError>> + Send;

    fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, RegistryError>;

    fn cancel(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<TaskSnapshot, RegistryError>> + Send;
}

pub(crate) enum SubmitReservation<O, W> {
    Owner(O),
    Existing(W),
}

pub(crate) trait SubmitWaiterPort: Send + 'static {
    fn task_id(&self) -> &str;

    fn wait(self) -> Pin<Box<dyn Future<Output = SubmitObservation> + Send>>;
}

pub(crate) trait CompletionPublicationPort {
    fn publish_completion(self) -> TaskSnapshot;
}

pub(crate) trait SubmitOwnerPort: Send + 'static {
    type Publication: CompletionPublicationPort;

    fn task_id(&self) -> &str;

    fn request(&self) -> &ValidatedSubmit;

    fn publish_running_with_cancellation(
        &self,
        snapshot: TaskSnapshot,
        effective_limits: VerifiedEffectiveLimits,
        cancellation: RunningCancellation,
    ) -> Result<TaskSnapshot, RegistryError>;

    fn finish(self, snapshot: TaskSnapshot) -> Result<Self::Publication, RegistryError>;

    fn fail(self, failure: SubmitFailure) -> SubmitObservation;

    fn rollback_before_running(
        self,
        failure: SubmitFailure,
    ) -> Result<SubmitObservation, RegistryError>;
}

/// 멱등 예약과 현재 snapshot 접근에 필요한 최소 Registry 계약이다.
pub(crate) trait TaskSubmissionPort: TaskQueryPort + Clone + Send + Sync + 'static {
    type Owner: SubmitOwnerPort;
    type Waiter: SubmitWaiterPort;

    fn existing_submit(
        &self,
        request: &ValidatedSubmit,
    ) -> Result<Option<Self::Waiter>, RegistryError>;

    fn reserve_submit_with<F>(
        &self,
        request: ValidatedSubmit,
        make_task_id: F,
    ) -> Result<SubmitReservation<Self::Owner, Self::Waiter>, RegistryError>
    where
        F: FnOnce() -> String;

    fn effective_limits_for(
        &self,
        task_id: &str,
        observation: &SubmitObservation,
    ) -> Result<Option<VerifiedEffectiveLimits>, RegistryError>;
}
