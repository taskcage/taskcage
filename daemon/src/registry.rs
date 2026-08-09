//! 실행 중인 작업, 완료 결과와 idempotent submit 예약을 메모리에 보관한다.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Notify;

#[cfg(test)]
use crate::cancellation::cancellation_channel;
use crate::cancellation::{CancellationWaiter, RunningCancellation};
use crate::protocol::{ErrorCode, SubmitTaskPayload, TaskPayload};
use crate::resource_budget::VerifiedEffectiveLimits;
use crate::submit::ValidatedSubmit;

pub(crate) const MIN_FINISHED_RETENTION: Duration = Duration::from_secs(10 * 60);

pub(crate) const REGISTRY_CAPACITY_EXHAUSTED_MESSAGE: &str =
    "task registry retention capacity is exhausted";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TaskRegistrySettings {
    max_tasks: NonZeroUsize,
}

impl TaskRegistrySettings {
    pub(crate) fn new(max_tasks: usize) -> Result<Self, TaskRegistrySettingsError> {
        let max_tasks =
            NonZeroUsize::new(max_tasks).ok_or(TaskRegistrySettingsError::ZeroMaximum)?;
        Ok(Self { max_tasks })
    }

    pub(crate) fn max_tasks(self) -> usize {
        self.max_tasks.get()
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskRegistrySettingsError {
    #[error("최대 Registry 작업 수는 0보다 커야 합니다")]
    ZeroMaximum,
}

pub(crate) trait RegistryClock {
    fn now(&self) -> Instant;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MonotonicClock;

impl RegistryClock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
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
    pub(crate) fn error_code(&self) -> Option<ErrorCode> {
        match self {
            Self::IdempotencyConflict(_) => Some(ErrorCode::IdempotencyConflict),
            Self::TaskNotFound(_) => Some(ErrorCode::TaskNotFound),
            Self::TaskAlreadyFinished(_) => Some(ErrorCode::TaskAlreadyFinished),
            Self::StateUnavailable => Some(ErrorCode::InternalError),
            Self::VerifiedEffectiveLimitsRequired(_) => Some(ErrorCode::InternalError),
            Self::CapacityExhausted => Some(ErrorCode::CapacityExhausted),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct TaskRecord {
    snapshot: TaskPayload,
    effective_limits: Option<VerifiedEffectiveLimits>,
    finished_monotonic: Option<Instant>,
    cancellation: Option<RunningCancellation>,
}

#[derive(Debug)]
struct RequestRecord {
    task_id: String,
    payload: SubmitTaskPayload,
    reservation: Arc<SubmitSignal>,
}

#[derive(Debug)]
struct FinishedExpiration {
    task_id: String,
    finished_monotonic: Instant,
}

#[derive(Debug)]
struct RegistryState<C> {
    clock: C,
    settings: TaskRegistrySettings,
    tasks: HashMap<String, TaskRecord>,
    requests: HashMap<String, RequestRecord>,
    task_requests: HashMap<String, String>,
    finished_expirations: VecDeque<FinishedExpiration>,
}

#[derive(Debug)]
pub(crate) struct TaskRegistry<C = MonotonicClock> {
    state: Arc<Mutex<RegistryState<C>>>,
}

impl<C> Clone for TaskRegistry<C> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

#[cfg(test)]
impl Default for TaskRegistry<MonotonicClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry<MonotonicClock> {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_clock(MonotonicClock)
    }

    pub(crate) fn initialize(settings: TaskRegistrySettings) -> Self {
        Self::with_clock_and_settings(MonotonicClock, settings)
    }
}

impl<C> TaskRegistry<C>
where
    C: RegistryClock,
{
    #[cfg(test)]
    pub(crate) fn with_clock(clock: C) -> Self {
        Self::with_clock_and_settings(
            clock,
            TaskRegistrySettings::new(usize::MAX)
                .expect("시험용 Registry 상한은 0보다 커야 합니다"),
        )
    }

    pub(crate) fn with_clock_and_settings(clock: C, settings: TaskRegistrySettings) -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState {
                clock,
                settings,
                tasks: HashMap::new(),
                requests: HashMap::new(),
                task_requests: HashMap::new(),
                finished_expirations: VecDeque::new(),
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn poison_state_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.state.lock().unwrap();
            panic!("injected registry state failure");
        }));
    }

    #[cfg(test)]
    pub(crate) fn logical_task_count_for_test(&self) -> Result<usize, RegistryError> {
        let mut state = self.lock_state()?;
        state.purge_expired();
        Ok(state.requests.len())
    }

    /// clientRequestId 확인과 새 예약 등록을 같은 잠금 구간에서 결정한다.
    #[cfg(test)]
    pub(crate) fn reserve_submit(
        &self,
        request: ValidatedSubmit,
        candidate_task_id: String,
    ) -> Result<SubmitReservation<C>, RegistryError> {
        self.reserve_submit_with(request, || candidate_task_id)
    }

    pub(crate) fn reserve_submit_with<F>(
        &self,
        request: ValidatedSubmit,
        make_task_id: F,
    ) -> Result<SubmitReservation<C>, RegistryError>
    where
        F: FnOnce() -> String,
    {
        let payload = request.payload().clone();
        let client_request_id = payload.client_request_id.clone();
        let mut state = self.lock_state()?;
        state.purge_expired();

        if let Some(existing) = state.requests.get(&client_request_id) {
            if existing.payload != payload {
                return Err(RegistryError::IdempotencyConflict(client_request_id));
            }
            return Ok(SubmitReservation::Existing(SubmitWaiter {
                task_id: existing.task_id.clone(),
                signal: Arc::clone(&existing.reservation),
            }));
        }
        // requests는 예약부터 FINISHED 보존까지 논리 작업 하나를 정확히 한 번 나타낸다.
        if state.requests.len() >= state.settings.max_tasks() {
            return Err(RegistryError::CapacityExhausted);
        }
        let candidate_task_id = make_task_id();
        if state.task_requests.contains_key(&candidate_task_id) {
            return Err(RegistryError::TaskAlreadyExists(candidate_task_id));
        }

        let signal = Arc::new(SubmitSignal::new());
        state
            .task_requests
            .insert(candidate_task_id.clone(), client_request_id.clone());
        state.requests.insert(
            client_request_id.clone(),
            RequestRecord {
                task_id: candidate_task_id.clone(),
                payload,
                reservation: Arc::clone(&signal),
            },
        );
        drop(state);

        Ok(SubmitReservation::Owner(SubmitExecutionOwner {
            registry: self.clone(),
            client_request_id,
            task_id: candidate_task_id,
            request: Box::new(request),
            signal,
            resolved: false,
        }))
    }

    /// fail-stop 중에는 기존 멱등 요청만 관찰하고 새 예약은 만들지 않는다.
    pub(crate) fn existing_submit(
        &self,
        request: &ValidatedSubmit,
    ) -> Result<Option<SubmitWaiter>, RegistryError> {
        let payload = request.payload();
        let mut state = self.lock_state()?;
        state.purge_expired();
        let Some(existing) = state.requests.get(&payload.client_request_id) else {
            return Ok(None);
        };
        if existing.payload != *payload {
            return Err(RegistryError::IdempotencyConflict(
                payload.client_request_id.clone(),
            ));
        }
        Ok(Some(SubmitWaiter {
            task_id: existing.task_id.clone(),
            signal: Arc::clone(&existing.reservation),
        }))
    }

    pub(crate) fn snapshot(&self, task_id: &str) -> Result<Option<TaskPayload>, RegistryError> {
        let mut state = self.lock_state()?;
        state.purge_expired();
        Ok(state
            .tasks
            .get(task_id)
            .map(|record| record.snapshot.clone()))
    }

    pub(crate) fn snapshot_by_client_request_id(
        &self,
        client_request_id: &str,
    ) -> Result<Option<TaskPayload>, RegistryError> {
        let mut state = self.lock_state()?;
        state.purge_expired();
        let Some(task_id) = state
            .requests
            .get(client_request_id)
            .map(|request| &request.task_id)
        else {
            return Ok(None);
        };
        Ok(state
            .tasks
            .get(task_id)
            .map(|record| record.snapshot.clone()))
    }

    pub(crate) fn effective_limits_for(
        &self,
        task_id: &str,
        observation: &SubmitObservation,
    ) -> Result<Option<VerifiedEffectiveLimits>, RegistryError> {
        if !matches!(
            observation,
            SubmitObservation::Task(TaskPayload::Running { .. })
        ) {
            return Ok(None);
        }
        let mut state = self.lock_state()?;
        state.purge_expired();
        state
            .tasks
            .get(task_id)
            .and_then(|record| record.effective_limits.clone())
            .map(Some)
            .ok_or_else(|| RegistryError::VerifiedEffectiveLimitsRequired(task_id.to_owned()))
    }

    /// RUNNING 확인과 cancel trigger 기록을 같은 Registry 잠금 구간에서 수행한다.
    pub(crate) fn request_cancel(
        &self,
        task_id: &str,
    ) -> Result<CancellationWaiter, RegistryError> {
        let mut state = self.lock_state()?;
        state.purge_expired();
        let record = state
            .tasks
            .get(task_id)
            .ok_or_else(|| RegistryError::TaskNotFound(task_id.to_owned()))?;
        if matches!(record.snapshot, TaskPayload::Finished { .. }) {
            return Err(RegistryError::TaskAlreadyFinished(task_id.to_owned()));
        }
        record
            .cancellation
            .as_ref()
            .map(RunningCancellation::request_cancel)
            .ok_or(RegistryError::StateUnavailable)
    }

    fn publish_running(
        &self,
        owner: &SubmitExecutionOwner<C>,
        snapshot: TaskPayload,
        effective_limits: VerifiedEffectiveLimits,
        cancellation: RunningCancellation,
    ) -> Result<TaskPayload, RegistryError> {
        let actual_task_id = match &snapshot {
            TaskPayload::Running { task_id, .. } => task_id,
            TaskPayload::Finished { .. } => return Err(RegistryError::RunningSnapshotRequired),
        };
        verify_task_id(&owner.task_id, actual_task_id)?;

        let mut state = self.lock_state()?;
        state.purge_expired();
        state.verify_owner(owner)?;
        if state.tasks.contains_key(&owner.task_id) {
            return Err(RegistryError::TaskAlreadyExists(owner.task_id.clone()));
        }
        state.tasks.insert(
            owner.task_id.clone(),
            TaskRecord {
                snapshot: snapshot.clone(),
                effective_limits: Some(effective_limits),
                finished_monotonic: None,
                cancellation: Some(cancellation),
            },
        );
        drop(state);
        owner
            .signal
            .publish(SubmitObservation::Task(snapshot.clone()));
        Ok(snapshot)
    }

    fn finish_snapshot(
        &self,
        owner: &SubmitExecutionOwner<C>,
        snapshot: TaskPayload,
    ) -> Result<FinishedTaskPublication, RegistryError> {
        let actual_task_id = match &snapshot {
            TaskPayload::Finished { task_id, .. } => task_id,
            TaskPayload::Running { .. } => return Err(RegistryError::FinishedSnapshotRequired),
        };
        verify_task_id(&owner.task_id, actual_task_id)?;

        let mut state = self.lock_state()?;
        state.purge_expired();
        state.verify_owner(owner)?;
        let finished_monotonic = state.clock.now();
        let cancellation = match state.tasks.get_mut(&owner.task_id) {
            Some(record) if record.finished_monotonic.is_some() => {
                return Err(RegistryError::TaskAlreadyFinished(owner.task_id.clone()));
            }
            Some(record) => {
                record.snapshot = snapshot.clone();
                record.finished_monotonic = Some(finished_monotonic);
                record.cancellation.take()
            }
            None => {
                // execve 시작 실패는 RUNNING 공개 없이 정리된 FINISHED로 바로 저장한다.
                state.tasks.insert(
                    owner.task_id.clone(),
                    TaskRecord {
                        snapshot: snapshot.clone(),
                        effective_limits: None,
                        finished_monotonic: Some(finished_monotonic),
                        cancellation: None,
                    },
                );
                None
            }
        };
        state.finished_expirations.push_back(FinishedExpiration {
            task_id: owner.task_id.clone(),
            finished_monotonic,
        });
        drop(state);
        Ok(FinishedTaskPublication {
            snapshot,
            cancellation,
            submit_signal: Arc::clone(&owner.signal),
        })
    }

    /// RUNNING 공개 전 실패는 예약과 두 인덱스를 같은 잠금 구간에서 되돌린다.
    fn release_failed_owner(
        &self,
        owner: &SubmitExecutionOwner<C>,
    ) -> Result<Option<TaskPayload>, RegistryError> {
        let mut state = self.lock_state()?;
        state.purge_expired();
        state.verify_owner(owner)?;
        if let Some(record) = state.tasks.get(&owner.task_id) {
            // RUNNING 이후 실패는 저장된 상태와 멱등 재전송 결과를 함께 유지한다.
            return Ok(Some(record.snapshot.clone()));
        }

        let removed_request = state.requests.remove(&owner.client_request_id);
        let removed_client_request_id = state.task_requests.remove(&owner.task_id);
        debug_assert!(removed_request.is_some());
        debug_assert_eq!(
            removed_client_request_id.as_deref(),
            Some(owner.client_request_id.as_str())
        );
        Ok(None)
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RegistryState<C>>, RegistryError> {
        self.state
            .lock()
            .map_err(|_| RegistryError::StateUnavailable)
    }
}

impl<C> RegistryState<C>
where
    C: RegistryClock,
{
    fn verify_owner(&self, owner: &SubmitExecutionOwner<C>) -> Result<(), RegistryError> {
        let request = self
            .requests
            .get(&owner.client_request_id)
            .ok_or_else(|| RegistryError::TaskNotFound(owner.task_id.clone()))?;
        if request.task_id != owner.task_id || !Arc::ptr_eq(&request.reservation, &owner.signal) {
            return Err(RegistryError::TaskNotFound(owner.task_id.clone()));
        }
        Ok(())
    }

    fn purge_expired(&mut self) {
        let now = self.clock.now();
        while self.finished_expirations.front().is_some_and(|entry| {
            now.checked_duration_since(entry.finished_monotonic)
                .is_some_and(|elapsed| elapsed > MIN_FINISHED_RETENTION)
        }) {
            let expired = self
                .finished_expirations
                .pop_front()
                .expect("앞에서 확인한 만료 항목이 있어야 합니다");
            let Some(record) = self.tasks.remove(&expired.task_id) else {
                debug_assert!(false, "만료 큐의 작업이 Registry에 없습니다");
                continue;
            };
            debug_assert_eq!(record.finished_monotonic, Some(expired.finished_monotonic));
            let client_request_id = self.task_requests.remove(&expired.task_id);
            debug_assert!(client_request_id.is_some());
            if let Some(client_request_id) = client_request_id {
                let request = self.requests.remove(&client_request_id);
                debug_assert!(request.is_some());
            }
        }
    }
}

fn verify_task_id(expected: &str, actual: &str) -> Result<(), RegistryError> {
    if expected == actual {
        Ok(())
    } else {
        Err(RegistryError::TaskIdMismatch {
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmitFailure {
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
}

impl SubmitFailure {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubmitObservation {
    Task(TaskPayload),
    Failed(SubmitFailure),
}

#[derive(Debug)]
struct SubmitSignal {
    observation: Mutex<Option<SubmitObservation>>,
    notify: Notify,
}

impl SubmitSignal {
    fn new() -> Self {
        Self {
            observation: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    fn publish(&self, observation: SubmitObservation) {
        *self.lock_observation() = Some(observation);
        self.notify.notify_waiters();
    }

    fn current(&self) -> Option<SubmitObservation> {
        self.lock_observation().clone()
    }

    fn lock_observation(&self) -> MutexGuard<'_, Option<SubmitObservation>> {
        self.observation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// FINISHED 저장과 호출자 통지를 분리해 실행 슬롯을 먼저 반환할 수 있게 한다.
#[derive(Debug)]
#[must_use = "FINISHED 저장 뒤 실행 슬롯을 처리하고 완료 통지를 공개해야 합니다"]
pub(crate) struct FinishedTaskPublication {
    snapshot: TaskPayload,
    cancellation: Option<RunningCancellation>,
    submit_signal: Arc<SubmitSignal>,
}

impl FinishedTaskPublication {
    pub(crate) fn publish_completion(self) -> TaskPayload {
        if let Some(cancellation) = self.cancellation {
            cancellation.complete(self.snapshot.clone());
        }
        self.submit_signal
            .publish(SubmitObservation::Task(self.snapshot.clone()));
        self.snapshot
    }
}

#[derive(Debug)]
pub(crate) enum SubmitReservation<C>
where
    C: RegistryClock,
{
    Owner(SubmitExecutionOwner<C>),
    Existing(SubmitWaiter),
}

#[derive(Debug)]
pub(crate) struct SubmitExecutionOwner<C>
where
    C: RegistryClock,
{
    registry: TaskRegistry<C>,
    client_request_id: String,
    task_id: String,
    request: Box<ValidatedSubmit>,
    signal: Arc<SubmitSignal>,
    resolved: bool,
}

impl<C> SubmitExecutionOwner<C>
where
    C: RegistryClock,
{
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    pub(crate) fn request(&self) -> &ValidatedSubmit {
        self.request.as_ref()
    }

    pub(crate) fn publish_running_with_cancellation(
        &self,
        snapshot: TaskPayload,
        effective_limits: VerifiedEffectiveLimits,
        cancellation: RunningCancellation,
    ) -> Result<TaskPayload, RegistryError> {
        self.registry
            .publish_running(self, snapshot, effective_limits, cancellation)
    }

    #[cfg(test)]
    pub(crate) fn publish_running(
        &self,
        snapshot: TaskPayload,
    ) -> Result<TaskPayload, RegistryError> {
        let (_runtime, cancellation) = cancellation_channel();
        let effective_limits = self.request.budget().verified_effective_limits_for_test();
        self.publish_running_with_cancellation(snapshot, effective_limits, cancellation)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn finish(
        mut self,
        completed: crate::runner::CompletedTask,
    ) -> Result<FinishedTaskPublication, RegistryError> {
        self.finish_inner(completed.into_payload())
    }

    #[cfg(test)]
    pub(crate) fn finish_for_test(
        mut self,
        snapshot: TaskPayload,
    ) -> Result<TaskPayload, RegistryError> {
        self.finish_inner(snapshot)
            .map(FinishedTaskPublication::publish_completion)
    }

    #[cfg(test)]
    pub(crate) fn prepare_finish_for_test(
        mut self,
        snapshot: TaskPayload,
    ) -> Result<FinishedTaskPublication, RegistryError> {
        self.finish_inner(snapshot)
    }

    fn finish_inner(
        &mut self,
        snapshot: TaskPayload,
    ) -> Result<FinishedTaskPublication, RegistryError> {
        match self.registry.finish_snapshot(self, snapshot) {
            Ok(publication) => {
                self.resolved = true;
                Ok(publication)
            }
            Err(error) => {
                self.fail_inner(SubmitFailure::new(
                    ErrorCode::InternalError,
                    error.to_string(),
                ));
                Err(error)
            }
        }
    }

    pub(crate) fn fail(mut self, failure: SubmitFailure) -> SubmitObservation {
        self.fail_inner(failure)
    }

    /// RUNNING 전 거절에서 예약 제거가 확인된 경우에만 원래 오류를 반환한다.
    pub(crate) fn rollback_before_running(
        mut self,
        failure: SubmitFailure,
    ) -> Result<SubmitObservation, RegistryError> {
        match self.registry.release_failed_owner(&self) {
            Ok(None) => {
                let observation = SubmitObservation::Failed(failure);
                self.signal.publish(observation.clone());
                self.resolved = true;
                Ok(observation)
            }
            Ok(Some(_)) => {
                let error = RegistryError::StateUnavailable;
                self.signal
                    .publish(SubmitObservation::Failed(SubmitFailure::new(
                        ErrorCode::InternalError,
                        error.to_string(),
                    )));
                self.resolved = true;
                Err(error)
            }
            Err(error) => {
                self.signal
                    .publish(SubmitObservation::Failed(SubmitFailure::new(
                        ErrorCode::InternalError,
                        error.to_string(),
                    )));
                self.resolved = true;
                Err(error)
            }
        }
    }

    fn fail_inner(&mut self, failure: SubmitFailure) -> SubmitObservation {
        let failed = SubmitObservation::Failed(failure);
        // 잠금 자체를 사용할 수 없으면 waiter는 깨우되 Registry 오류는 이후 조회에서 보존한다.
        let observation = match self.registry.release_failed_owner(self) {
            Ok(Some(snapshot)) => SubmitObservation::Task(snapshot),
            Ok(None) | Err(_) => failed,
        };
        self.signal.publish(observation.clone());
        self.resolved = true;
        observation
    }
}

impl<C> Drop for SubmitExecutionOwner<C>
where
    C: RegistryClock,
{
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        self.fail_inner(SubmitFailure::new(
            ErrorCode::InternalError,
            "실행 소유자가 결과를 공개하기 전에 종료됐습니다",
        ));
    }
}

#[derive(Debug)]
pub(crate) struct SubmitWaiter {
    task_id: String,
    signal: Arc<SubmitSignal>,
}

impl SubmitWaiter {
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    pub(crate) async fn wait(self) -> SubmitObservation {
        loop {
            // 알림 등록을 먼저 해 publish와 상태 확인 사이의 missed wakeup을 막는다.
            let notified = self.signal.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(observation) = self.signal.current() {
                return observation;
            }
            // Registry와 reservation의 Mutex guard를 가진 채 await하지 않는다.
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use tokio::sync::Barrier;
    use tokio::time::{Duration as TokioDuration, timeout};

    use super::*;
    use crate::protocol::{
        CommandSpec, CpuMax, OutputLimits, ProcessResult, Request, ResourceLimits, TaskOutput,
        TaskTiming, TaskUsage, TerminationReason,
    };

    const TASK_ID: &str = "33333333-3333-3333-3333-333333333333";
    const OTHER_TASK_ID: &str = "44444444-4444-4444-4444-444444444444";
    const CLIENT_REQUEST_ID: &str = "22222222-2222-2222-2222-222222222222";
    const OTHER_CLIENT_REQUEST_ID: &str = "55555555-5555-5555-5555-555555555555";
    const REQUEST_ID: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER_REQUEST_ID: &str = "66666666-6666-6666-6666-666666666666";

    #[derive(Debug, Clone)]
    struct FakeClock {
        now: Arc<Mutex<Instant>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Arc::new(Mutex::new(Instant::now())),
            }
        }

        fn advance(&self, duration: Duration) {
            let mut now = self.now.lock().unwrap();
            *now = now.checked_add(duration).unwrap();
        }
    }

    impl RegistryClock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }
    }

    fn submit_payload() -> SubmitTaskPayload {
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
                    quota_micros: 50_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 64 * 1024 * 1024,
                pids_max: 8,
                wall_time_limit_ms: 5_000,
            },
            output: OutputLimits {
                stdout_tail_max_bytes: 1_024,
                stderr_tail_max_bytes: 1_024,
            },
        }
    }

    fn validated(payload: SubmitTaskPayload) -> ValidatedSubmit {
        ValidatedSubmit::try_from_payload(payload).unwrap()
    }

    fn running(task_id: &str) -> TaskPayload {
        TaskPayload::Running {
            task_id: task_id.to_owned(),
            submitted_at: "2026-07-20T09:00:00Z".to_owned(),
            started_at: "2026-07-20T09:00:01Z".to_owned(),
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
                submitted_at: "2026-07-20T09:00:00Z".to_owned(),
                started_at: "2026-07-20T09:00:01Z".to_owned(),
                finished_at: "2026-07-20T09:00:02Z".to_owned(),
                wall_time_ms: 1_000,
            },
            usage: TaskUsage {
                cpu_time_micros: 42,
                memory_peak_bytes: 24,
            },
            output: TaskOutput {
                stdout_tail: "done\n".to_owned(),
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

    fn registry() -> (TaskRegistry<FakeClock>, FakeClock) {
        let clock = FakeClock::new();
        (TaskRegistry::with_clock(clock.clone()), clock)
    }

    fn bounded_registry(max_tasks: usize) -> (TaskRegistry<FakeClock>, FakeClock) {
        let clock = FakeClock::new();
        let settings = TaskRegistrySettings::new(max_tasks).unwrap();
        (
            TaskRegistry::with_clock_and_settings(clock.clone(), settings),
            clock,
        )
    }

    #[test]
    fn registry_settings_require_a_positive_task_limit() {
        assert_eq!(
            TaskRegistrySettings::new(0),
            Err(TaskRegistrySettingsError::ZeroMaximum)
        );
        assert_eq!(TaskRegistrySettings::new(1).unwrap().max_tasks(), 1);
    }

    #[test]
    fn reservations_running_and_finished_each_consume_one_logical_slot() {
        let (registry, _) = bounded_registry(3);
        let reserved = reserve_owner(&registry, submit_payload(), TASK_ID);

        let mut running_payload = submit_payload();
        running_payload.client_request_id = OTHER_CLIENT_REQUEST_ID.to_owned();
        let running_owner = reserve_owner(&registry, running_payload, OTHER_TASK_ID);
        running_owner
            .publish_running(running(OTHER_TASK_ID))
            .unwrap();

        let mut finished_payload = submit_payload();
        finished_payload.client_request_id = OTHER_REQUEST_ID.to_owned();
        let finished_owner = reserve_owner(
            &registry,
            finished_payload,
            "77777777-7777-7777-7777-777777777777",
        );
        finished_owner
            .finish_for_test(finished("77777777-7777-7777-7777-777777777777"))
            .unwrap();

        assert_eq!(registry.logical_task_count_for_test(), Ok(3));
        let mut rejected = submit_payload();
        rejected.client_request_id = "88888888-8888-8888-8888-888888888888".to_owned();
        assert!(matches!(
            registry.reserve_submit_with(validated(rejected), || {
                panic!("Registry가 가득 차면 task ID를 만들면 안 됩니다")
            }),
            Err(RegistryError::CapacityExhausted)
        ));
        assert_eq!(
            RegistryError::CapacityExhausted.error_code(),
            Some(ErrorCode::CapacityExhausted)
        );
        assert_eq!(
            RegistryError::CapacityExhausted.to_string(),
            REGISTRY_CAPACITY_EXHAUSTED_MESSAGE
        );

        reserved.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
        running_owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn full_registry_keeps_idempotency_and_conflict_precedence() {
        let (registry, _) = bounded_registry(1);
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        owner.publish_running(running(TASK_ID)).unwrap();

        assert!(matches!(
            registry.reserve_submit(validated(submit_payload()), OTHER_TASK_ID.to_owned()),
            Ok(SubmitReservation::Existing(waiter)) if waiter.task_id() == TASK_ID
        ));
        let mut conflict = submit_payload();
        conflict.command.args.push("different".to_owned());
        assert!(matches!(
            registry.reserve_submit(validated(conflict), OTHER_TASK_ID.to_owned()),
            Err(RegistryError::IdempotencyConflict(client_request_id))
                if client_request_id == CLIENT_REQUEST_ID
        ));
        owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn finished_retention_does_not_release_capacity_at_exact_boundary() {
        let (registry, clock) = bounded_registry(1);
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        owner.finish_for_test(finished(TASK_ID)).unwrap();

        clock.advance(MIN_FINISHED_RETENTION);
        let mut next = submit_payload();
        next.client_request_id = OTHER_CLIENT_REQUEST_ID.to_owned();
        assert!(matches!(
            registry.reserve_submit(validated(next.clone()), OTHER_TASK_ID.to_owned()),
            Err(RegistryError::CapacityExhausted)
        ));

        clock.advance(Duration::from_nanos(1));
        let replacement = reserve_owner(&registry, next, OTHER_TASK_ID);
        assert_eq!(registry.logical_task_count_for_test(), Ok(1));
        replacement.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn rollback_reuses_registry_capacity_but_uncertain_running_does_not() {
        let (registry, _) = bounded_registry(1);
        for sequence in 0..100 {
            let task_id = format!("90000000-0000-4000-8000-{sequence:012}");
            let owner = reserve_owner(&registry, submit_payload(), &task_id);
            owner
                .rollback_before_running(SubmitFailure::new(
                    ErrorCode::InternalError,
                    "pre-running rollback",
                ))
                .unwrap();
            assert_eq!(registry.logical_task_count_for_test(), Ok(0));
        }

        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        owner.publish_running(running(TASK_ID)).unwrap();
        owner.fail(SubmitFailure::new(
            ErrorCode::InternalError,
            "cleanup uncertain",
        ));
        assert_eq!(registry.logical_task_count_for_test(), Ok(1));

        let mut next = submit_payload();
        next.client_request_id = OTHER_CLIENT_REQUEST_ID.to_owned();
        assert!(matches!(
            registry.reserve_submit(validated(next), OTHER_TASK_ID.to_owned()),
            Err(RegistryError::CapacityExhausted)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_new_requests_share_the_last_registry_slot_atomically() {
        const CALLS: usize = 12;
        let (registry, _) = bounded_registry(1);
        let start = Arc::new(Barrier::new(CALLS));
        let owners = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for sequence in 0..CALLS {
            let registry = registry.clone();
            let start = Arc::clone(&start);
            let owners = Arc::clone(&owners);
            handles.push(tokio::spawn(async move {
                let mut request = submit_payload();
                request.client_request_id = format!("a0000000-0000-4000-8000-{sequence:012}");
                start.wait().await;
                match registry.reserve_submit(
                    validated(request),
                    format!("b0000000-0000-4000-8000-{sequence:012}"),
                ) {
                    Ok(SubmitReservation::Owner(owner)) => {
                        owners.fetch_add(1, Ordering::SeqCst);
                        Ok(owner)
                    }
                    Err(RegistryError::CapacityExhausted) => Err(()),
                    _ => panic!("예상하지 못한 Registry 결정"),
                }
            }));
        }

        let mut winner = None;
        let mut rejected = 0;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(owner) => assert!(winner.replace(owner).is_none()),
                Err(()) => rejected += 1,
            }
        }
        assert_eq!(owners.load(Ordering::SeqCst), 1);
        assert_eq!(rejected, CALLS - 1);
        assert_eq!(registry.logical_task_count_for_test(), Ok(1));
        winner
            .unwrap()
            .fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
        assert_eq!(registry.logical_task_count_for_test(), Ok(0));
    }

    #[test]
    fn poisoned_mutex_is_not_reported_as_a_missing_task() {
        let registry = TaskRegistry::new();
        let state = Arc::clone(&registry.state);
        let poison = thread::spawn(move || {
            let _guard = state.lock().unwrap();
            panic!("Registry Mutex를 시험용으로 오염시킵니다");
        });
        assert!(poison.join().is_err());

        let snapshot_error = registry.snapshot(TASK_ID).unwrap_err();
        assert_eq!(snapshot_error, RegistryError::StateUnavailable);
        assert_eq!(snapshot_error.error_code(), Some(ErrorCode::InternalError));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Err(RegistryError::StateUnavailable)
        );
    }

    fn reserve_owner<C>(
        registry: &TaskRegistry<C>,
        payload: SubmitTaskPayload,
        task_id: &str,
    ) -> SubmitExecutionOwner<C>
    where
        C: RegistryClock,
    {
        match registry
            .reserve_submit(validated(payload), task_id.to_owned())
            .unwrap()
        {
            SubmitReservation::Owner(owner) => owner,
            SubmitReservation::Existing(_) => panic!("새 실행 소유자가 필요합니다"),
        }
    }

    fn existing<C>(
        registry: &TaskRegistry<C>,
        payload: SubmitTaskPayload,
        task_id: &str,
    ) -> SubmitWaiter
    where
        C: RegistryClock,
    {
        match registry
            .reserve_submit(validated(payload), task_id.to_owned())
            .unwrap()
        {
            SubmitReservation::Existing(waiter) => waiter,
            SubmitReservation::Owner(_) => panic!("기존 요청 waiter가 필요합니다"),
        }
    }

    #[test]
    fn stores_running_snapshot_for_both_identifiers() {
        let registry = TaskRegistry::new();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        let expected = running(TASK_ID);

        owner.publish_running(expected.clone()).unwrap();

        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected.clone())));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(Some(expected))
        );
        owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn restart_does_not_restore_old_tasks_or_idempotency_mapping() {
        let previous = TaskRegistry::new();
        let previous_owner = reserve_owner(&previous, submit_payload(), TASK_ID);
        previous_owner.publish_running(running(TASK_ID)).unwrap();

        let restarted = TaskRegistry::new();
        assert_eq!(restarted.snapshot(TASK_ID), Ok(None));
        assert_eq!(
            restarted.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );
        let new_owner = reserve_owner(&restarted, submit_payload(), OTHER_TASK_ID);
        assert_eq!(new_owner.task_id(), OTHER_TASK_ID);

        previous_owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
        new_owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[tokio::test]
    async fn finished_storage_does_not_wake_cancel_waiters_before_publication() {
        let (registry, _) = registry();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        let (runtime, cancellation) = cancellation_channel();
        let effective_limits = owner
            .request()
            .budget()
            .verified_effective_limits_for_test();
        owner
            .publish_running_with_cancellation(running(TASK_ID), effective_limits, cancellation)
            .unwrap();

        let first = registry.request_cancel(TASK_ID).unwrap();
        let second = registry.request_cancel(TASK_ID).unwrap();
        runtime.cancelled().await;
        assert_eq!(
            runtime.control_snapshot().first(),
            Some(crate::lifecycle::ControlTrigger::Cancelled)
        );

        let mut first_wait = Box::pin(first.wait());
        assert!(
            timeout(TokioDuration::from_millis(10), &mut first_wait)
                .await
                .is_err()
        );
        let expected = cancelled(TASK_ID);
        let publication = owner.prepare_finish_for_test(expected.clone()).unwrap();

        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected.clone())));
        assert!(
            timeout(TokioDuration::from_millis(10), &mut first_wait)
                .await
                .is_err()
        );
        publication.publish_completion();

        assert_eq!(first_wait.await, expected);
        assert_eq!(second.wait().await, expected);
        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected)));
    }

    #[tokio::test]
    async fn dropped_cancel_waiter_does_not_stop_internal_cancellation() {
        let (registry, _) = registry();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        let (runtime, cancellation) = cancellation_channel();
        let effective_limits = owner
            .request()
            .budget()
            .verified_effective_limits_for_test();
        owner
            .publish_running_with_cancellation(running(TASK_ID), effective_limits, cancellation)
            .unwrap();

        drop(registry.request_cancel(TASK_ID).unwrap());
        runtime.cancelled().await;
        let expected = cancelled(TASK_ID);
        owner.finish_for_test(expected.clone()).unwrap();
        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected)));
    }

