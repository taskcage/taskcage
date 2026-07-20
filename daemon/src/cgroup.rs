//! 위임받은 cgroup v2 영역을 찾고, 작업별 제한을 적용하고, 실행 결과를 수집하고,
//! 작업이 끝난 뒤 남은 프로세스와 cgroup을 정리한다.
//!
//! 데몬 자신은 `manager` 하위로 옮기고 실제 작업은 `jobs/job-...` 하위에 둔다.
//! 이렇게 나누어야 상위 cgroup에 필요한 제어기를 켤 수 있고 작업별 정리도 쉬워진다.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::num::{NonZeroU32, NonZeroU64};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use thiserror::Error;
use tokio::time::sleep;

const CGROUP_MOUNT: &str = "/sys/fs/cgroup";
const SELF_CGROUP: &str = "/proc/self/cgroup";
const REQUIRED_CONTROLLERS: [&str; 3] = ["cpu", "memory", "pids"];

#[derive(Debug, Error)]
pub enum CgroupError {
    #[error("{operation} failed for {path:?}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unsupported cgroup environment: {0}")]
    Unsupported(String),
    #[error("invalid job id: {0:?}")]
    InvalidJobId(String),
    #[error("cgroup value mismatch at {path:?}: expected {expected:?}, got {actual:?}")]
    ValueMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("cgroup {path:?} did not become empty within {timeout:?}")]
    EmptyTimeout { path: PathBuf, timeout: Duration },
}

#[derive(Debug, Clone, Copy)]
/// 한 주기 안에서 CPU를 얼마나 오래 사용할 수 있는지 나타낸다.
pub struct CpuLimit {
    /// 한 주기 동안 허용할 CPU 사용 시간이다.
    pub quota_micros: NonZeroU64,
    /// CPU 사용량을 다시 계산하는 주기의 길이다.
    pub period_micros: NonZeroU64,
}

#[derive(Debug, Clone, Copy)]
/// 새 작업 cgroup에 적용할 자원 상한이다.
pub struct CgroupLimits {
    /// 작업 전체가 사용할 수 있는 최대 메모리 크기다.
    pub memory_max_bytes: NonZeroU64,
    /// 작업이 동시에 만들 수 있는 프로세스 수다.
    pub max_processes: NonZeroU32,
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
    /// 작업 시작 전 값과 현재 값의 차이만 구한다.
    ///
    /// 커널 수치는 계속 누적되므로 현재 값만 보면 이번 작업에서 생긴 사건인지 알 수 없다.
    /// 수치가 예상과 다르게 작아져도 음수가 되지 않도록 포화 뺄셈을 사용한다.
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
/// 작업이 사용한 자원과 제한 초과 사건을 호출자에게 돌려줄 형태로 모은 값이다.
pub struct JobStats {
    pub cpu_usage_micros: u64,
    pub memory_current_bytes: u64,
    pub memory_peak_bytes: u64,
    pub current_processes: u64,
    pub peak_processes: Option<u64>,
    pub event_delta: KernelEvents,
}

#[derive(Debug)]
/// 위임받은 cgroup 영역과 작업을 만들 위치를 관리한다.
pub struct CgroupManager {
    root: PathBuf,
    jobs: PathBuf,
}

impl CgroupManager {
    /// 현재 데몬이 속한 cgroup을 읽어 systemd가 위임한 실제 경로를 찾는다.
    pub fn discover_and_initialize() -> Result<Self, CgroupError> {
        let membership = read_to_string(Path::new(SELF_CGROUP), "read self cgroup")?;
        let relative = parse_unified_membership(&membership)?;
        let root = Path::new(CGROUP_MOUNT).join(relative.strip_prefix("/").unwrap_or(&relative));
        Self::initialize(root)
    }

