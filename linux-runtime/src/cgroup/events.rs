//! cgroup 커널 이벤트와 작업 자원 통계를 읽고 해석한다.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;

use super::manager::{CgroupError, cgroup_io_error};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
/// 커널이 누적해서 기록하는 메모리 부족과 프로세스 제한 사건이다.
pub struct KernelEvents {
    pub memory_oom: u64,
    pub memory_oom_kill: u64,
    pub pids_max: u64,
}

impl KernelEvents {
    /// 작업 시작 전 수치와 비교해 이번 작업에서 늘어난 값만 계산한다.
    pub fn delta_from(&self, baseline: &Self) -> Self {
        Self {
            memory_oom: self.memory_oom.saturating_sub(baseline.memory_oom),
            memory_oom_kill: self
                .memory_oom_kill
                .saturating_sub(baseline.memory_oom_kill),
            pids_max: self.pids_max.saturating_sub(baseline.pids_max),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
/// 작업 종료 전에 읽은 커널 자원 사용량이다.
pub struct JobStats {
    pub cpu_usage_micros: u64,
    pub memory_current_bytes: u64,
    pub memory_peak_bytes: u64,
    pub current_processes: u64,
    pub peak_processes: Option<u64>,
    pub event_delta: KernelEvents,
}

pub(super) fn read_kernel_events(path: &Path) -> Result<KernelEvents, CgroupError> {
    let memory_path = path.join("memory.events.local");
    let pids_path = path.join("pids.events");
    let memory = read_flat_keys(&memory_path)?;
    let pids = read_flat_keys(&pids_path)?;
    Ok(KernelEvents {
        memory_oom: required_key(&memory, &memory_path, "oom")?,
        memory_oom_kill: required_key(&memory, &memory_path, "oom_kill")?,
        pids_max: required_key(&pids, &pids_path, "max")?,
    })
}

#[cfg(target_os = "linux")]
pub(super) fn read_flat_keys(path: &Path) -> Result<BTreeMap<String, u64>, CgroupError> {
    let contents = fs::read_to_string(path)
        .map_err(|source| cgroup_io_error("cgroup 항목 읽기", path, source))?;
    let mut values = BTreeMap::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let (Some(key), Some(raw_value), None) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let Ok(value) = raw_value.parse::<u64>() {
            values.insert(key.to_owned(), value);
        }
    }
    Ok(values)
}

#[cfg(target_os = "linux")]
pub(super) fn required_key(
    values: &BTreeMap<String, u64>,
    path: &Path,
    key: &'static str,
) -> Result<u64, CgroupError> {
    values
        .get(key)
        .copied()
        .ok_or_else(|| CgroupError::MissingKey {
            path: path.to_path_buf(),
            key,
        })
}

#[cfg(target_os = "linux")]
pub(super) fn read_word_set(path: &Path) -> Result<BTreeSet<String>, CgroupError> {
    let contents = fs::read_to_string(path)
        .map_err(|source| cgroup_io_error("cgroup 제어기 읽기", path, source))?;
    Ok(contents.split_whitespace().map(str::to_owned).collect())
}

#[cfg(target_os = "linux")]
pub(super) fn read_u64(path: &Path) -> Result<u64, CgroupError> {
    let value = fs::read_to_string(path)
        .map_err(|source| cgroup_io_error("cgroup 숫자 읽기", path, source))?;
    value.trim().parse::<u64>().map_err(|source| {
        cgroup_io_error(
            "cgroup 숫자 변환",
            path,
            io::Error::new(io::ErrorKind::InvalidData, source),
        )
    })
}

#[cfg(target_os = "linux")]
pub(super) fn read_optional_u64(path: &Path) -> Result<Option<u64>, CgroupError> {
    if path.exists() {
        read_u64(path).map(Some)
    } else {
        Ok(None)
    }
}
