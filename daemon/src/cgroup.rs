//! 데몬이 제어할 cgroup v2 경로를 찾고 경로 이탈을 막는다.
//!
//! 이 모듈은 아직 작업을 실행하지 않는다. 실행 기능이 추가될 때도 여기서 확인한
//! 위임 경로 안에서만 작업 cgroup을 만들도록 경로 규칙을 한곳에 모아 둔다.

use std::fs;
use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(target_os = "linux")]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(target_os = "linux")]
use std::time::Instant;

use serde::Serialize;
use thiserror::Error;
#[cfg(target_os = "linux")]
use tokio::time::sleep;

#[cfg(target_os = "linux")]
use crate::preflight::VerifiedEnvironment;

pub const DEFAULT_CGROUP_MOUNT: &str = "/sys/fs/cgroup";
pub const SELF_CGROUP_FILE: &str = "/proc/self/cgroup";

#[derive(Debug, Error)]
pub enum CgroupPathError {
    #[error("{operation} 작업이 {path:?}에서 실패했습니다")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("현재 프로세스에서 하나의 cgroup v2 경로를 찾지 못했습니다")]
    InvalidMembership,
    #[error("위임 경로 {root:?}가 cgroup 마운트 {mount:?} 밖에 있습니다")]
    RootOutsideMount { root: PathBuf, mount: PathBuf },
    #[error(
        "설정한 위임 경로가 현재 프로세스의 실제 cgroup과 다릅니다: 설정 {configured:?}, 실제 {actual:?}"
    )]
    ConfiguredRootMismatch {
        configured: PathBuf,
        actual: PathBuf,
    },
    #[error("manager 이동 결과가 다릅니다: 예상 {expected:?}, 실제 {actual:?}")]
    ManagerMembershipMismatch { expected: PathBuf, actual: PathBuf },
    #[error("작업 식별자 형식이 올바르지 않습니다: {0:?}")]
    InvalidJobId(String),
    #[error("같은 작업 식별자의 cgroup이 이미 있습니다: {job_id:?}, 경로 {path:?}")]
    DuplicateJobId { job_id: String, path: PathBuf },
}

#[derive(Debug, Error)]
pub enum CgroupError {
    #[error(transparent)]
    Path(#[from] CgroupPathError),
    #[error("{operation} 작업이 {path:?}에서 실패했습니다")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("작업 cgroup 상위 경로가 이미 있습니다: {path:?}")]
    JobsAlreadyExists { path: PathBuf },
    #[error("cgroup 값이 요청과 다릅니다: {path:?}, 예상 {expected:?}, 실제 {actual:?}")]
    ValueMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("cgroup 값 {key:?}을 {path:?}에서 찾지 못했습니다")]
    MissingKey { path: PathBuf, key: &'static str },
    #[error("cgroup {path:?}가 {timeout:?} 안에 비지 않았습니다")]
    EmptyTimeout { path: PathBuf, timeout: Duration },
    #[error("작업 결과 수집과 cgroup 제거가 모두 실패했습니다: 결과={primary}; 제거={cleanup}")]
    CleanupCombined { primary: String, cleanup: String },
}

#[derive(Debug, Clone, Copy)]
/// 한 주기 안에서 CPU를 얼마나 오래 사용할지 정한다.
pub struct CpuLimit {
    pub quota_micros: NonZeroU64,
    pub period_micros: NonZeroU64,
}

#[derive(Debug, Clone, Copy)]
/// 작업 cgroup 하나에 적용할 자원 상한이다.
pub struct CgroupLimits {
    pub memory_max_bytes: NonZeroU64,
    pub max_processes: NonZeroU64,
    pub cpu: CpuLimit,
}

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

/// 위임받은 cgroup과 데몬·작업용 하위 경로를 한 묶음으로 보관한다.
#[derive(Debug, Clone)]
pub struct CgroupPaths {
    mount: PathBuf,
    root: PathBuf,
    manager: PathBuf,
    jobs: PathBuf,
}

impl CgroupPaths {
    /// 현재 프로세스가 속한 cgroup을 위임 경로로 사용한다.
    pub fn discover() -> Result<Self, CgroupPathError> {
        Self::resolve(None)
    }

