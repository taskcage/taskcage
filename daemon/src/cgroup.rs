//! Delegated cgroup v2 discovery, limit application, evidence and cleanup.

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
pub struct CpuLimit {
    pub quota_micros: NonZeroU64,
    pub period_micros: NonZeroU64,
}

#[derive(Debug, Clone, Copy)]
pub struct CgroupLimits {
    pub memory_max_bytes: NonZeroU64,
    pub max_processes: NonZeroU32,
    pub cpu: CpuLimit,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelEvents {
    pub memory_oom: u64,
    pub memory_oom_kill: u64,
    pub pids_max: u64,
}

impl KernelEvents {
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
pub struct JobStats {
    pub cpu_usage_micros: u64,
    pub memory_current_bytes: u64,
    pub memory_peak_bytes: u64,
    pub current_processes: u64,
    pub peak_processes: Option<u64>,
    pub event_delta: KernelEvents,
}

#[derive(Debug)]
pub struct CgroupManager {
    root: PathBuf,
    jobs: PathBuf,
}

impl CgroupManager {
    pub fn discover_and_initialize() -> Result<Self, CgroupError> {
        let membership = read_to_string(Path::new(SELF_CGROUP), "read self cgroup")?;
        let relative = parse_unified_membership(&membership)?;
        let root = Path::new(CGROUP_MOUNT).join(relative.strip_prefix("/").unwrap_or(&relative));
        Self::initialize(root)
    }

    pub fn initialize(root: impl AsRef<Path>) -> Result<Self, CgroupError> {
        let supplied_root = root.as_ref();
        let root = fs::canonicalize(supplied_root)
            .map_err(|source| io_error("canonicalize cgroup root", supplied_root, source))?;

        ensure_cgroup2_filesystem(&root)?;
        require_regular_file(&root.join("cgroup.controllers"), "cgroup v2")?;
        require_regular_file(&root.join("cgroup.procs"), "cgroup process file")?;

        let available = read_word_set(&root.join("cgroup.controllers"))?;
        ensure_controllers(&available, &root)?;

        let manager = root.join("manager");
        let jobs = root.join("jobs");
        create_dir_if_missing(&manager)?;
        create_dir_if_missing(&jobs)?;

        write_control(
            &manager.join("cgroup.procs"),
            &format!("{}\n", std::process::id()),
        )?;
        enable_required_controllers(&root)?;
        enable_required_controllers(&jobs)?;

        Ok(Self { root, jobs })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create_job(&self, job_id: &str, limits: CgroupLimits) -> Result<JobCgroup, CgroupError> {
        validate_job_id(job_id)?;
        let path = self.jobs.join(format!("job-{job_id}"));
        fs::create_dir(&path).map_err(|source| io_error("create job cgroup", &path, source))?;

        let configured = (|| {
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
            require_regular_file(&path.join("cgroup.kill"), "cgroup.kill")?;
            let directory =
                File::open(&path).map_err(|source| io_error("open job cgroup", &path, source))?;
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
                let _ = fs::remove_dir(&path);
                Err(error)
            }
        }
    }
}

#[derive(Debug)]
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

    pub fn is_populated(&self) -> Result<bool, CgroupError> {
        let events = read_flat_keys(&self.path.join("cgroup.events"))?;
        Ok(events.get("populated").copied().unwrap_or(0) != 0)
    }

    pub fn contains_pid(&self, pid: libc::pid_t) -> Result<bool, CgroupError> {
        let processes = read_to_string(&self.path.join("cgroup.procs"), "read job processes")?;
        Ok(processes
            .lines()
            .filter_map(|value| value.parse::<libc::pid_t>().ok())
            .any(|value| value == pid))
    }

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

    pub async fn finish(mut self, timeout: Duration) -> Result<JobStats, CgroupError> {
        if self.is_populated()? {
            self.kill_all()?;
        }
        self.wait_empty(timeout).await?;
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
        let _ = write_control(&self.path.join("cgroup.kill"), "1\n");
        let _ = fs::remove_dir(&self.path);
    }
}

fn parse_unified_membership(contents: &str) -> Result<PathBuf, CgroupError> {
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
