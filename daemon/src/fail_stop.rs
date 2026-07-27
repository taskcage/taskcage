//! 정리 불확실성을 신규 실행 차단과 process-wide 종료로 연결한다.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Notify;

use crate::deadline::MonotonicDeadline;

type Clock = dyn Fn() -> Instant + Send + Sync + 'static;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FailStopSettings {
    timeout: Duration,
}

impl FailStopSettings {
    pub(crate) fn new(timeout: Duration) -> Result<Self, FailStopSettingsError> {
        if timeout.is_zero() {
            return Err(FailStopSettingsError::ZeroTimeout);
        }
        if MonotonicDeadline::from_now(timeout).is_none() {
            return Err(FailStopSettingsError::UnrepresentableTimeout);
        }
        Ok(Self { timeout })
    }

    pub(crate) fn timeout(self) -> Duration {
        self.timeout
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailStopSettingsError {
    #[error("fail-stop timeout은 0보다 커야 합니다")]
    ZeroTimeout,
    #[error("fail-stop timeout을 단조시간 deadline으로 표현할 수 없습니다")]
    UnrepresentableTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanupFailureReport {
    pub(crate) task_id: String,
    pub(crate) stage: &'static str,
    pub(crate) uncleaned: Vec<&'static str>,
    pub(crate) retry: &'static str,
}

impl CleanupFailureReport {
    pub(crate) fn new(
        task_id: impl Into<String>,
        stage: &'static str,
        uncleaned: Vec<&'static str>,
        retry: &'static str,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            stage,
            uncleaned,
            retry,
        }
    }
}

#[derive(Debug)]
enum Phase {
    Healthy,
    FailStopping {
        deadline: MonotonicDeadline,
        first_report: CleanupFailureReport,
    },
}

#[derive(Debug)]
struct State {
    phase: Phase,
    active: HashSet<String>,
}

pub(crate) struct FailStopCoordinator {
    settings: FailStopSettings,
    state: Mutex<State>,
    activated: Notify,
    active_changed: Notify,
    now: Arc<Clock>,
}

impl fmt::Debug for FailStopCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FailStopCoordinator")
            .field("settings", &self.settings)
            .field("state", &self.lock_state())
            .finish_non_exhaustive()
    }
}

impl FailStopCoordinator {
    pub(crate) fn new(settings: FailStopSettings) -> Arc<Self> {
        Self::with_clock(settings, Arc::new(Instant::now))
    }

