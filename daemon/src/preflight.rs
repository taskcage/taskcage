//! 사용자 프로그램을 실행하기 전에 Linux cgroup 기능과 권한을 모두 확인한다.
//!
//! 하나라도 확인하지 못하면 성공 보고서를 만들지 않는다. 이후 실행 기능은 이 보고서를
//! 받은 경로에서만 시작하도록 연결해 보호되지 않은 실행으로 넘어가는 우회 경로를 막는다.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use std::fs::{self, File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::mem::size_of;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(any(target_os = "linux", test))]
use std::path::Path;

use serde::Serialize;
use thiserror::Error;

use crate::cgroup::CgroupPathError;
#[cfg(target_os = "linux")]
use crate::cgroup::CgroupPaths;

#[cfg(any(target_os = "linux", test))]
const REQUIRED_CONTROLLERS: [&str; 3] = ["cpu", "memory", "pids"];
#[cfg(target_os = "linux")]
const REQUIRED_FILES: [(&str, &str); 6] = [
    ("cgroup 종료", "cgroup.kill"),
    ("cgroup 상태", "cgroup.events"),
    ("메모리 사건", "memory.events.local"),
    ("프로세스 수 사건", "pids.events"),
    ("CPU 사용량", "cpu.stat"),
    ("프로세스 이동", "cgroup.procs"),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub delegated_root: PathBuf,
    pub manager_cgroup: PathBuf,
    pub controllers: BTreeSet<String>,
    pub cgroup_kill: bool,
    pub event_and_stat_files: bool,
    pub delegated_root_writable: bool,
    pub manager_membership_verified: bool,
    pub atomic_entry_supported: bool,
}

/// 모든 필수 검사를 실제로 통과했음을 나타내는 값이다.
///
/// 필드는 외부에 공개하지 않아 단순한 성공 보고서를 직접 만들어 실행 권한처럼 사용할 수
/// 없다. 작업 실행기는 이 값을 넘겨받아야만 cgroup을 만들 수 있다.
#[derive(Debug)]
pub struct VerifiedEnvironment {
    report: CapabilityReport,
    #[cfg(target_os = "linux")]
    paths: CgroupPaths,
}