    /// 설정으로 받은 경로가 있으면 그 경로를 사용하고, 없으면 현재 경로를 찾는다.
    pub fn resolve(root_override: Option<&Path>) -> Result<Self, CgroupPathError> {
        let mount_path = Path::new(DEFAULT_CGROUP_MOUNT);
        let mount = fs::canonicalize(mount_path)
            .map_err(|source| io_error("cgroup 마운트 경로 확인", mount_path, source))?;

        let membership = read_unified_membership()?;
        let discovered_root = mount.join(membership.strip_prefix("/").unwrap_or(&membership));
        let actual_root = fs::canonicalize(&discovered_root)
            .map_err(|source| io_error("실제 위임 경로 확인", &discovered_root, source))?;
        let supplied_root = root_override.unwrap_or(&actual_root);
        let root = fs::canonicalize(supplied_root)
            .map_err(|source| io_error("위임 경로 확인", supplied_root, source))?;

        // `..` 또는 심볼릭 링크가 실제 마운트 밖을 가리키면 여기서 거부한다.
        if !root.starts_with(&mount) {
            return Err(CgroupPathError::RootOutsideMount { root, mount });
        }
        if root != actual_root {
            return Err(CgroupPathError::ConfiguredRootMismatch {
                configured: root,
                actual: actual_root,
            });
        }

        Ok(Self {
            manager: root.join("manager"),
            jobs: root.join("jobs"),
            mount,
            root,
        })
    }