    pub fn initialize(root: impl AsRef<Path>) -> Result<Self, CgroupError> {
        let supplied_root = root.as_ref();
        // `..`이나 심볼릭 링크가 섞인 경로를 그대로 사용하지 않고 실제 경로 하나로 고정한다.
        let root = fs::canonicalize(supplied_root)
            .map_err(|source| io_error("canonicalize cgroup root", supplied_root, source))?;

        // 이름만 비슷한 일반 디렉터리를 잘못 제어하지 않도록 cgroup v2 파일 시스템인지 확인한다.
        ensure_cgroup2_filesystem(&root)?;
        require_regular_file(&root.join("cgroup.controllers"), "cgroup v2")?;
        require_regular_file(&root.join("cgroup.procs"), "cgroup process file")?;

        let available = read_word_set(&root.join("cgroup.controllers"))?;
        ensure_controllers(&available, &root)?;

        let manager = root.join("manager");
        let jobs = root.join("jobs");
        create_dir_if_missing(&manager)?;
        create_dir_if_missing(&jobs)?;

        // cgroup v2는 프로세스가 들어 있는 cgroup에서 하위 제어기를 켤 수 없다.
        // 따라서 데몬을 `manager`로 먼저 옮긴 다음 상위 영역의 제어기를 활성화한다.
        write_control(
            &manager.join("cgroup.procs"),
            &format!("{}\n", std::process::id()),
        )?;
        enable_required_controllers(&root)?;
        // `jobs` 아래에 실제 작업 cgroup을 만들 수 있도록 같은 제어기를 한 단계 더 내려준다.
        enable_required_controllers(&jobs)?;

        Ok(Self { root, jobs })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create_job(&self, job_id: &str, limits: CgroupLimits) -> Result<JobCgroup, CgroupError> {
        // 작업 식별자가 경로 구분자를 포함하면 지정된 영역 밖으로 벗어날 수 있으므로 먼저 막는다.
        validate_job_id(job_id)?;
        let path = self.jobs.join(format!("job-{job_id}"));
        fs::create_dir(&path).map_err(|source| io_error("create job cgroup", &path, source))?;

        let configured = (|| {
            // 제한값은 쓰는 데 성공한 것만으로 충분하지 않다. 커널이 받아들인 값을 다시 읽어
            // 요청한 값과 같은지 확인하고, 다르면 보호되지 않은 작업을 시작하지 않는다.
            write_and_verify(
                &path.join("memory.max"),
                &limits.memory_max_bytes.get().to_string(),
            )?;
            let oom_group = path.join("memory.oom.group");
            if oom_group.exists() {
                // 메모리가 부족할 때 일부 프로세스만 남지 않도록 작업 cgroup 전체를 한 단위로 다룬다.
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
            require_regular_file(&path.join("cgroup.kill"), "cgroup.kill")?;
            // 이 디렉터리 파일 설명자는 `clone3`에 넘겨 새 프로세스를 처음부터 이 cgroup 안에 둔다.
            let directory =
                File::open(&path).map_err(|source| io_error("open job cgroup", &path, source))?;
            // 사건 수치는 누적값이므로 작업 시작 전 값을 저장해 두었다가 종료 시 차이를 구한다.
            let baseline = read_kernel_events(&path)?;
            Ok((directory, baseline))
        })();

        match configured {
            Ok((directory, baseline)) => Ok(JobCgroup {
                job_id: job_id.to_owned(),
                path,
                directory,
                baseline,
                cleaned: false,
            }),
            Err(error) => {
                // 설정 도중 하나라도 실패하면 덜 만들어진 cgroup을 남기지 않는다.
                let _ = fs::remove_dir(&path);
                Err(error)
            }
        }
    }
}

#[derive(Debug)]
/// 실행 중인 작업 하나에 대응하는 cgroup이다.
pub struct JobCgroup {
    job_id: String,
    path: PathBuf,
    directory: File,
    baseline: KernelEvents,
    cleaned: bool,
}

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

    /// 작업 또는 그 자식 cgroup에 프로세스가 하나라도 남아 있는지 확인한다.
    pub fn is_populated(&self) -> Result<bool, CgroupError> {
        let events = read_flat_keys(&self.path.join("cgroup.events"))?;
        Ok(events.get("populated").copied().unwrap_or(0) != 0)
    }

    /// 시작한 프로세스가 실제로 이 작업 cgroup에 들어왔는지 확인한다.
    pub fn contains_pid(&self, pid: libc::pid_t) -> Result<bool, CgroupError> {
        let processes = read_to_string(&self.path.join("cgroup.procs"), "read job processes")?;
        Ok(processes
            .lines()
            .filter_map(|value| value.parse::<libc::pid_t>().ok())
            .any(|value| value == pid))
    }

    /// 대표 프로세스 하나가 아니라 이 cgroup 아래의 모든 프로세스를 종료한다.
    pub fn kill_all(&self) -> Result<(), CgroupError> {
        write_control(&self.path.join("cgroup.kill"), "1\n")
    }

    /// 커널이 모든 프로세스를 정리했다고 알릴 때까지 짧게 반복해서 확인한다.
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

    /// cgroup을 지우기 전에 커널이 기록한 최종 자원 사용량을 모은다.
    pub fn stats(&self) -> Result<JobStats, CgroupError> {
        let cpu = read_flat_keys(&self.path.join("cpu.stat"))?;
        let current_events = read_kernel_events(&self.path)?;
        let memory_current = read_u64(&self.path.join("memory.current"))?;
        let memory_peak =
            read_optional_u64(&self.path.join("memory.peak"))?.unwrap_or(memory_current);
        let current_processes = read_u64(&self.path.join("pids.current"))?;
        let peak_processes = read_optional_u64(&self.path.join("pids.peak"))?;

        Ok(JobStats {
            cpu_usage_micros: cpu.get("usage_usec").copied().unwrap_or(0),
            memory_current_bytes: memory_current,
            memory_peak_bytes: memory_peak,
            current_processes,
            peak_processes,
            event_delta: current_events.delta_from(&self.baseline),
        })
    }

    /// 남은 프로세스를 끝내고 통계를 읽은 뒤 작업 cgroup을 제거한다.
    pub async fn finish(mut self, timeout: Duration) -> Result<JobStats, CgroupError> {
        if self.is_populated()? {
            self.kill_all()?;
        }
        self.wait_empty(timeout).await?;
        // 디렉터리를 지우면 통계 파일도 사라지므로 반드시 먼저 읽는다.
        let stats = self.stats()?;
        fs::remove_dir(&self.path)
            .map_err(|source| io_error("remove job cgroup", &self.path, source))?;
        self.cleaned = true;
        Ok(stats)
    }
}

impl Drop for JobCgroup {
    fn drop(&mut self) {
        if self.cleaned || !self.path.exists() {
            return;
        }
        // 정상 정리 경로가 오류로 중단되어도 프로세스를 남기지 않도록 마지막 방어선을 둔다.
        // `Drop`에서는 오류를 돌려줄 곳이 없으므로 가능한 정리만 시도한다.
        let _ = write_control(&self.path.join("cgroup.kill"), "1\n");
        let _ = fs::remove_dir(&self.path);
    }
}

fn parse_unified_membership(contents: &str) -> Result<PathBuf, CgroupError> {
    // cgroup v2의 `/proc/self/cgroup` 항목은 `0::<경로>` 형태다.
    let mut paths = contents.lines().filter_map(|line| line.strip_prefix("0::"));
    let path = paths.next().ok_or_else(|| {
        CgroupError::Unsupported("/proc/self/cgroup has no unified v2 entry".to_owned())
    })?;
    if paths.next().is_some() || path.is_empty() || path.contains(" (deleted)") {
        return Err(CgroupError::Unsupported(
            "ambiguous or deleted unified cgroup membership".to_owned(),
        ));
    }
    Ok(PathBuf::from(path))
}

fn validate_job_id(job_id: &str) -> Result<(), CgroupError> {
    let valid = !job_id.is_empty()
        && job_id.len() <= 64
        && job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(CgroupError::InvalidJobId(job_id.to_owned()))
    }
}

fn enable_required_controllers(path: &Path) -> Result<(), CgroupError> {
    let available = read_word_set(&path.join("cgroup.controllers"))?;
    ensure_controllers(&available, path)?;
    // 앞에 `+`를 붙이면 이 cgroup의 자식들이 해당 제어기를 사용할 수 있게 된다.
    write_control(&path.join("cgroup.subtree_control"), "+cpu +memory +pids\n")?;
    let enabled = read_word_set(&path.join("cgroup.subtree_control"))?;
    ensure_controllers(&enabled, path)
}

fn ensure_controllers(controllers: &BTreeSet<String>, path: &Path) -> Result<(), CgroupError> {
    let missing: Vec<_> = REQUIRED_CONTROLLERS
        .iter()
        .filter(|controller| !controllers.contains(**controller))
        .copied()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CgroupError::Unsupported(format!(
            "missing controllers at {}: {}",
            path.display(),
            missing.join(", ")
        )))
    }
}

