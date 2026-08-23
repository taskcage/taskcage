//! 작업 cgroup의 전체 프로세스 트리 종료와 정리 확인을 담당한다.

use std::fs::{self, File};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use crate::cleanup_fault::{CleanupFaultPoint, CleanupFaults};
use crate::deadline::MonotonicDeadline;

use super::events::{
    JobStats, KernelEvents, read_flat_keys, read_kernel_events, read_optional_u64, read_u64,
    required_key,
};
use super::limits::VerifiedCgroupLimits;
use super::manager::{CgroupError, cgroup_io_error, write_control};

#[derive(Debug)]
/// 실행 중인 작업 하나와 그 cgroup 파일 설명자를 함께 보관한다.
pub struct JobCgroup {
    pub(super) job_id: String,
    pub(super) path: PathBuf,
    pub(super) directory: File,
    pub(super) baseline: KernelEvents,
    pub(super) verified_limits: VerifiedCgroupLimits,
    pub(super) cleaned: bool,
    #[cfg(target_os = "linux")]
    pub(super) cleanup_faults: Option<Arc<CleanupFaults>>,
}

#[cfg(target_os = "linux")]
impl JobCgroup {
    pub fn id(&self) -> &str {
        &self.job_id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw_fd(&self) -> RawFd {
        self.directory.as_raw_fd()
    }

    pub fn verified_limits(&self) -> VerifiedCgroupLimits {
        self.verified_limits
    }

    pub fn is_cleaned(&self) -> bool {
        self.cleaned
    }

    #[cfg(target_os = "linux")]
    pub fn cleanup_faults(&self) -> Option<Arc<CleanupFaults>> {
        self.cleanup_faults.clone()
    }

    pub fn is_populated(&self) -> Result<bool, CgroupError> {
        let path = self.path.join("cgroup.events");
        #[cfg(target_os = "linux")]
        if self
            .cleanup_faults
            .as_ref()
            .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::PopulatedZero))
        {
            return Err(injected_cleanup_error(
                "작업 상태 확인",
                &path,
                CleanupFaultPoint::PopulatedZero,
            ));
        }
        let events = read_flat_keys(&path)?;
        Ok(required_key(&events, &path, "populated")? != 0)
    }

    pub fn contains_pid(&self, pid: libc::pid_t) -> Result<bool, CgroupError> {
        let path = self.path.join("cgroup.procs");
        let processes = fs::read_to_string(&path)
            .map_err(|source| cgroup_io_error("작업 프로세스 읽기", &path, source))?;
        Ok(processes
            .lines()
            .filter_map(|value| value.parse::<libc::pid_t>().ok())
            .any(|value| value == pid))
    }

    /// 대표 PID 하나가 아니라 작업 cgroup 아래의 모든 프로세스를 종료한다.
    pub fn kill_all(&self) -> Result<(), CgroupError> {
        let path = self.path.join("cgroup.kill");
        #[cfg(target_os = "linux")]
        if self
            .cleanup_faults
            .as_ref()
            .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::CgroupKill))
        {
            return Err(injected_cleanup_error(
                "작업 전체 종료",
                &path,
                CleanupFaultPoint::CgroupKill,
            ));
        }
        write_control(&path, "1\n")
    }

    pub async fn wait_empty_until(&self, deadline: MonotonicDeadline) -> Result<(), CgroupError> {
        loop {
            if !self.is_populated()? {
                return Ok(());
            }
            let Some(remaining) = deadline.remaining() else {
                return Err(CgroupError::EmptyTimeout {
                    path: self.path.clone(),
                    timeout: deadline.budget(),
                });
            };
            sleep(remaining.min(Duration::from_millis(10))).await;
        }
    }

    pub fn stats(&self) -> Result<JobStats, CgroupError> {
        let cpu_path = self.path.join("cpu.stat");
        #[cfg(target_os = "linux")]
        if self
            .cleanup_faults
            .as_ref()
            .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::Statistics))
        {
            return Err(injected_cleanup_error(
                "작업 통계 수집",
                &cpu_path,
                CleanupFaultPoint::Statistics,
            ));
        }
        let cpu = read_flat_keys(&cpu_path)?;
        let current_events = read_kernel_events(&self.path)?;
        let memory_current = read_u64(&self.path.join("memory.current"))?;
        let memory_peak =
            read_optional_u64(&self.path.join("memory.peak"))?.unwrap_or(memory_current);
        let current_processes = read_u64(&self.path.join("pids.current"))?;
        let peak_processes = read_optional_u64(&self.path.join("pids.peak"))?;

        Ok(JobStats {
            cpu_usage_micros: required_key(&cpu, &cpu_path, "usage_usec")?,
            memory_current_bytes: memory_current,
            memory_peak_bytes: memory_peak,
            current_processes,
            peak_processes,
            event_delta: current_events.delta_from(&self.baseline),
        })
    }

    /// 전체 종료, 빈 상태 확인, 통계 읽기, cgroup 제거 순서를 지킨다.
    pub async fn finish_until(
        &mut self,
        deadline: MonotonicDeadline,
    ) -> Result<JobStats, CgroupError> {
        if self.is_populated()? {
            self.kill_all()?;
        }
        self.finish_after_kill_until(deadline).await
    }

    /// 이미 cgroup.kill을 보낸 제어 종료 경로는 같은 종료 명령을 중복 전송하지 않는다.
    pub async fn finish_after_kill_until(
        &mut self,
        deadline: MonotonicDeadline,
    ) -> Result<JobStats, CgroupError> {
        self.wait_empty_until(deadline).await?;

        if deadline.remaining().is_none() {
            return Err(CgroupError::EmptyTimeout {
                path: self.path.clone(),
                timeout: deadline.budget(),
            });
        }

        // 통계 읽기에 실패해도 빈 cgroup 제거는 반드시 시도한다.
        let stats = self.stats();
        #[cfg(target_os = "linux")]
        let removal = if self
            .cleanup_faults
            .as_ref()
            .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::CgroupRemoval))
        {
            Err(injected_cleanup_error(
                "작업 cgroup 제거",
                &self.path,
                CleanupFaultPoint::CgroupRemoval,
            ))
        } else {
            fs::remove_dir(&self.path)
                .map_err(|source| cgroup_io_error("작업 cgroup 제거", &self.path, source))
        };
        #[cfg(not(target_os = "linux"))]
        let removal = fs::remove_dir(&self.path)
            .map_err(|source| cgroup_io_error("작업 cgroup 제거", &self.path, source));
        if removal.is_ok() {
            self.cleaned = true;
        }
        match (stats, removal) {
            (Ok(stats), Ok(())) => Ok(stats),
            (Err(primary), Ok(())) => Err(primary),
            (Ok(_), Err(cleanup)) => Err(cleanup),
            (Err(primary), Err(cleanup)) => Err(CgroupError::CleanupCombined {
                primary: primary.to_string(),
                cleanup: cleanup.to_string(),
            }),
        }
    }
}

#[cfg(target_os = "linux")]
fn injected_cleanup_error(
    operation: &'static str,
    path: &Path,
    point: CleanupFaultPoint,
) -> CgroupError {
    CgroupError::Io {
        operation,
        path: path.to_path_buf(),
        source: CleanupFaults::error(point),
    }
}

#[cfg(target_os = "linux")]
impl Drop for JobCgroup {
    fn drop(&mut self) {
        if self.cleaned || !self.path.exists() {
            return;
        }
        // 오류 반환이 불가능한 마지막 방어선이다. 정상 경로에서는 `finish`가 빈 상태를 확인한다.
        let _ = write_control(&self.path.join("cgroup.kill"), "1\n");
        let _ = fs::remove_dir(&self.path);
    }
}
