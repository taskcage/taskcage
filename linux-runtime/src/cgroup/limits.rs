//! 실행 전에 적용하고 read-back하는 cgroup 자원 제한을 담당한다.

use std::fs::{self, File};
use std::num::NonZeroU64;
use std::path::Path;

use super::events::{KernelEvents, read_kernel_events};
use super::manager::{
    CgroupCreateFaults, CgroupError, cgroup_io_error, require_regular_file, write_control,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 한 주기 안에서 CPU를 얼마나 오래 사용할지 정한다.
pub struct CpuLimit {
    pub quota_micros: NonZeroU64,
    pub period_micros: NonZeroU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 작업 cgroup 하나에 적용할 자원 상한이다.
pub struct CgroupLimits {
    pub memory_max_bytes: NonZeroU64,
    pub max_processes: NonZeroU64,
    pub cpu: CpuLimit,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 모든 cgroup 제한을 쓰고 같은 값으로 다시 읽은 뒤에만 만드는 내부 증거다.
pub struct VerifiedCgroupLimits {
    limits: CgroupLimits,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
impl VerifiedCgroupLimits {
    pub(super) fn new(limits: CgroupLimits) -> Self {
        Self { limits }
    }

    pub fn limits(self) -> CgroupLimits {
        self.limits
    }

    #[cfg(target_os = "linux")]
    pub fn for_test(limits: CgroupLimits) -> Self {
        Self::new(limits)
    }
}

pub(super) fn configure_job_with_read_back_mismatch(
    path: &Path,
    limits: CgroupLimits,
    faults: &CgroupCreateFaults,
) -> Result<(File, KernelEvents, VerifiedCgroupLimits), CgroupError> {
    faults.record_read_back_attempt();
    let expected = limits.memory_max_bytes.get().to_string();
    write_and_verify_with_injected_actual(
        &path.join("memory.max"),
        &expected,
        "injected-read-back-value",
    )?;
    unreachable!("주입된 read-back 값은 요청값과 달라야 합니다")
}

#[cfg(target_os = "linux")]
pub(super) fn configure_job(
    path: &Path,
    limits: CgroupLimits,
) -> Result<(File, KernelEvents, VerifiedCgroupLimits), CgroupError> {
    // 커널 제어 파일은 쓰기 성공만으로 적용됐다고 단정하지 않고 다시 읽어 확인한다.
    write_and_verify(
        &path.join("memory.max"),
        &limits.memory_max_bytes.get().to_string(),
    )?;
    let oom_group = path.join("memory.oom.group");
    if oom_group.exists() {
        write_and_verify(&oom_group, "1")?;
    }
    write_and_verify(
        &path.join("pids.max"),
        &limits.max_processes.get().to_string(),
    )?;
    write_and_verify(
        &path.join("cpu.max"),
        &format!(
            "{} {}",
            limits.cpu.quota_micros.get(),
            limits.cpu.period_micros.get()
        ),
    )?;
    require_regular_file(&path.join("cgroup.kill"), "작업 전체 종료")?;
    require_regular_file(&path.join("cgroup.events"), "작업 상태")?;
    let directory =
        File::open(path).map_err(|source| cgroup_io_error("작업 cgroup 열기", path, source))?;
    let baseline = read_kernel_events(path)?;
    Ok((directory, baseline, VerifiedCgroupLimits::new(limits)))
}

#[cfg(target_os = "linux")]
pub(super) fn write_and_verify(path: &Path, value: &str) -> Result<(), CgroupError> {
    write_control(path, &format!("{value}\n"))?;
    let actual = fs::read_to_string(path)
        .map_err(|source| cgroup_io_error("cgroup 값 재확인", path, source))?;
    verify_read_back(path, value, actual.trim())
}

#[cfg(target_os = "linux")]
fn write_and_verify_with_injected_actual(
    path: &Path,
    value: &str,
    injected_actual: &str,
) -> Result<(), CgroupError> {
    write_control(path, &format!("{value}\n"))?;
    let _actual = fs::read_to_string(path)
        .map_err(|source| cgroup_io_error("cgroup 값 재확인", path, source))?;
    verify_read_back(path, value, injected_actual)
}

#[cfg(target_os = "linux")]
fn verify_read_back(path: &Path, value: &str, actual: &str) -> Result<(), CgroupError> {
    if actual == value {
        Ok(())
    } else {
        Err(CgroupError::ValueMismatch {
            path: path.to_path_buf(),
            expected: value.to_owned(),
            actual: actual.to_owned(),
        })
    }
}