fn read_kernel_events(path: &Path) -> Result<KernelEvents, CgroupError> {
    let memory = read_flat_keys(&path.join("memory.events.local"))?;
    let pids = read_flat_keys(&path.join("pids.events"))?;
    Ok(KernelEvents {
        memory_oom: memory.get("oom").copied().unwrap_or(0),
        memory_oom_kill: memory.get("oom_kill").copied().unwrap_or(0),
        pids_max: pids.get("max").copied().unwrap_or(0),
    })
}

fn read_flat_keys(path: &Path) -> Result<BTreeMap<String, u64>, CgroupError> {
    let contents = read_to_string(path, "read cgroup key-value file")?;
    let mut values = BTreeMap::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else {
            continue;
        };
        let Some(raw_value) = fields.next() else {
            continue;
        };
        if fields.next().is_some() {
            continue;
        }
        if let Ok(value) = raw_value.parse::<u64>() {
            values.insert(key.to_owned(), value);
        }
    }
    Ok(values)
}

fn read_word_set(path: &Path) -> Result<BTreeSet<String>, CgroupError> {
    Ok(read_to_string(path, "read cgroup controller file")?
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

fn read_u64(path: &Path) -> Result<u64, CgroupError> {
    let value = read_to_string(path, "read cgroup number")?;
    value.trim().parse::<u64>().map_err(|source| {
        io_error(
            "parse cgroup number",
            path,
            io::Error::new(io::ErrorKind::InvalidData, source),
        )
    })
}

fn read_optional_u64(path: &Path) -> Result<Option<u64>, CgroupError> {
    if path.exists() {
        read_u64(path).map(Some)
    } else {
        Ok(None)
    }
}

fn read_to_string(path: &Path, operation: &'static str) -> Result<String, CgroupError> {
    fs::read_to_string(path).map_err(|source| io_error(operation, path, source))
}

fn write_and_verify(path: &Path, value: &str) -> Result<(), CgroupError> {
    write_control(path, &format!("{value}\n"))?;
    // 커널 제어 파일은 일반 파일과 달라 쓰기 성공만으로 적용 여부를 단정할 수 없다.
    let actual = read_to_string(path, "verify cgroup value")?;
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

fn write_control(path: &Path, value: &str) -> Result<(), CgroupError> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| io_error("open cgroup control", path, source))?;
    file.write_all(value.as_bytes())
        .map_err(|source| io_error("write cgroup control", path, source))
}

fn create_dir_if_missing(path: &Path) -> Result<(), CgroupError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(source) => Err(io_error("create cgroup directory", path, source)),
    }
}