    pub fn mount(&self) -> &Path {
        &self.mount
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manager(&self) -> &Path {
        &self.manager
    }

    pub fn jobs(&self) -> &Path {
        &self.jobs
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn for_test(root: PathBuf) -> Self {
        Self {
            mount: root.clone(),
            manager: root.join("manager"),
            jobs: root.join("jobs"),
            root,
        }
    }

    /// 데몬이 manager cgroup으로 실제 이동했는지 `/proc/self/cgroup`에서 다시 확인한다.
    pub fn verify_manager_membership(&self) -> Result<(), CgroupPathError> {
        let actual = read_unified_membership()?;
        let expected = self.expected_membership(&self.manager)?;
        if actual == expected {
            Ok(())
        } else {
            Err(CgroupPathError::ManagerMembershipMismatch { expected, actual })
        }
    }

    /// 새 작업 경로를 계산하면서 경로 문자와 중복 식별자를 함께 검사한다.
    pub fn new_job_path(&self, job_id: &str) -> Result<PathBuf, CgroupPathError> {
        validate_job_id(job_id)?;
        let path = self.jobs.join(format!("job-{job_id}"));
        match fs::symlink_metadata(&path) {
            Ok(_) => Err(CgroupPathError::DuplicateJobId {
                job_id: job_id.to_owned(),
                path,
            }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(path),
            Err(source) => Err(io_error("작업 cgroup 중복 확인", &path, source)),
        }
    }

    fn expected_membership(&self, path: &Path) -> Result<PathBuf, CgroupPathError> {
        let relative =
            path.strip_prefix(&self.mount)
                .map_err(|_| CgroupPathError::RootOutsideMount {
                    root: path.to_path_buf(),
                    mount: self.mount.clone(),
                })?;
        Ok(Path::new("/").join(relative))
    }
}

pub fn read_unified_membership() -> Result<PathBuf, CgroupPathError> {
    let path = Path::new(SELF_CGROUP_FILE);
    let contents =
        fs::read_to_string(path).map_err(|source| io_error("현재 cgroup 읽기", path, source))?;
    parse_unified_membership(&contents)
}

fn parse_unified_membership(contents: &str) -> Result<PathBuf, CgroupPathError> {
    // cgroup v2 항목은 `0::<경로>` 형태이며 하나만 있어야 한다.
    let mut paths = contents.lines().filter_map(|line| line.strip_prefix("0::"));
    let Some(path) = paths.next() else {
        return Err(CgroupPathError::InvalidMembership);
    };
    if paths.next().is_some() || path.is_empty() || path.contains(" (deleted)") {
        return Err(CgroupPathError::InvalidMembership);
    }
    Ok(PathBuf::from(path))
}

fn validate_job_id(job_id: &str) -> Result<(), CgroupPathError> {
    let valid = !job_id.is_empty()
        && job_id.len() <= 64
        && job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(CgroupPathError::InvalidJobId(job_id.to_owned()))
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> CgroupPathError {
    CgroupPathError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(target_os = "linux")]
const REQUIRED_CONTROLLERS: [&str; 3] = ["cpu", "memory", "pids"];

#[cfg(target_os = "linux")]
#[derive(Debug)]
/// 검증된 위임 영역에서 작업 cgroup을 만들고 관리한다.
pub struct CgroupManager {
    paths: CgroupPaths,
}

#[cfg(target_os = "linux")]
impl CgroupManager {
    /// 외부에서 만들 수 없는 사전 검사 성공 값을 소비해야만 관리자를 만들 수 있다.
    pub fn initialize(environment: VerifiedEnvironment) -> Result<Self, CgroupError> {
        let paths = environment.into_paths();
        paths.verify_manager_membership()?;
        let root_enabled = read_word_set(&paths.root().join("cgroup.subtree_control"))?;
        ensure_controllers(&root_enabled, paths.root())?;

        match fs::create_dir(paths.jobs()) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                return Err(CgroupError::JobsAlreadyExists {
                    path: paths.jobs().to_path_buf(),
                });
            }
            Err(source) => {
                return Err(cgroup_io_error(
                    "작업 cgroup 상위 경로 만들기",
                    paths.jobs(),
                    source,
                ));
            }
        }

        if let Err(error) = enable_required_controllers(paths.jobs()) {
            return match fs::remove_dir(paths.jobs()) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(CgroupError::CleanupCombined {
                    primary: error.to_string(),
                    cleanup: cleanup.to_string(),
                }),
            };
        }
        Ok(Self { paths })
    }

    pub fn root(&self) -> &Path {
        self.paths.root()
    }

    pub fn create_job(&self, job_id: &str, limits: CgroupLimits) -> Result<JobCgroup, CgroupError> {
        self.create_job_with(job_id, limits, configure_job)
    }

    fn create_job_with<F>(
        &self,
        job_id: &str,
        limits: CgroupLimits,
        configure: F,
    ) -> Result<JobCgroup, CgroupError>
    where
        F: FnOnce(&Path, CgroupLimits) -> Result<(File, KernelEvents), CgroupError>,
    {
        let path = self.paths.new_job_path(job_id)?;
        fs::create_dir(&path)
            .map_err(|source| cgroup_io_error("작업 cgroup 만들기", &path, source))?;

        let configured = configure(&path, limits);

        match configured {
            Ok((directory, baseline)) => Ok(JobCgroup {
                job_id: job_id.to_owned(),
                path,
                directory,
                baseline,
                cleaned: false,
            }),
            Err(error) => {
                let _ = write_control(&path.join("cgroup.kill"), "1\n");
                match fs::remove_dir(&path) {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(CgroupError::CleanupCombined {
                        primary: error.to_string(),
                        cleanup: cleanup.to_string(),
                    }),
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn configure_job(path: &Path, limits: CgroupLimits) -> Result<(File, KernelEvents), CgroupError> {
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
    Ok((directory, baseline))
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
/// 실행 중인 작업 하나와 그 cgroup 파일 설명자를 함께 보관한다.
pub struct JobCgroup {
    job_id: String,
    path: PathBuf,
    directory: File,
    baseline: KernelEvents,
    cleaned: bool,
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

    pub fn is_populated(&self) -> Result<bool, CgroupError> {
        let path = self.path.join("cgroup.events");
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
        write_control(&self.path.join("cgroup.kill"), "1\n")
    }

    pub async fn wait_empty(&self, timeout: Duration) -> Result<(), CgroupError> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_populated()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CgroupError::EmptyTimeout {
                    path: self.path.clone(),
                    timeout,
                });
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn stats(&self) -> Result<JobStats, CgroupError> {
        let cpu_path = self.path.join("cpu.stat");
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
    pub async fn finish(self, timeout: Duration) -> Result<JobStats, CgroupError> {
        if self.is_populated()? {
            self.kill_all()?;
        }
        self.finish_after_kill(timeout).await
    }

    /// 이미 cgroup.kill을 보낸 제어 종료 경로는 같은 종료 명령을 중복 전송하지 않는다.
    pub(crate) async fn finish_after_kill(
        mut self,
        timeout: Duration,
    ) -> Result<JobStats, CgroupError> {
        self.wait_empty(timeout).await?;

        // 통계 읽기에 실패해도 빈 cgroup 제거는 반드시 시도한다.
        let stats = self.stats();
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

#[cfg(target_os = "linux")]
fn enable_required_controllers(path: &Path) -> Result<(), CgroupError> {
    let available = read_word_set(&path.join("cgroup.controllers"))?;
    ensure_controllers(&available, path)?;
    write_control(&path.join("cgroup.subtree_control"), "+cpu +memory +pids\n")?;
    let enabled = read_word_set(&path.join("cgroup.subtree_control"))?;
    ensure_controllers(&enabled, path)
}

#[cfg(target_os = "linux")]
fn ensure_controllers(controllers: &BTreeSet<String>, path: &Path) -> Result<(), CgroupError> {
    let missing: Vec<_> = REQUIRED_CONTROLLERS
        .iter()
        .filter(|controller| !controllers.contains(**controller))
        .copied()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CgroupError::Io {
            operation: "필수 cgroup 제어기 확인",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                format!("누락된 제어기: {}", missing.join(", ")),
            ),
        })
    }
}

#[cfg(target_os = "linux")]
fn read_kernel_events(path: &Path) -> Result<KernelEvents, CgroupError> {
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
fn read_flat_keys(path: &Path) -> Result<BTreeMap<String, u64>, CgroupError> {
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
fn required_key(
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
fn read_word_set(path: &Path) -> Result<BTreeSet<String>, CgroupError> {
    let contents = fs::read_to_string(path)
        .map_err(|source| cgroup_io_error("cgroup 제어기 읽기", path, source))?;
    Ok(contents.split_whitespace().map(str::to_owned).collect())
}

#[cfg(target_os = "linux")]
fn read_u64(path: &Path) -> Result<u64, CgroupError> {
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
fn read_optional_u64(path: &Path) -> Result<Option<u64>, CgroupError> {
    if path.exists() {
        read_u64(path).map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn write_and_verify(path: &Path, value: &str) -> Result<(), CgroupError> {
    write_control(path, &format!("{value}\n"))?;
    let actual = fs::read_to_string(path)
        .map_err(|source| cgroup_io_error("cgroup 값 재확인", path, source))?;
    if actual.trim() == value {
        Ok(())
    } else {
        Err(CgroupError::ValueMismatch {
            path: path.to_path_buf(),
            expected: value.to_owned(),
            actual: actual.trim().to_owned(),
        })
    }
}

#[cfg(target_os = "linux")]
fn write_control(path: &Path, value: &str) -> Result<(), CgroupError> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| cgroup_io_error("cgroup 제어 파일 열기", path, source))?;
    file.write_all(value.as_bytes())
        .map_err(|source| cgroup_io_error("cgroup 값 쓰기", path, source))
}

#[cfg(target_os = "linux")]
fn require_regular_file(path: &Path, capability: &'static str) -> Result<(), CgroupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(cgroup_io_error(
            capability,
            path,
            io::Error::new(io::ErrorKind::Unsupported, "일반 파일이 아닙니다"),
        )),
        Err(source) => Err(cgroup_io_error(capability, path, source)),
    }
}

#[cfg(target_os = "linux")]
fn cgroup_io_error(operation: &'static str, path: &Path, source: io::Error) -> CgroupError {
    CgroupError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn unified_membership_parses_one_v2_path() {
        let path = parse_unified_membership("0::/system.slice/taskcaged.service\n").unwrap();
        assert_eq!(path, PathBuf::from("/system.slice/taskcaged.service"));
    }

    #[test]
    fn unified_membership_rejects_missing_or_ambiguous_paths() {
        assert!(parse_unified_membership("2:cpu:/legacy\n").is_err());
        assert!(parse_unified_membership("0::/a\n0::/b\n").is_err());
        assert!(parse_unified_membership("0::/a (deleted)\n").is_err());
    }

    #[test]
    fn job_ids_cannot_escape_the_jobs_directory() {
        assert!(validate_job_id("01J-task_2").is_ok());
        assert!(validate_job_id("../escape").is_err());
        assert!(validate_job_id("").is_err());
    }

    #[test]
    fn duplicate_job_id_has_a_specific_error() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "taskcage-cgroup-path-test-{}-{sequence}",
            std::process::id()
        ));
        let jobs = root.join("jobs");
        let duplicate = jobs.join("job-same");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&jobs).unwrap();
        fs::create_dir(&duplicate).unwrap();

        let paths = CgroupPaths {
            mount: root.clone(),
            root: root.clone(),
            manager: root.join("manager"),
            jobs: jobs.clone(),
        };
        let error = paths.new_job_path("same").unwrap_err();
        assert!(matches!(error, CgroupPathError::DuplicateJobId { .. }));

        fs::remove_dir(&duplicate).unwrap();
        fs::remove_dir(&jobs).unwrap();
        fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn kernel_event_delta_never_becomes_negative() {
        let baseline = KernelEvents {
            memory_oom: 2,
            memory_oom_kill: 1,
            pids_max: 4,
        };
        let current = KernelEvents {
            memory_oom: 5,
            memory_oom_kill: 2,
            pids_max: 3,
        };

        let delta = current.delta_from(&baseline);
        assert_eq!(delta.memory_oom, 3);
        assert_eq!(delta.memory_oom_kill, 1);
        assert_eq!(delta.pids_max, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn write_and_verify_detects_a_read_back_mismatch() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "taskcage-cgroup-value-test-{}-{sequence}",
            std::process::id()
        ));
        fs::write(&path, "999\n").unwrap();

        let error = write_and_verify(&path, "1").unwrap_err();

        assert!(matches!(error, CgroupError::ValueMismatch { .. }));
        fs::remove_file(path).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_configuration_failure_removes_job_before_target_start() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "taskcage-cgroup-rollback-test-{}-{sequence}",
            std::process::id()
        ));
        let jobs = root.join("jobs");
        fs::create_dir_all(&jobs).unwrap();
        let manager = CgroupManager {
            paths: CgroupPaths {
                mount: root.clone(),
                root: root.clone(),
                manager: root.join("manager"),
                jobs: jobs.clone(),
            },
        };
        let limits = CgroupLimits {
            memory_max_bytes: NonZeroU64::new(1).unwrap(),
            max_processes: NonZeroU64::new(1).unwrap(),
            cpu: CpuLimit {
                quota_micros: NonZeroU64::new(1).unwrap(),
                period_micros: NonZeroU64::new(1).unwrap(),
            },
        };
        let configured_limits = std::cell::Cell::new(0);
        let target_starts = std::cell::Cell::new(0);

        let result = manager
            .create_job_with("partial", limits, |path, _| {
                configured_limits.set(1);
                Err::<(File, KernelEvents), _>(CgroupError::ValueMismatch {
                    path: path.join("pids.max"),
                    expected: "1".to_owned(),
                    actual: "2".to_owned(),
                })
            })
            .map(|_| target_starts.set(target_starts.get() + 1));

        assert!(matches!(result, Err(CgroupError::ValueMismatch { .. })));
        assert_eq!(configured_limits.get(), 1);
        assert_eq!(target_starts.get(), 0);
        assert!(!jobs.join("job-partial").exists());
        fs::remove_dir(&jobs).unwrap();
        fs::remove_dir(&root).unwrap();
    }
}
