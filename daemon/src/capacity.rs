//! 공개 capability와 실제 실행기가 함께 사용하는 비차단 작업 슬롯이다.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "UDS handler가 다음 단계에서 capacity 설정과 submit 경계를 연결합니다"
    )
)]

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use thiserror::Error;
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TaskCapacitySettings {
    max_concurrent_tasks: NonZeroU32,
}

impl TaskCapacitySettings {
    pub(crate) fn new(max_concurrent_tasks: u32) -> Result<Self, TaskCapacitySettingsError> {
        let max_concurrent_tasks =
            NonZeroU32::new(max_concurrent_tasks).ok_or(TaskCapacitySettingsError::ZeroMaximum)?;
        Ok(Self {
            max_concurrent_tasks,
        })
    }

    pub(crate) fn max_concurrent_tasks(self) -> u32 {
        self.max_concurrent_tasks.get()
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskCapacitySettingsError {
    #[error("최대 동시 실행 수는 0보다 커야 합니다")]
    ZeroMaximum,
}

#[derive(Debug)]
pub(crate) struct TaskCapacity {
    settings: TaskCapacitySettings,
    in_use: AtomicU32,
    retained_for_fail_stop: AtomicU32,
    idle: Notify,
}

impl TaskCapacity {
    pub(crate) fn new(settings: TaskCapacitySettings) -> Self {
        Self {
            settings,
            in_use: AtomicU32::new(0),
            retained_for_fail_stop: AtomicU32::new(0),
            idle: Notify::new(),
        }
    }

    pub(crate) fn settings(&self) -> TaskCapacitySettings {
        self.settings
    }

    pub(crate) fn in_use(&self) -> u32 {
        self.in_use.load(Ordering::Acquire)
    }

    /// 대기하지 않고 즉시 실행 슬롯 하나를 얻는다.
    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<TaskCapacityPermit> {
        let maximum = self.settings.max_concurrent_tasks();
        let mut current = self.in_use.load(Ordering::Acquire);
        loop {
            if current >= maximum {
                return None;
            }
            match self.in_use.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(TaskCapacityPermit {
                        capacity: Arc::clone(self),
                        release_on_drop: true,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// 신규 연결을 닫은 정상 shutdown에서 이미 시작된 작업 lifecycle이 끝날 때까지 기다린다.
    pub(crate) async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.in_use.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn retained_for_fail_stop(&self) -> u32 {
        self.retained_for_fail_stop.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(crate) struct TaskCapacityPermit {
    capacity: Arc<TaskCapacity>,
    release_on_drop: bool,
}

impl TaskCapacityPermit {
    /// 정리 완료를 증명하지 못한 슬롯은 fail-stop 종료까지 다시 사용하지 않는다.
    pub(crate) fn retain_for_fail_stop(mut self) {
        self.capacity
            .retained_for_fail_stop
            .fetch_add(1, Ordering::AcqRel);
        self.release_on_drop = false;
    }
}

impl Drop for TaskCapacityPermit {
    fn drop(&mut self) {
        if !self.release_on_drop {
            return;
        }
        let previous = self.capacity.in_use.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "반환할 작업 실행 슬롯이 있어야 합니다");
        if previous == 1 {
            self.capacity.idle.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_maximum_is_rejected() {
        assert_eq!(
            TaskCapacitySettings::new(0),
            Err(TaskCapacitySettingsError::ZeroMaximum)
        );
    }

    #[test]
    fn permits_are_nonblocking_and_reusable_after_drop() {
        let capacity = Arc::new(TaskCapacity::new(TaskCapacitySettings::new(2).unwrap()));
        let first = capacity.try_acquire().unwrap();
        let second = capacity.try_acquire().unwrap();

        assert!(capacity.try_acquire().is_none());
        drop(first);
        assert!(capacity.try_acquire().is_some());
        drop(second);
    }

    #[test]
    fn retained_permit_is_not_reused() {
        let capacity = Arc::new(TaskCapacity::new(TaskCapacitySettings::new(1).unwrap()));
        capacity.try_acquire().unwrap().retain_for_fail_stop();

        assert!(capacity.try_acquire().is_none());
    }

    #[tokio::test]
    async fn idle_waiter_finishes_only_after_the_last_permit_returns() {
        let capacity = Arc::new(TaskCapacity::new(TaskCapacitySettings::new(2).unwrap()));
        let first = capacity.try_acquire().unwrap();
        let second = capacity.try_acquire().unwrap();

        let waiting_capacity = Arc::clone(&capacity);
        let mut waiting = Box::pin(async move { waiting_capacity.wait_idle().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        drop(first);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        drop(second);
        waiting.await;
    }
}