    #[test]
    fn cancel_rejects_finished_missing_and_expired_tasks() {
        let (registry, clock) = registry();
        assert!(matches!(
            registry.request_cancel(TASK_ID),
            Err(RegistryError::TaskNotFound(task_id)) if task_id == TASK_ID
        ));

        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        owner.finish_for_test(finished(TASK_ID)).unwrap();
        assert!(matches!(
            registry.request_cancel(TASK_ID),
            Err(RegistryError::TaskAlreadyFinished(task_id)) if task_id == TASK_ID
        ));

        clock.advance(MIN_FINISHED_RETENTION + Duration::from_nanos(1));
        assert!(matches!(
            registry.request_cancel(TASK_ID),
            Err(RegistryError::TaskNotFound(task_id)) if task_id == TASK_ID
        ));
    }

    #[test]
    fn reservation_is_not_exposed_as_a_wire_state() {
        let registry = TaskRegistry::new();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);

        assert_eq!(registry.snapshot(TASK_ID), Ok(None));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );
        assert_eq!(
            owner.publish_running(finished(TASK_ID)),
            Err(RegistryError::RunningSnapshotRequired)
        );
        owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[tokio::test]
    async fn same_request_returns_existing_task_without_starting_again() {
        let registry = TaskRegistry::new();
        let runner_starts = AtomicUsize::new(0);
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        runner_starts.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            owner.request().payload().client_request_id,
            CLIENT_REQUEST_ID
        );
        assert_eq!(
            owner.request().budget().wall_timeout(),
            Duration::from_secs(5)
        );
        let expected = owner.publish_running(running(TASK_ID)).unwrap();

        let waiter = existing(&registry, submit_payload(), OTHER_TASK_ID);
        assert_eq!(waiter.task_id(), TASK_ID);
        assert_eq!(waiter.wait().await, SubmitObservation::Task(expected));
        assert_eq!(runner_starts.load(Ordering::SeqCst), 1);
        owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[tokio::test]
    async fn envelope_request_id_and_json_object_order_are_not_identity_fields() {
        let first_json = format!(
            r#"{{"protocolVersion":1,"requestId":"{REQUEST_ID}","type":"submitTask","payload":{{"clientRequestId":"{CLIENT_REQUEST_ID}","command":{{"program":"/usr/bin/true","args":[],"workingDirectory":"/tmp","environment":{{"LANG":"C.UTF-8","TZ":"UTC"}}}},"limits":{{"cpuMax":{{"quotaMicros":1,"periodMicros":1}},"memoryMaxBytes":1,"pidsMax":1,"wallTimeLimitMs":1}},"output":{{"stdoutTailMaxBytes":1,"stderrTailMaxBytes":1}}}}}}"#
        );
        let second_json = format!(
            r#"{{"protocolVersion":1,"requestId":"{OTHER_REQUEST_ID}","type":"submitTask","payload":{{"clientRequestId":"{CLIENT_REQUEST_ID}","command":{{"program":"/usr/bin/true","args":[],"workingDirectory":"/tmp","environment":{{"TZ":"UTC","LANG":"C.UTF-8"}}}},"limits":{{"cpuMax":{{"quotaMicros":1,"periodMicros":1}},"memoryMaxBytes":1,"pidsMax":1,"wallTimeLimitMs":1}},"output":{{"stdoutTailMaxBytes":1,"stderrTailMaxBytes":1}}}}}}"#
        );
        let (_, first) = ValidatedSubmit::try_from_request(
            serde_json::from_str::<Request>(&first_json).unwrap(),
        )
        .unwrap();
        let (_, second) = ValidatedSubmit::try_from_request(
            serde_json::from_str::<Request>(&second_json).unwrap(),
        )
        .unwrap();
        assert_eq!(first.payload(), second.payload());

        let registry = TaskRegistry::new();
        let owner = match registry.reserve_submit(first, TASK_ID.to_owned()).unwrap() {
            SubmitReservation::Owner(owner) => owner,
            SubmitReservation::Existing(_) => unreachable!(),
        };
        owner.publish_running(running(TASK_ID)).unwrap();
        let waiter = match registry
            .reserve_submit(second, OTHER_TASK_ID.to_owned())
            .unwrap()
        {
            SubmitReservation::Existing(waiter) => waiter,
            SubmitReservation::Owner(_) => {
                panic!("requestId는 실행 소유권을 새로 만들면 안 됩니다")
            }
        };

        assert_eq!(
            waiter.wait().await,
            SubmitObservation::Task(running(TASK_ID))
        );
        owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn each_execution_field_difference_is_an_idempotency_conflict() {
        let base = submit_payload();
        let mut cases = Vec::new();

        let mut command = base.clone();
        command.command.program = "/usr/bin/false".to_owned();
        cases.push(command);

        let mut args = base.clone();
        args.command.args.push("different".to_owned());
        cases.push(args);

        let mut directory = base.clone();
        directory.command.working_directory = "/var/tmp".to_owned();
        cases.push(directory);

        let mut environment = base.clone();
        environment
            .command
            .environment
            .insert("TZ".to_owned(), "UTC".to_owned());
        cases.push(environment);

        let mut limits = base.clone();
        limits.limits.memory_max_bytes += 1;
        cases.push(limits);

        let mut output = base.clone();
        output.output.stdout_tail_max_bytes += 1;
        cases.push(output);

        for different in cases {
            let registry = TaskRegistry::new();
            let owner = reserve_owner(&registry, base.clone(), TASK_ID);
            let expected = owner.publish_running(running(TASK_ID)).unwrap();

            let error = registry
                .reserve_submit(validated(different), OTHER_TASK_ID.to_owned())
                .unwrap_err();

            assert_eq!(
                error,
                RegistryError::IdempotencyConflict(CLIENT_REQUEST_ID.to_owned())
            );
            assert_eq!(error.error_code(), Some(ErrorCode::IdempotencyConflict));
            assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected)));
            assert_eq!(registry.snapshot(OTHER_TASK_ID), Ok(None));
            owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
        }
    }

    #[test]
    fn duplicate_task_id_does_not_overwrite_the_first_reservation() {
        let registry = TaskRegistry::new();
        let first = reserve_owner(&registry, submit_payload(), TASK_ID);
        let mut other = submit_payload();
        other.client_request_id = OTHER_CLIENT_REQUEST_ID.to_owned();

        assert_eq!(
            registry
                .reserve_submit(validated(other), TASK_ID.to_owned())
                .unwrap_err(),
            RegistryError::TaskAlreadyExists(TASK_ID.to_owned())
        );
        assert_eq!(
            first.publish_running(running(TASK_ID)).unwrap(),
            running(TASK_ID)
        );
        assert_eq!(
            registry.snapshot_by_client_request_id(OTHER_CLIENT_REQUEST_ID),
            Ok(None)
        );
        first.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn finished_task_id_must_match_the_owner_reservation() {
        let registry = TaskRegistry::new();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);

        assert_eq!(
            owner.finish_for_test(finished(OTHER_TASK_ID)),
            Err(RegistryError::TaskIdMismatch {
                expected: TASK_ID.to_owned(),
                actual: OTHER_TASK_ID.to_owned(),
            })
        );
        assert_eq!(registry.snapshot(TASK_ID), Ok(None));
        assert_eq!(registry.snapshot(OTHER_TASK_ID), Ok(None));
    }

    #[test]
    fn validation_failure_does_not_create_an_idempotency_mapping() {
        let registry = TaskRegistry::new();
        let mut invalid = submit_payload();
        invalid.limits.wall_time_limit_ms = 0;

        assert!(ValidatedSubmit::try_from_payload(invalid).is_err());
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        assert_eq!(owner.task_id(), TASK_ID);
        owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn finished_snapshot_is_immutable_and_can_start_without_running() {
        let registry = TaskRegistry::new();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        let expected = finished(TASK_ID);
        owner.finish_for_test(expected.clone()).unwrap();

        let mut caller_copy = registry.snapshot(TASK_ID).unwrap().unwrap();
        match &mut caller_copy {
            TaskPayload::Finished { output, .. } => output.stdout_tail.push_str("changed"),
            TaskPayload::Running { .. } => panic!("FINISHED snapshot이 필요합니다"),
        }
        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected)));
    }

    #[test]
    fn finished_result_is_available_through_the_ten_minute_boundary() {
        let (registry, clock) = registry();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        let expected = owner.finish_for_test(finished(TASK_ID)).unwrap();

        clock.advance(MIN_FINISHED_RETENTION - Duration::from_nanos(1));
        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected.clone())));

        clock.advance(Duration::from_nanos(1));
        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(expected.clone())));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(Some(expected))
        );
    }

    #[test]
    fn expiration_removes_snapshot_mapping_and_comparison_payload_together() {
        let (registry, clock) = registry();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        owner.finish_for_test(finished(TASK_ID)).unwrap();

        clock.advance(MIN_FINISHED_RETENTION + Duration::from_nanos(1));

        assert_eq!(registry.snapshot(TASK_ID), Ok(None));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );
        let mut changed = submit_payload();
        changed.command.program = "/usr/bin/false".to_owned();
        let replacement = reserve_owner(&registry, changed, OTHER_TASK_ID);
        assert_eq!(replacement.task_id(), OTHER_TASK_ID);
        replacement.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[test]
    fn expiration_queue_removes_only_finished_entries_past_retention() {
        let (registry, clock) = registry();
        let first = reserve_owner(&registry, submit_payload(), TASK_ID);
        first.finish_for_test(finished(TASK_ID)).unwrap();

        clock.advance(Duration::from_secs(5 * 60));
        let mut other_payload = submit_payload();
        other_payload.client_request_id = OTHER_CLIENT_REQUEST_ID.to_owned();
        let second = reserve_owner(&registry, other_payload, OTHER_TASK_ID);
        second.finish_for_test(finished(OTHER_TASK_ID)).unwrap();

        clock.advance(Duration::from_secs(5 * 60) + Duration::from_nanos(1));

        assert_eq!(registry.snapshot(TASK_ID), Ok(None));
        assert_eq!(
            registry.snapshot(OTHER_TASK_ID),
            Ok(Some(finished(OTHER_TASK_ID)))
        );
    }

    #[test]
    fn running_task_is_not_removed_by_finished_retention() {
        let (registry, clock) = registry();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        owner.publish_running(running(TASK_ID)).unwrap();

        clock.advance(Duration::from_secs(24 * 60 * 60));

        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(running(TASK_ID))));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(Some(running(TASK_ID)))
        );
        owner.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_identical_requests_have_exactly_one_execution_owner() {
        const CALLS: usize = 12;
        let registry = TaskRegistry::new();
        let start = Arc::new(Barrier::new(CALLS));
        let decisions = Arc::new(AtomicUsize::new(0));
        let runner_starts = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for index in 0..CALLS {
            let registry = registry.clone();
            let start = Arc::clone(&start);
            let decisions = Arc::clone(&decisions);
            let runner_starts = Arc::clone(&runner_starts);
            handles.push(tokio::spawn(async move {
                start.wait().await;
                let candidate = format!("33333333-3333-3333-3333-{index:012}");
                let reservation = registry
                    .reserve_submit(validated(submit_payload()), candidate)
                    .unwrap();
                decisions.fetch_add(1, Ordering::SeqCst);
                match reservation {
                    SubmitReservation::Owner(owner) => {
                        runner_starts.fetch_add(1, Ordering::SeqCst);
                        while decisions.load(Ordering::SeqCst) < CALLS {
                            tokio::task::yield_now().await;
                        }
                        let snapshot = owner.publish_running(running(owner.task_id())).unwrap();
                        (SubmitObservation::Task(snapshot), Some(owner))
                    }
                    SubmitReservation::Existing(waiter) => {
                        let observation = timeout(TokioDuration::from_secs(2), waiter.wait())
                            .await
                            .expect("동일 요청 waiter가 끝나야 합니다");
                        (observation, None)
                    }
                }
            }));
        }

        let mut observations = Vec::new();
        let mut owner = None;
        for handle in handles {
            let (observation, candidate_owner) = handle.await.unwrap();
            observations.push(observation);
            if let Some(candidate_owner) = candidate_owner {
                assert!(owner.replace(candidate_owner).is_none());
            }
        }

        assert_eq!(runner_starts.load(Ordering::SeqCst), 1);
        assert!(observations.windows(2).all(|pair| pair[0] == pair[1]));
        let task_ids: Vec<_> = observations
            .iter()
            .map(|observation| match observation {
                SubmitObservation::Task(TaskPayload::Running { task_id, .. }) => task_id,
                _ => panic!("RUNNING 공개 상태가 필요합니다"),
            })
            .collect();
        assert!(task_ids.windows(2).all(|pair| pair[0] == pair[1]));
        owner
            .unwrap()
            .fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_conflict_has_zero_runner_cgroup_and_process_side_effects() {
        let registry = TaskRegistry::new();
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
            let start = Arc::clone(&start);
            let counters = Arc::clone(&counters);
            handles.push(tokio::spawn(async move {
                let mut payload = submit_payload();
                if index == 1 {
                    payload.command.args.push("conflict".to_owned());
                }
                start.wait().await;
                let result = registry.reserve_submit(
                    validated(payload),
                    format!("44444444-4444-4444-4444-{index:012}"),
                );
                match result {
                    Ok(SubmitReservation::Owner(owner)) => {
                        counters[index][0].fetch_add(1, Ordering::SeqCst);
                        counters[index][1].fetch_add(1, Ordering::SeqCst);
                        counters[index][2].fetch_add(1, Ordering::SeqCst);
                        owner.publish_running(running(owner.task_id())).unwrap();
                        Ok(owner)
                    }
                    Err(error @ RegistryError::IdempotencyConflict(_)) => Err(error),
                    Ok(SubmitReservation::Existing(_)) => panic!("서로 다른 payload입니다"),
                    Err(error) => panic!("예상하지 못한 Registry 오류: {error}"),
                }
            }));
        }

        let mut owner = None;
        let mut conflict_index = None;
        for (index, handle) in handles.into_iter().enumerate() {
            match handle.await.unwrap() {
                Ok(found_owner) => owner = Some(found_owner),
                Err(error) => {
                    assert_eq!(error.error_code(), Some(ErrorCode::IdempotencyConflict));
                    conflict_index = Some(index);
                }
            }
        }

        let conflict_index = conflict_index.expect("한 요청은 충돌해야 합니다");
        assert_eq!(counters[conflict_index][0].load(Ordering::SeqCst), 0);
        assert_eq!(counters[conflict_index][1].load(Ordering::SeqCst), 0);
        assert_eq!(counters[conflict_index][2].load(Ordering::SeqCst), 0);
        for side_effect in 0..3 {
            assert_eq!(
                counters
                    .iter()
                    .map(|counter| counter[side_effect].load(Ordering::SeqCst))
                    .sum::<usize>(),
                1
            );
        }
        owner
            .unwrap()
            .fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn owner_failure_wakes_all_waiters() {
        const CALLS: usize = 8;
        let registry = TaskRegistry::new();
        let start = Arc::new(Barrier::new(CALLS));
        let decisions = Arc::new(AtomicUsize::new(0));
        let runner_starts = Arc::new(AtomicUsize::new(0));
        let expected = SubmitFailure::new(ErrorCode::InternalError, "runner start failed");
        let mut handles = Vec::new();

        for index in 0..CALLS {
            let registry = registry.clone();
            let start = Arc::clone(&start);
            let decisions = Arc::clone(&decisions);
            let runner_starts = Arc::clone(&runner_starts);
            let expected = expected.clone();
            handles.push(tokio::spawn(async move {
                start.wait().await;
                let reservation = registry
                    .reserve_submit(
                        validated(submit_payload()),
                        format!("55555555-5555-5555-5555-{index:012}"),
                    )
                    .unwrap();
                decisions.fetch_add(1, Ordering::SeqCst);
                match reservation {
                    SubmitReservation::Owner(owner) => {
                        runner_starts.fetch_add(1, Ordering::SeqCst);
                        while decisions.load(Ordering::SeqCst) < CALLS {
                            tokio::task::yield_now().await;
                        }
                        owner.fail(expected)
                    }
                    SubmitReservation::Existing(waiter) => {
                        timeout(TokioDuration::from_secs(2), waiter.wait())
                            .await
                            .expect("owner 실패 뒤 waiter가 끝나야 합니다")
                    }
                }
            }));
        }

        let mut observations = Vec::new();
        for handle in handles {
            observations.push(handle.await.unwrap());
        }
        assert_eq!(runner_starts.load(Ordering::SeqCst), 1);
        assert!(
            observations
                .iter()
                .all(|observation| observation == &SubmitObservation::Failed(expected.clone()))
        );
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Ok(None)
        );
        let retry = reserve_owner(&registry, submit_payload(), OTHER_TASK_ID);
        assert_eq!(retry.task_id(), OTHER_TASK_ID);
        retry.fail(SubmitFailure::new(ErrorCode::InternalError, "test done"));
    }

    #[tokio::test]
    async fn failure_after_running_preserves_public_snapshot_for_idempotent_retries() {
        let registry = TaskRegistry::new();
        let owner = reserve_owner(&registry, submit_payload(), TASK_ID);
        let running = owner.publish_running(running(TASK_ID)).unwrap();
        let expected = SubmitFailure::new(ErrorCode::InternalError, "cleanup uncertain");

        assert_eq!(
            owner.fail(expected),
            SubmitObservation::Task(running.clone())
        );
        assert_eq!(registry.snapshot(TASK_ID), Ok(Some(running.clone())));

        let waiter = existing(&registry, submit_payload(), OTHER_TASK_ID);
        assert_eq!(waiter.wait().await, SubmitObservation::Task(running));
    }
}
