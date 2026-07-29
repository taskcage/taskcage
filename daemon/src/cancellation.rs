//! 실행 중인 작업의 cancel 신호와 정리 완료 통지를 한 번만 공유한다.

use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Notify;

use crate::lifecycle::{ControlTrigger, ControlTriggers, TerminalTriggerLatch};
use crate::protocol::TaskPayload;

#[derive(Debug)]
struct CancellationState {
    terminal: TerminalTriggerLatch,
    cancel_notify: Notify,
    finished: Mutex<Option<TaskPayload>>,
    finished_notify: Notify,
}

impl CancellationState {
    fn lock_finished(&self) -> MutexGuard<'_, Option<TaskPayload>> {
        self.finished
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Runner가 timeout, cancel과 정상 종료의 첫 관찰 순서를 결정할 때 사용한다.
#[derive(Debug)]
pub(crate) struct CancellationRuntime {
    state: Arc<CancellationState>,
}

impl CancellationRuntime {
    pub(crate) async fn cancelled(&self) {
        loop {
            // 알림 등록을 먼저 해 cancel 관찰과 상태 확인 사이의 missed wakeup을 막는다.
            let notified = self.state.cancel_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.terminal.snapshot().first() == Some(ControlTrigger::Cancelled) {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn observe_timeout(&self) -> bool {
        self.state.terminal.observe(ControlTrigger::TimedOut)
    }

    pub(crate) fn close_without_control(&self) -> bool {
        self.state.terminal.close_without_control()
    }

    pub(crate) fn control_snapshot(&self) -> ControlTriggers {
        self.state.terminal.snapshot()
    }
}

/// Registry가 RUNNING 작업과 함께 보관하는 단일 취소 제어권이다.
#[derive(Debug)]
pub(crate) struct RunningCancellation {
    state: Arc<CancellationState>,
}

impl RunningCancellation {
    pub(crate) fn request_cancel(&self) -> CancellationWaiter {
        if self.state.terminal.observe(ControlTrigger::Cancelled) {
            self.state.cancel_notify.notify_waiters();
        }
        CancellationWaiter {
            state: Arc::clone(&self.state),
        }
    }

    /// Registry가 FINISHED를 저장한 뒤에만 취소 응답 대기자를 깨운다.
    pub(crate) fn complete(self, finished: TaskPayload) {
        debug_assert!(matches!(finished, TaskPayload::Finished { .. }));
        *self.state.lock_finished() = Some(finished);
        self.state.finished_notify.notify_waiters();
    }
}

/// 요청 task가 사라져도 내부 cancel과 cleanup은 독립적으로 계속된다.
#[derive(Debug)]
pub(crate) struct CancellationWaiter {
    state: Arc<CancellationState>,
}

impl CancellationWaiter {
    pub(crate) async fn wait(self) -> TaskPayload {
        loop {
            let notified = self.state.finished_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(finished) = self.state.lock_finished().clone() {
                return finished;
            }
            notified.await;
        }
    }
}

pub(crate) fn cancellation_channel() -> (CancellationRuntime, RunningCancellation) {
    let state = Arc::new(CancellationState {
        terminal: TerminalTriggerLatch::default(),
        cancel_notify: Notify::new(),
        finished: Mutex::new(None),
        finished_notify: Notify::new(),
    });
    (
        CancellationRuntime {
            state: Arc::clone(&state),
        },
        RunningCancellation { state },
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;
    use crate::protocol::{ProcessResult, TaskOutput, TaskTiming, TaskUsage, TerminationReason};

    fn finished(reason: TerminationReason) -> TaskPayload {
        TaskPayload::Finished {
            task_id: "task".to_owned(),
            termination_reason: reason,
            process: ProcessResult {
                exit_code: None,
                signal: Some("SIGKILL".to_owned()),
            },
            timing: TaskTiming {
                submitted_at: "submitted".to_owned(),
                started_at: "started".to_owned(),
                finished_at: "finished".to_owned(),
                wall_time_ms: 1,
            },
            usage: TaskUsage {
                cpu_time_micros: 0,
                memory_peak_bytes: 0,
            },
            output: TaskOutput {
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
        }
    }

    #[tokio::test]
    async fn concurrent_cancel_requests_share_one_terminal_trigger_and_completion() {
        let (runtime, running) = cancellation_channel();
        let first = running.request_cancel();
        let second = running.request_cancel();

        runtime.cancelled().await;
        assert_eq!(
            runtime.control_snapshot().first(),
            Some(ControlTrigger::Cancelled)
        );

        let expected = finished(TerminationReason::Cancelled);
        running.complete(expected.clone());
        assert_eq!(first.wait().await, expected);
        assert_eq!(second.wait().await, expected);
    }

    #[tokio::test]
    async fn timeout_or_normal_exit_cannot_be_overwritten_by_late_cancel() {
        let (timeout_runtime, timeout_running) = cancellation_channel();
        assert!(timeout_runtime.observe_timeout());
        let timeout_waiter = timeout_running.request_cancel();
        assert_eq!(
            timeout_runtime.control_snapshot().first(),
            Some(ControlTrigger::TimedOut)
        );
        timeout_running.complete(finished(TerminationReason::TimedOut));
        assert!(matches!(
            timeout_waiter.wait().await,
            TaskPayload::Finished {
                termination_reason: TerminationReason::TimedOut,
                ..
            }
        ));

        let (exit_runtime, exit_running) = cancellation_channel();
        assert!(exit_runtime.close_without_control());
        let exit_waiter = exit_running.request_cancel();
        assert_eq!(exit_runtime.control_snapshot().first(), None);
        exit_running.complete(finished(TerminationReason::Exited));
        assert!(matches!(
            exit_waiter.wait().await,
            TaskPayload::Finished {
                termination_reason: TerminationReason::Exited,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn cancel_waiter_does_not_finish_before_registry_completion_signal() {
        let (runtime, running) = cancellation_channel();
        let waiter = running.request_cancel();
        runtime.cancelled().await;

        let mut waiting = Box::pin(waiter.wait());
        assert!(
            timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        running.complete(finished(TerminationReason::Cancelled));
        assert!(matches!(
            waiting.await,
            TaskPayload::Finished {
                termination_reason: TerminationReason::Cancelled,
                ..
            }
        ));
    }
}