fn require_regular_file(path: &Path, capability: &str) -> Result<(), CgroupError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(CgroupError::Unsupported(format!(
            "{capability} is unavailable at {}",
            path.display()
        )))
    }
}

#[cfg(target_os = "linux")]
fn ensure_cgroup2_filesystem(path: &Path) -> Result<(), CgroupError> {
    // cgroup v2 파일 시스템은 커널이 정한 고유 번호를 가진다.
    // `statfs`로 그 번호를 확인해 잘못된 경로에 제어 파일을 만들지 않도록 한다.
    const CGROUP2_SUPER_MAGIC: i128 = 0x6367_7270;
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| CgroupError::Unsupported("cgroup root contains a NUL byte".to_owned()))?;
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    let result = unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) };
    if result == -1 {
        return Err(CgroupError::Unsupported(format!(
            "statfs failed for cgroup root: {}",
            io::Error::last_os_error()
        )));
    }
    let stats = unsafe { stats.assume_init() };
    if stats.f_type as i128 == CGROUP2_SUPER_MAGIC {
        Ok(())
    } else {
        Err(CgroupError::Unsupported(
            "configured root is not a cgroup v2 filesystem".to_owned(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn ensure_cgroup2_filesystem(_path: &Path) -> Result<(), CgroupError> {
    Err(CgroupError::Unsupported(
        "cgroup v2 execution requires Linux".to_owned(),
    ))
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> CgroupError {
    CgroupError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unified_membership() {
        let path = parse_unified_membership("0::/system.slice/taskcaged.service\n").unwrap();
        assert_eq!(path, PathBuf::from("/system.slice/taskcaged.service"));
    }

    #[test]
    fn rejects_missing_unified_membership() {
        assert!(parse_unified_membership("2:cpu:/legacy\n").is_err());
    }

    #[test]
    fn validates_job_ids() {
        assert!(validate_job_id("01J-task_2").is_ok());
        assert!(validate_job_id("../escape").is_err());
        assert!(validate_job_id("").is_err());
    }

    #[test]
    fn computes_saturating_event_delta() {
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
}