impl VerifiedEnvironment {
    pub fn report(&self) -> &CapabilityReport {
        &self.report
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn into_paths(self) -> CgroupPaths {
        self.paths
    }
}

#[derive(Debug, Error)]
pub enum PreflightError {
    #[error(transparent)]
    CgroupPath(#[from] CgroupPathError),
    #[error("TaskCage 실행에는 Linux cgroup v2가 필요합니다")]
    UnsupportedPlatform,
    #[error("지정한 경로가 cgroup v2 파일 시스템이 아닙니다: {path:?}")]
    NotCgroupV2 { path: PathBuf },
    #[error("필수 cgroup 제어기가 없습니다: {controller}, 확인 경로 {path:?}")]
    MissingController { controller: String, path: PathBuf },
    #[error("{capability}에 필요한 파일이 없습니다: {path:?}")]
    MissingFile {
        capability: &'static str,
        path: PathBuf,
    },
    #[error("cgroup 제어 파일에 쓸 권한이 없습니다: {path:?}")]
    NotWritable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("manager cgroup을 새로 만들 수 없습니다: {path:?}")]
    ManagerCreate {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("manager cgroup이 이미 있어 안전하게 재사용할 수 없습니다: {path:?}")]
    ManagerAlreadyExists { path: PathBuf },
    #[error("clone3의 원자적 cgroup 진입을 사용할 수 없습니다")]
    AtomicEntryUnsupported {
        #[source]
        source: io::Error,
    },
    #[error("사전 검사 실패 뒤 manager cgroup 정리도 실패했습니다: 검사={cause}; 정리={cleanup}")]
    RollbackFailed { cause: String, cleanup: String },
    #[error("{operation} 작업이 {path:?}에서 실패했습니다")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// 실제 환경 검사와 단위 시험용 가짜 검사를 같은 실행 차단 경로에 연결한다.
pub trait CapabilityProbe {
    fn check(&self) -> Result<VerifiedEnvironment, PreflightError>;
}

#[derive(Debug, Clone, Default)]
pub struct SystemProbe {
    root_override: Option<PathBuf>,
}

impl SystemProbe {
    pub fn from_environment() -> Self {
        Self {
            root_override: std::env::var_os("TASKCAGE_CGROUP_ROOT").map(PathBuf::from),
        }
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root_override: Some(root.into()),
        }
    }
}

impl CapabilityProbe for SystemProbe {
    fn check(&self) -> Result<VerifiedEnvironment, PreflightError> {
        #[cfg(target_os = "linux")]
        {
            check_linux(self.root_override.as_deref())
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = &self.root_override;
            Err(PreflightError::UnsupportedPlatform)
        }
    }
}

/// 검사에 성공했을 때만 뒤의 작업을 호출한다.
///
/// 이후 사용자 프로그램 실행 경로도 이 함수 또는 같은 의미의 소유권 토큰을 통과해야 한다.
pub fn with_verified_environment<P, F, T>(probe: &P, action: F) -> Result<T, PreflightError>
where
    P: CapabilityProbe,
    F: FnOnce(VerifiedEnvironment) -> T,
{
    let environment = probe.check()?;
    Ok(action(environment))
}

#[cfg(target_os = "linux")]
fn check_linux(root_override: Option<&Path>) -> Result<VerifiedEnvironment, PreflightError> {
    let paths = CgroupPaths::resolve(root_override)?;
    ensure_cgroup2_filesystem(paths.root())?;
    let controllers = read_words(&paths.root().join("cgroup.controllers"))?;
    ensure_required_controllers(&controllers, paths.root())?;
    ensure_required_files(paths.root())?;
    require_writable(&paths.root().join("cgroup.procs"))?;
    require_writable(&paths.root().join("cgroup.subtree_control"))?;

    create_manager(paths.manager())?;
    let original_subtree = read_words(&paths.root().join("cgroup.subtree_control"))?;
    let mut moved_to_manager = false;

    let result: Result<VerifiedEnvironment, PreflightError> = (|| {
        require_regular_file(
            "manager 프로세스 이동",
            &paths.manager().join("cgroup.procs"),
        )?;
        require_regular_file("manager 전체 종료", &paths.manager().join("cgroup.kill"))?;

        write_control(
            &paths.manager().join("cgroup.procs"),
            &format!("{}\n", std::process::id()),
        )?;
        moved_to_manager = true;
        paths.verify_manager_membership()?;

        let manager_directory = File::open(paths.manager())
            .map_err(|source| io_error("manager cgroup 열기", paths.manager(), source))?;
        probe_atomic_entry(manager_directory.as_raw_fd())?;
        enable_required_controllers(paths.root())?;

        Ok(VerifiedEnvironment {
            report: CapabilityReport {
                delegated_root: paths.root().to_path_buf(),
                manager_cgroup: paths.manager().to_path_buf(),
                controllers,
                cgroup_kill: true,
                event_and_stat_files: true,
                delegated_root_writable: true,
                manager_membership_verified: true,
                atomic_entry_supported: true,
            },
            paths: paths.clone(),
        })
    })();

    match result {
        Ok(report) => Ok(report),
        Err(cause) => {
            if let Err(cleanup) = rollback_manager(&paths, &original_subtree, moved_to_manager) {
                return Err(PreflightError::RollbackFailed {
                    cause: cause.to_string(),
                    cleanup: cleanup.to_string(),
                });
            }
            Err(cause)
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn ensure_required_controllers(
    controllers: &BTreeSet<String>,
    path: &Path,
) -> Result<(), PreflightError> {
    for required in REQUIRED_CONTROLLERS {
        if !controllers.contains(required) {
            return Err(PreflightError::MissingController {
                controller: required.to_owned(),
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_required_files(root: &Path) -> Result<(), PreflightError> {
    require_regular_file("사용 가능한 제어기 목록", &root.join("cgroup.controllers"))?;
    require_regular_file("하위 제어기 설정", &root.join("cgroup.subtree_control"))?;
    for (capability, file) in REQUIRED_FILES {
        require_regular_file(capability, &root.join(file))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_regular_file(capability: &'static str, path: &Path) -> Result<(), PreflightError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(PreflightError::MissingFile {
            capability,
            path: path.to_path_buf(),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Err(PreflightError::MissingFile {
                capability,
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(io_error("cgroup 파일 확인", path, source)),
    }
}

#[cfg(target_os = "linux")]
fn require_writable(path: &Path) -> Result<(), PreflightError> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| PreflightError::NotWritable {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(target_os = "linux")]
fn create_manager(path: &Path) -> Result<(), PreflightError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            Err(PreflightError::ManagerAlreadyExists {
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(PreflightError::ManagerCreate {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(target_os = "linux")]
fn enable_required_controllers(root: &Path) -> Result<(), PreflightError> {
    let path = root.join("cgroup.subtree_control");
    let enabled = read_words(&path)?;
    let missing: Vec<_> = REQUIRED_CONTROLLERS
        .iter()
        .filter(|controller| !enabled.contains(**controller))
        .map(|controller| format!("+{controller}"))
        .collect();
    if !missing.is_empty() {
        write_control(&path, &format!("{}\n", missing.join(" ")))?;
    }
    let enabled = read_words(&path)?;
    ensure_required_controllers(&enabled, root)
}

#[cfg(target_os = "linux")]
fn rollback_manager(
    paths: &CgroupPaths,
    original_subtree: &BTreeSet<String>,
    moved_to_manager: bool,
) -> Result<(), PreflightError> {
    if moved_to_manager {
        let enabled = read_words(&paths.root().join("cgroup.subtree_control"))?;
        let added: Vec<_> = REQUIRED_CONTROLLERS
            .iter()
            .filter(|controller| {
                enabled.contains(**controller) && !original_subtree.contains(**controller)
            })
            .map(|controller| format!("-{controller}"))
            .collect();
        if !added.is_empty() {
            write_control(
                &paths.root().join("cgroup.subtree_control"),
                &format!("{}\n", added.join(" ")),
            )?;
        }
        write_control(
            &paths.root().join("cgroup.procs"),
            &format!("{}\n", std::process::id()),
        )?;
    }
    fs::remove_dir(paths.manager())
        .map_err(|source| io_error("manager cgroup 제거", paths.manager(), source))
}

#[cfg(target_os = "linux")]
fn read_words(path: &Path) -> Result<BTreeSet<String>, PreflightError> {
    let contents =
        fs::read_to_string(path).map_err(|source| io_error("cgroup 값 읽기", path, source))?;
    Ok(contents.split_whitespace().map(str::to_owned).collect())
}

#[cfg(target_os = "linux")]
fn write_control(path: &Path, value: &str) -> Result<(), PreflightError> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| PreflightError::NotWritable {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(value.as_bytes())
        .map_err(|source| io_error("cgroup 값 쓰기", path, source))
}

#[cfg(target_os = "linux")]
fn ensure_cgroup2_filesystem(path: &Path) -> Result<(), PreflightError> {
    const CGROUP2_SUPER_MAGIC: i128 = 0x6367_7270;
    let path_bytes = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
        PreflightError::NotCgroupV2 {
            path: path.to_path_buf(),
        }
    })?;
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    let result = unsafe { libc::statfs(path_bytes.as_ptr(), stats.as_mut_ptr()) };
    if result == -1 {
        return Err(io_error(
            "cgroup 파일 시스템 확인",
            path,
            io::Error::last_os_error(),
        ));
    }
    let stats = unsafe { stats.assume_init() };
    if stats.f_type as i128 == CGROUP2_SUPER_MAGIC {
        Ok(())
    } else {
        Err(PreflightError::NotCgroupV2 {
            path: path.to_path_buf(),
        })
    }
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

#[cfg(target_os = "linux")]
fn probe_atomic_entry(cgroup_fd: RawFd) -> Result<(), PreflightError> {
    const CLONE_INTO_CGROUP: u64 = 0x0002_0000_0000;
    let args = CloneArgs {
        flags: CLONE_INTO_CGROUP,
        exit_signal: libc::SIGCHLD as u64,
        cgroup: cgroup_fd as u64,
        ..CloneArgs::default()
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &args as *const CloneArgs,
            size_of::<CloneArgs>(),
        )
    };
    if result == -1 {
        return Err(PreflightError::AtomicEntryUnsupported {
            source: io::Error::last_os_error(),
        });
    }
    if result == 0 {
        // 이 자식은 사용자 프로그램이 아니다. cgroup 진입 기능만 확인하고 즉시 끝낸다.
        // clone3 뒤에는 메모리 할당, 잠금, 로그를 사용하지 않고 `_exit`만 호출한다.
        unsafe { libc::_exit(0) };
    }

    let pid = result as libc::pid_t;
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        if waited == pid {
            break;
        }
        if waited == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(PreflightError::AtomicEntryUnsupported {
            source: io::Error::last_os_error(),
        });
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else {
        Err(PreflightError::AtomicEntryUnsupported {
            source: io::Error::other("내부 clone3 검사 프로세스가 정상 종료하지 않았습니다"),
        })
    }
}

#[cfg(target_os = "linux")]
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> PreflightError {
    PreflightError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_controller_has_a_specific_error() {
        let controllers = BTreeSet::from(["cpu".to_owned(), "memory".to_owned()]);
        let error = ensure_required_controllers(&controllers, Path::new("/delegated")).unwrap_err();

        match error {
            PreflightError::MissingController { controller, .. } => {
                assert_eq!(controller, "pids");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn action_runs_only_after_a_successful_report() {
        struct PassingProbe;
        impl CapabilityProbe for PassingProbe {
            fn check(&self) -> Result<VerifiedEnvironment, PreflightError> {
                let root = PathBuf::from("/delegated");
                Ok(VerifiedEnvironment {
                    report: CapabilityReport {
                        delegated_root: root.clone(),
                        manager_cgroup: root.join("manager"),
                        controllers: BTreeSet::from([
                            "cpu".to_owned(),
                            "memory".to_owned(),
                            "pids".to_owned(),
                        ]),
                        cgroup_kill: true,
                        event_and_stat_files: true,
                        delegated_root_writable: true,
                        manager_membership_verified: true,
                        atomic_entry_supported: true,
                    },
                    #[cfg(target_os = "linux")]
                    paths: CgroupPaths::for_test(root),
                })
            }
        }

        let value = with_verified_environment(&PassingProbe, |_| 42).unwrap();
        assert_eq!(value, 42);
    }
}
