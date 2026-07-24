//! 실행 중인 작업과 완료 결과를 daemon 수명 동안 메모리에 보관한다.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::protocol::TaskPayload;

pub const MIN_FINISHED_RETENTION: Duration = Duration::from_secs(10 * 60);

pub trait RegistryClock {
    fn now(&self) -> Instant;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MonotonicClock;

impl RegistryClock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("새 작업은 RUNNING snapshot으로 등록해야 합니다")]
    RunningSnapshotRequired,
    #[error("작업 완료에는 FINISHED snapshot이 필요합니다")]
    FinishedSnapshotRequired,
    #[error("taskId가 이미 등록되어 있습니다: {0}")]
    TaskAlreadyExists(String),
    #[error("clientRequestId가 이미 다른 작업에 연결되어 있습니다: {0}")]
    ClientRequestAlreadyMapped(String),
    #[error("작업을 찾을 수 없습니다: {0}")]
    TaskNotFound(String),
    #[error("완료된 작업 결과는 바꿀 수 없습니다: {0}")]
    TaskAlreadyFinished(String),
}

#[derive(Debug)]
struct TaskRecord {
    client_request_id: String,
    snapshot: TaskPayload,
    finished_monotonic: Option<Instant>,
}

impl TaskRecord {
    fn is_expired(&self, now: Instant) -> bool {
        self.finished_monotonic
            .and_then(|finished| now.checked_duration_since(finished))
            .is_some_and(|elapsed| elapsed > MIN_FINISHED_RETENTION)
    }
}

#[derive(Debug)]
pub struct TaskRegistry<C = MonotonicClock> {
    clock: C,
    tasks: HashMap<String, TaskRecord>,
    client_tasks: HashMap<String, String>,
}

impl Default for TaskRegistry<MonotonicClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry<MonotonicClock> {
    pub fn new() -> Self {
        Self::with_clock(MonotonicClock)
    }
}

