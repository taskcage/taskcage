//! 실패·취소 뒤 child 회수와 output reader 종료를 끝까지 수행한다.

use std::io;
use std::time::Duration;

use taskcage_core::output::CapturedOutput;
use tokio::time::sleep;

use crate::cleanup_fault::{CleanupFaultPoint, CleanupFaults};
use crate::deadline::MonotonicDeadline;

use super::ExecutorError;
use super::spawn::{PendingProcess, SpawnedProcess, StartCommittedProcess};
use super::wait::{ProcessExit, wait_blocking, wait_nohang};

impl PendingProcess {
    pub async fn abort_until(&mut self, deadline: MonotonicDeadline) -> Result<(), ExecutorError> {
        self.start_write_end.take();
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        #[cfg(target_os = "linux")]
        let injected = self
            .cleanup_faults
            .as_ref()
            .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::PendingCloneAbort));
        #[cfg(not(target_os = "linux"))]
        let injected = false;
        let wait_result = if injected {
            #[cfg(target_os = "linux")]
            {
                Err(ExecutorError::Wait(CleanupFaults::error(
                    CleanupFaultPoint::PendingCloneAbort,
                )))
            }
            #[cfg(not(target_os = "linux"))]
            {
                unreachable!("운영 빌드에는 cleanup fault가 없습니다")
            }
        } else {
            loop {
                if let Some(status) = wait_nohang(self.pid)? {
                    break Ok(status);
                }
                let Some(remaining) = deadline.remaining() else {
                    break Err(ExecutorError::Wait(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "pending clone3 child의 종료 상태를 회수하지 못했습니다",
                    )));
                };
                sleep(remaining.min(Duration::from_millis(10))).await;
            }
        };
        self.exec_read_end.take();
        if let Some(output_readers) = self.output_readers.take() {
            output_readers.cancel_and_join();
        }
        if wait_result.is_ok() {
            self.reaped = true;
        }
        wait_result.map(|_| ())
    }

    fn stop_and_reap(&mut self) -> Result<(), ExecutorError> {
        self.start_write_end.take();
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let wait_result = wait_blocking(self.pid);
        self.exec_read_end.take();
        if let Some(output_readers) = self.output_readers.take() {
            output_readers.cancel_and_join();
        }
        if wait_result.is_ok() {
            self.reaped = true;
        }
        wait_result.map(|_| ())
    }
}

impl Drop for PendingProcess {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.stop_and_reap();
        }
    }
}

impl Drop for StartCommittedProcess {
    fn drop(&mut self) {
        if self.output_readers.is_some() {
            let _ = self.stop_and_reap();
        }
    }
}

impl SpawnedProcess {
    pub async fn reap_after_kill_until(
        &self,
        deadline: MonotonicDeadline,
    ) -> Result<ProcessExit, ExecutorError> {
        #[cfg(target_os = "linux")]
        {
            if self
                .cleanup_faults
                .as_ref()
                .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::DirectChildReap))
            {
                let mut cached = self.reaped_exit.lock().expect("시험용 child 상태 잠금");
                if cached.is_none() {
                    *cached = Some(wait_blocking(self.pid)?);
                }
                return Err(ExecutorError::Wait(CleanupFaults::error(
                    CleanupFaultPoint::DirectChildReap,
                )));
            }
            if let Some(exit) = *self.reaped_exit.lock().expect("시험용 child 상태 잠금") {
                return Ok(exit);
            }
        }
        loop {
            if let Some(status) = wait_nohang(self.pid)? {
                return Ok(status);
            }
            let Some(remaining) = deadline.remaining() else {
                return Err(ExecutorError::Wait(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "cgroup 전체 종료 뒤 target의 종료 상태를 회수하지 못했습니다",
                )));
            };
            sleep(remaining.min(Duration::from_millis(10))).await;
        }
    }

    pub async fn finish_output_until(
        self,
        deadline: MonotonicDeadline,
    ) -> Result<CapturedOutput, ExecutorError> {
        self.output_readers.collect_until(deadline).await
    }
}
