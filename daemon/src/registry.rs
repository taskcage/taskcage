//! 실행 중인 작업과 완료 결과를 daemon 수명 동안 메모리에 보관한다.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::protocol::TaskPayload;

pub(crate) const MIN_FINISHED_RETENTION: Duration = Duration::from_secs(10 * 60);

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

#[derive(Debug)]
struct FinishedExpiration {
    task_id: String,
    finished_monotonic: Instant,
}

#[derive(Debug)]
pub(crate) struct TaskRegistry<C = MonotonicClock> {
    clock: C,
    tasks: HashMap<String, TaskRecord>,
    client_tasks: HashMap<String, String>,
    finished_expirations: VecDeque<FinishedExpiration>,
}

impl Default for TaskRegistry<MonotonicClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry<MonotonicClock> {
    pub(crate) fn new() -> Self {
        Self::with_clock(MonotonicClock)
    }
}

impl<C> TaskRegistry<C>
where
    C: RegistryClock,
{
    pub(crate) fn with_clock(clock: C) -> Self {
        Self {
            clock,
            tasks: HashMap::new(),
            client_tasks: HashMap::new(),
            finished_expirations: VecDeque::new(),
        }
    }

    pub(crate) fn insert_running(
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

    #[cfg(target_os = "linux")]
    pub(crate) fn finish(
        &mut self,
        completed: crate::runner::CompletedTask,
    ) -> Result<(), RegistryError> {
        self.finish_snapshot(completed.into_payload())
    }

    fn finish_snapshot(&mut self, snapshot: TaskPayload) -> Result<(), RegistryError> {
        self.purge_expired();

        let task_id = match &snapshot {
            TaskPayload::Finished { task_id, .. } => task_id.clone(),
            TaskPayload::Running { .. } => unreachable!("완료 토큰은 FINISHED만 포함합니다"),
        };
        let finished_monotonic = self.clock.now();
        let record = self
            .tasks
            .get_mut(&task_id)
            .ok_or_else(|| RegistryError::TaskNotFound(task_id.clone()))?;
        if record.finished_monotonic.is_some() {
            return Err(RegistryError::TaskAlreadyFinished(task_id));
        }

        record.snapshot = snapshot;
        record.finished_monotonic = Some(finished_monotonic);
        self.finished_expirations.push_back(FinishedExpiration {
            task_id,
            finished_monotonic,
        });
        Ok(())
    }

    #[cfg(test)]
    fn finish_for_test(&mut self, snapshot: TaskPayload) -> Result<(), RegistryError> {
        self.finish_snapshot(snapshot)
    }

    pub(crate) fn snapshot(&mut self, task_id: &str) -> Option<TaskPayload> {
        self.purge_expired();
        self.tasks
            .get(task_id)
            .map(|record| record.snapshot.clone())
    }

    pub(crate) fn snapshot_by_client_request_id(
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
            let mapped_task = self.client_tasks.remove(&record.client_request_id);
            debug_assert_eq!(mapped_task.as_deref(), Some(expired.task_id.as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    #[cfg(target_os = "linux")]
    use std::collections::BTreeMap;
    #[cfg(target_os = "linux")]
    use std::fs;

    #[cfg(target_os = "linux")]
    use crate::protocol::{CommandSpec, CpuMax, OutputLimits, ResourceLimits};
    use crate::protocol::{ProcessResult, TaskOutput, TaskTiming, TaskUsage, TerminationReason};
    #[cfg(target_os = "linux")]
    use crate::resource_budget::ResourceBudget;
    #[cfg(target_os = "linux")]
    use crate::{TaskRunConfig, TaskRunner, preflight::CapabilityProbe, preflight::SystemProbe};

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

        registry.finish_for_test(expected.clone()).unwrap();

        let mut caller_copy = registry.snapshot(TASK_ID).unwrap();
        match &mut caller_copy {
            TaskPayload::Finished { output, .. } => output.stdout_tail.push_str("changed"),
            TaskPayload::Running { .. } => panic!("FINISHED snapshot이 필요합니다"),
        }
        assert_eq!(registry.snapshot(TASK_ID), Some(expected.clone()));
        assert_eq!(
            registry.finish_for_test(finished(TASK_ID)),
            Err(RegistryError::TaskAlreadyFinished(TASK_ID.to_owned()))
        );
        assert_eq!(registry.snapshot(TASK_ID), Some(expected));
    }

    #[test]
    fn unknown_finished_task_does_not_change_the_running_snapshot() {
        let (mut registry, _) = registry();
        insert_default(&mut registry);
        let expected = running(TASK_ID);

        assert_eq!(
            registry.finish_for_test(finished(OTHER_TASK_ID)),
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
        registry.finish_for_test(expected.clone()).unwrap();

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
        registry.finish_for_test(finished(TASK_ID)).unwrap();

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
    fn expiration_queue_removes_only_finished_entries_past_retention() {
        let (mut registry, clock) = registry();
        insert_default(&mut registry);
        registry.finish_for_test(finished(TASK_ID)).unwrap();

        clock.advance(Duration::from_secs(5 * 60));
        registry
            .insert_running(OTHER_CLIENT_REQUEST_ID.to_owned(), running(OTHER_TASK_ID))
            .unwrap();
        registry.finish_for_test(finished(OTHER_TASK_ID)).unwrap();

        clock.advance(Duration::from_secs(5 * 60) + Duration::from_nanos(1));

        assert_eq!(registry.snapshot(TASK_ID), None);
        assert_eq!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            None
        );
        assert_eq!(
            registry.snapshot(OTHER_TASK_ID),
            Some(finished(OTHER_TASK_ID))
        );
        assert_eq!(
            registry.snapshot_by_client_request_id(OTHER_CLIENT_REQUEST_ID),
            Some(finished(OTHER_TASK_ID))
        );
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_runner_completion_is_recorded_only_after_cleanup() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_REGISTRY_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let runner = TaskRunner::initialize(environment).unwrap();
        let started = Instant::now();
        let budget = ResourceBudget::try_from_protocol(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 50_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 64 * 1024 * 1024,
                pids_max: 8,
                wall_time_limit_ms: 5_000,
            },
            OutputLimits {
                stdout_tail_max_bytes: 1_024,
                stderr_tail_max_bytes: 1_024,
            },
        )
        .unwrap();
        let config = TaskRunConfig {
            task_id: TASK_ID.to_owned(),
            submitted_at: "2026-07-24T10:00:00.000Z".to_owned(),
            started_at: "2026-07-24T10:00:00.010Z".to_owned(),
            started_monotonic: started,
            cleanup_timeout: Duration::from_secs(5),
            command: CommandSpec {
                program: "/bin/sleep".to_owned(),
                args: vec!["0.1".to_owned()],
                working_directory: "/".to_owned(),
                environment: BTreeMap::new(),
            },
            budget,
        };
        let (running_sender, mut running_receiver) = tokio::sync::oneshot::channel();
        let run = runner.run_task(config, running_sender, || {
            (
                "2026-07-24T10:00:01.000Z".to_owned(),
                started + Duration::from_secs(1),
            )
        });
        tokio::pin!(run);

        let running_snapshot = tokio::select! {
            biased;
            running = &mut running_receiver => running.unwrap(),
            result = &mut run => panic!("RUNNING 등록 전에 실행이 끝났습니다: {result:?}"),
        };
        let mut registry = TaskRegistry::new();
        registry
            .insert_running(CLIENT_REQUEST_ID.to_owned(), running_snapshot)
            .unwrap();

        let completed = run.await.unwrap();
        assert!(matches!(completed.payload(), TaskPayload::Finished { .. }));
        registry.finish(completed).unwrap();
        assert!(matches!(
            registry.snapshot(TASK_ID),
            Some(TaskPayload::Finished { .. })
        ));
        assert!(matches!(
            registry.snapshot_by_client_request_id(CLIENT_REQUEST_ID),
            Some(TaskPayload::Finished { .. })
        ));

        let remaining_jobs = fs::read_dir(jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(remaining_jobs, 0, "작업 cgroup이 남아 있습니다");
    }
}