impl<C> TaskRegistry<C>
where
    C: RegistryClock,
{
    pub fn with_clock(clock: C) -> Self {
        Self {
            clock,
            tasks: HashMap::new(),
            client_tasks: HashMap::new(),
        }
    }

    pub fn insert_running(
        &mut self,
        client_request_id: String,
        snapshot: TaskPayload,
    ) -> Result<(), RegistryError> {
        self.purge_expired();

        let task_id = match &snapshot {
            TaskPayload::Running { task_id, .. } => task_id.clone(),
            TaskPayload::Finished { .. } => return Err(RegistryError::RunningSnapshotRequired),
        };

        if self.tasks.contains_key(&task_id) {
            return Err(RegistryError::TaskAlreadyExists(task_id));
        }
        if self.client_tasks.contains_key(&client_request_id) {
            return Err(RegistryError::ClientRequestAlreadyMapped(client_request_id));
        }

        self.client_tasks
            .insert(client_request_id.clone(), task_id.clone());
        self.tasks.insert(
            task_id,
            TaskRecord {
                client_request_id,
                snapshot,
                finished_monotonic: None,
            },
        );
        Ok(())
    }

    pub fn finish(&mut self, snapshot: TaskPayload) -> Result<(), RegistryError> {
        self.purge_expired();

        let task_id = match &snapshot {
            TaskPayload::Finished { task_id, .. } => task_id.clone(),
            TaskPayload::Running { .. } => return Err(RegistryError::FinishedSnapshotRequired),
        };
        let record = self
            .tasks
            .get_mut(&task_id)
            .ok_or_else(|| RegistryError::TaskNotFound(task_id.clone()))?;
        if record.finished_monotonic.is_some() {
            return Err(RegistryError::TaskAlreadyFinished(task_id));
        }

        record.snapshot = snapshot;
        record.finished_monotonic = Some(self.clock.now());
        Ok(())
    }

    pub fn snapshot(&mut self, task_id: &str) -> Option<TaskPayload> {
        self.purge_expired();
        self.tasks
            .get(task_id)
            .map(|record| record.snapshot.clone())
    }

    pub fn snapshot_by_client_request_id(
        &mut self,
        client_request_id: &str,
    ) -> Option<TaskPayload> {
        self.purge_expired();
        let task_id = self.client_tasks.get(client_request_id)?;
        self.tasks
            .get(task_id)
            .map(|record| record.snapshot.clone())
    }

    fn purge_expired(&mut self) {
        let now = self.clock.now();
        let tasks = &mut self.tasks;
        let client_tasks = &mut self.client_tasks;
        tasks.retain(|task_id, record| {
            if !record.is_expired(now) {
                return true;
            }

            let mapped_task = client_tasks.remove(&record.client_request_id);
            debug_assert_eq!(mapped_task.as_deref(), Some(task_id.as_str()));
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::protocol::{ProcessResult, TaskOutput, TaskTiming, TaskUsage, TerminationReason};

    use super::*;

    const TASK_ID: &str = "33333333-3333-3333-3333-333333333333";
    const OTHER_TASK_ID: &str = "44444444-4444-4444-4444-444444444444";
    const CLIENT_REQUEST_ID: &str = "22222222-2222-2222-2222-222222222222";
    const OTHER_CLIENT_REQUEST_ID: &str = "55555555-5555-5555-5555-555555555555";

    #[derive(Clone)]
    struct FakeClock {
        now: Rc<Cell<Instant>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Rc::new(Cell::new(Instant::now())),
            }
        }

        fn advance(&self, duration: Duration) {
            self.now.set(self.now.get().checked_add(duration).unwrap());
        }
    }

    impl RegistryClock for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }
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

    fn registry() -> (TaskRegistry<FakeClock>, FakeClock) {
        let clock = FakeClock::new();
        (TaskRegistry::with_clock(clock.clone()), clock)
    }

    fn insert_default(registry: &mut TaskRegistry<FakeClock>) {
        registry
            .insert_running(CLIENT_REQUEST_ID.to_owned(), running(TASK_ID))
            .unwrap();
    }

    #[test]
    fn stores_running_snapshot_for_both_identifiers() {
        let (mut registry, _) = registry();
        let expected = running(TASK_ID);

        registry
            .insert_running(CLIENT_REQUEST_ID.to_owned(), expected.clone())
            .unwrap();

        assert_eq!(registry.snapshot(TASK_ID), Some(expected.clone()));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Some(expected)
        );
    }

    #[test]
    fn only_running_snapshots_can_create_records() {
        let (mut registry, _) = registry();

        assert_eq!(
            registry.insert_running(CLIENT_REQUEST_ID.to_owned(), finished(TASK_ID)),
            Err(RegistryError::RunningSnapshotRequired)
        );
        assert_eq!(registry.snapshot(TASK_ID), None);
    }

    #[test]
    fn finished_snapshot_is_immutable_after_one_transition() {
        let (mut registry, _) = registry();
        insert_default(&mut registry);
        let expected = finished(TASK_ID);

        registry.finish(expected.clone()).unwrap();

        let mut caller_copy = registry.snapshot(TASK_ID).unwrap();
        match &mut caller_copy {
            TaskPayload::Finished { output, .. } => output.stdout_tail.push_str("changed"),
            TaskPayload::Running { .. } => panic!("FINISHED snapshot이 필요합니다"),
        }
        assert_eq!(registry.snapshot(TASK_ID), Some(expected.clone()));
        assert_eq!(
            registry.finish(finished(TASK_ID)),
            Err(RegistryError::TaskAlreadyFinished(TASK_ID.to_owned()))
        );
        assert_eq!(registry.snapshot(TASK_ID), Some(expected));
    }

    #[test]
    fn invalid_transition_does_not_change_the_running_snapshot() {
        let (mut registry, _) = registry();
        insert_default(&mut registry);
        let expected = running(TASK_ID);

        assert_eq!(
            registry.finish(running(TASK_ID)),
            Err(RegistryError::FinishedSnapshotRequired)
        );
        assert_eq!(
            registry.finish(finished(OTHER_TASK_ID)),
            Err(RegistryError::TaskNotFound(OTHER_TASK_ID.to_owned()))
        );
        assert_eq!(registry.snapshot(TASK_ID), Some(expected));
    }

    #[test]
    fn duplicate_identifiers_do_not_overwrite_existing_mappings() {
        let (mut registry, _) = registry();
        insert_default(&mut registry);

        assert_eq!(
            registry.insert_running(OTHER_CLIENT_REQUEST_ID.to_owned(), running(TASK_ID)),
            Err(RegistryError::TaskAlreadyExists(TASK_ID.to_owned()))
        );
        assert_eq!(
            registry.insert_running(CLIENT_REQUEST_ID.to_owned(), running(OTHER_TASK_ID)),
            Err(RegistryError::ClientRequestAlreadyMapped(
                CLIENT_REQUEST_ID.to_owned()
            ))
        );
        assert_eq!(registry.snapshot(OTHER_TASK_ID), None);
        assert_eq!(
            registry.snapshot_by_client_request_id(OTHER_CLIENT_REQUEST_ID),
            None
        );
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Some(running(TASK_ID))
        );
    }

    #[test]
    fn finished_result_is_available_through_the_ten_minute_boundary() {
        let (mut registry, clock) = registry();
        insert_default(&mut registry);
        let expected = finished(TASK_ID);
        registry.finish(expected.clone()).unwrap();

        clock.advance(MIN_FINISHED_RETENTION - Duration::from_nanos(1));
        assert_eq!(registry.snapshot(TASK_ID), Some(expected.clone()));

        clock.advance(Duration::from_nanos(1));
        assert_eq!(registry.snapshot(TASK_ID), Some(expected.clone()));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Some(expected)
        );
    }

    #[test]
    fn finished_result_and_client_mapping_can_expire_after_ten_minutes() {
        let (mut registry, clock) = registry();
        insert_default(&mut registry);
        registry.finish(finished(TASK_ID)).unwrap();

        clock.advance(MIN_FINISHED_RETENTION + Duration::from_nanos(1));

        assert_eq!(registry.snapshot(TASK_ID), None);
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            None
        );
        registry
            .insert_running(CLIENT_REQUEST_ID.to_owned(), running(TASK_ID))
            .unwrap();
        assert_eq!(registry.snapshot(TASK_ID), Some(running(TASK_ID)));
    }

    #[test]
    fn running_task_is_not_removed_by_finished_retention() {
        let (mut registry, clock) = registry();
        insert_default(&mut registry);

        clock.advance(Duration::from_secs(24 * 60 * 60));

        assert_eq!(registry.snapshot(TASK_ID), Some(running(TASK_ID)));
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Some(running(TASK_ID))
        );
    }
}