    fn with_clock(settings: FailStopSettings, now: Arc<Clock>) -> Arc<Self> {
        Arc::new(Self {
            settings,
            state: Mutex::new(State {
                phase: Phase::Healthy,
                active: HashSet::new(),
            }),
            activated: Notify::new(),
            active_changed: Notify::new(),
            now,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_clock(settings: FailStopSettings, now: Arc<Clock>) -> Arc<Self> {
        Self::with_clock(settings, now)
    }

    pub(crate) fn try_admit(self: &Arc<Self>) -> Option<FailStopAdmission<'_>> {
        let state = self.lock_state();
        if matches!(state.phase, Phase::Healthy) {
            Some(FailStopAdmission {
                coordinator: self,
                state,
            })
        } else {
            None
        }
    }

    pub(crate) fn activate(&self, report: CleanupFailureReport) -> MonotonicDeadline {
        let mut state = self.lock_state();
        if let Phase::FailStopping { deadline, .. } = state.phase {
            return deadline;
        }

        let now = (self.now)();
        let deadline = MonotonicDeadline::from_start(now, self.settings.timeout())
            // 설정 생성 때 검증했지만 플랫폼 Instant 범위 끝에 도달했다면 즉시 종료한다.
            .unwrap_or_else(|| MonotonicDeadline::expired_at(now));
        tracing::error!(
            task_id = %report.task_id,
            stage = report.stage,
            uncleaned = ?report.uncleaned,
            retry = report.retry,
            "작업 정리를 증명하지 못해 process-wide fail-stop을 시작합니다"
        );
        state.phase = Phase::FailStopping {
            deadline,
            first_report: report,
        };
        drop(state);
        self.activated.notify_waiters();
        deadline
    }

    pub(crate) fn is_fail_stopping(&self) -> bool {
        matches!(self.lock_state().phase, Phase::FailStopping { .. })
    }

    pub(crate) fn deadline(&self) -> Option<MonotonicDeadline> {
        match &self.lock_state().phase {
            Phase::Healthy => None,
            Phase::FailStopping { deadline, .. } => Some(*deadline),
        }
    }

    pub(crate) async fn activated(&self) -> MonotonicDeadline {
        loop {
            let notified = self.activated.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(deadline) = self.deadline() {
                return deadline;
            }
            notified.await;
        }
    }

    pub(crate) fn active_count(&self) -> usize {
        self.lock_state().active.len()
    }

    pub(crate) async fn active_changed(&self) {
        self.active_changed.notified().await;
    }

    pub(crate) fn first_report(&self) -> Option<CleanupFailureReport> {
        match &self.lock_state().phase {
            Phase::Healthy => None,
            Phase::FailStopping { first_report, .. } => Some(first_report.clone()),
        }
    }

    fn complete(&self, task_id: &str) {
        let removed = self.lock_state().active.remove(task_id);
        debug_assert!(removed, "완료할 활성 실행 소유권이 있어야 합니다");
        if removed {
            // 대기자가 등록되기 직전에 완료돼도 조기 종료 재확인을 놓치지 않는다.
            self.active_changed.notify_one();
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub(crate) struct FailStopAdmission<'a> {
    coordinator: &'a Arc<FailStopCoordinator>,
    state: MutexGuard<'a, State>,
}

impl FailStopAdmission<'_> {
    pub(crate) fn register(
        mut self,
        task_id: String,
    ) -> Result<ActiveExecution, ActiveExecutionError> {
        if !self.state.active.insert(task_id.clone()) {
            return Err(ActiveExecutionError::DuplicateTask(task_id));
        }
        Ok(ActiveExecution {
            coordinator: Arc::clone(self.coordinator),
            task_id,
            resolved: false,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ActiveExecutionError {
    #[error("활성 실행 taskId가 중복되었습니다: {0}")]
    DuplicateTask(String),
}

#[derive(Debug)]
pub(crate) struct ActiveExecution {
    coordinator: Arc<FailStopCoordinator>,
    task_id: String,
    resolved: bool,
}

impl ActiveExecution {
    pub(crate) fn complete(mut self) {
        self.coordinator.complete(&self.task_id);
        self.resolved = true;
    }
}

impl Drop for ActiveExecution {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        self.coordinator.activate(CleanupFailureReport::new(
            self.task_id.clone(),
            "활성 실행 소유권 종료",
            vec!["실행 lifecycle"],
            "소유권이 정리 결과 없이 종료됨",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[test]
    fn first_report_creates_one_deadline_and_later_reports_do_not_extend_it() {
        let base = Instant::now();
        let now = Arc::new(Mutex::new(base));
        let clock_calls = Arc::new(AtomicUsize::new(0));
        let clock = {
            let now = Arc::clone(&now);
            let clock_calls = Arc::clone(&clock_calls);
            Arc::new(move || {
                clock_calls.fetch_add(1, Ordering::SeqCst);
                *now.lock().unwrap()
            }) as Arc<Clock>
        };
        let coordinator = FailStopCoordinator::with_clock(
            FailStopSettings::new(Duration::from_secs(10)).unwrap(),
            clock,
        );
        let first = coordinator.activate(CleanupFailureReport::new(
            "first",
            "cgroup 제거",
            vec!["작업 cgroup"],
            "실패",
        ));
        *now.lock().unwrap() = base + Duration::from_secs(3);
        let second = coordinator.activate(CleanupFailureReport::new(
            "second",
            "출력 reader 정리",
            vec!["stdout reader"],
            "실패",
        ));

        assert_eq!(first, second);
        assert_eq!(first.at(), base + Duration::from_secs(10));
        assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
        assert_eq!(coordinator.first_report().unwrap().task_id, "first");
    }

    #[test]
    fn concurrent_reports_share_the_first_deadline() {
        let base = Instant::now();
        let coordinator = FailStopCoordinator::with_clock(
            FailStopSettings::new(Duration::from_secs(10)).unwrap(),
            Arc::new(move || base),
        );
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|index| {
                let coordinator = Arc::clone(&coordinator);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    coordinator.activate(CleanupFailureReport::new(
                        format!("task-{index}"),
                        "동시 정리 실패",
                        vec!["작업 cgroup"],
                        "실패",
                    ))
                })
            })
            .collect::<Vec<_>>();

        let deadlines = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert!(deadlines.iter().all(|deadline| *deadline == deadlines[0]));
        assert_eq!(deadlines[0].at(), base + Duration::from_secs(10));
    }

    #[test]
    fn exhausted_task_cleanup_does_not_consume_fail_stop_budget() {
        let base = Instant::now();
        let now = Arc::new(Mutex::new(base));
        let clock = {
            let now = Arc::clone(&now);
            Arc::new(move || *now.lock().unwrap()) as Arc<Clock>
        };
        let coordinator = FailStopCoordinator::with_clock(
            FailStopSettings::new(Duration::from_secs(10)).unwrap(),
            clock,
        );
        let task_deadline = MonotonicDeadline::from_start(base, Duration::from_secs(3)).unwrap();
        *now.lock().unwrap() = base + Duration::from_secs(4);
        assert_eq!(task_deadline.remaining_at(*now.lock().unwrap()), None);

        let fail_stop_deadline = coordinator.activate(CleanupFailureReport::new(
            "task",
            "작업별 정리 기한 소진",
            vec!["작업 cgroup"],
            "별도 fail-stop 예산으로 재시도",
        ));
        assert_eq!(fail_stop_deadline.at(), base + Duration::from_secs(14));
    }

    #[test]
    fn admission_and_fail_stop_transition_are_mutually_exclusive() {
        let coordinator =
            FailStopCoordinator::new(FailStopSettings::new(Duration::from_secs(1)).unwrap());
        let admission = coordinator.try_admit().unwrap();
        let active = admission.register("task".to_owned()).unwrap();
        assert_eq!(coordinator.active_count(), 1);

        coordinator.activate(CleanupFailureReport::new(
            "task",
            "시험",
            vec!["cgroup"],
            "실패",
        ));
        assert!(coordinator.try_admit().is_none());
        active.complete();
        assert_eq!(coordinator.active_count(), 0);
    }

    #[test]
    fn unresolved_active_owner_forces_fail_stop() {
        let coordinator =
            FailStopCoordinator::new(FailStopSettings::new(Duration::from_secs(1)).unwrap());
        let active = coordinator
            .try_admit()
            .unwrap()
            .register("task".to_owned())
            .unwrap();
        drop(active);

        assert!(coordinator.is_fail_stopping());
        assert_eq!(coordinator.active_count(), 1);
    }
}
