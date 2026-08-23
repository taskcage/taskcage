//! cgroup 위임 경로를 검증하고 작업 cgroup 생성을 조립한다.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use thiserror::Error;

use crate::cleanup_fault::CleanupFaults;
use crate::preflight::VerifiedEnvironment;

use super::events::{KernelEvents, read_word_set};
use super::limits::{
    CgroupLimits, VerifiedCgroupLimits, configure_job, configure_job_with_read_back_mismatch,
};
use super::task_group::JobCgroup;

pub const DEFAULT_CGROUP_MOUNT: &str = "/sys/fs/cgroup";
pub const SELF_CGROUP_FILE: &str = "/proc/self/cgroup";
#[cfg(target_os = "linux")]
const CGROUP_ROOT_ENV: &str = "TASKCAGE_CGROUP_ROOT";
#[cfg(target_os = "linux")]
const DELEGATE_SUBGROUP_ENV: &str = "TASKCAGE_CGROUP_DELEGATE_SUBGROUP";
const MANAGER_CGROUP_NAME: &str = "manager";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupCgroupPlacement {
    DelegatedRoot,
    ExistingManager,
}

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
    #[error(
        "현재 cgroup {actual:?}은 기존 TaskCage manager일 수 있어 부모 위임 경로를 명시해야 합니다"
    )]
    ParentRootRequiredForManager { actual: PathBuf },
    #[error("지원하지 않는 systemd delegate subgroup입니다: {configured:?}")]
    UnsupportedDelegateSubgroup { configured: OsString },
    #[error(
        "systemd delegate subgroup membership이 다릅니다: 설정 {configured:?}, 실제 {actual:?}"
    )]
    DelegateSubgroupMismatch {
        configured: OsString,
        actual: PathBuf,
    },
    #[error("manager 이동 결과가 다릅니다: 예상 {expected:?}, 실제 {actual:?}")]
    ManagerMembershipMismatch { expected: PathBuf, actual: PathBuf },
    #[error("작업 식별자 형식이 올바르지 않습니다: {0:?}")]
    InvalidJobId(String),
    #[error("같은 작업 식별자의 cgroup이 이미 있습니다: {job_id:?}, 경로 {path:?}")]
    DuplicateJobId { job_id: String, path: PathBuf },
}

#[cfg(target_os = "linux")]
/// 명시 root가 없으면 systemd가 배치한 manager membership의 부모를 위임 root로 사용한다.
pub fn configured_root_from_environment() -> Result<Option<PathBuf>, CgroupPathError> {
    if let Some(root) = std::env::var_os(CGROUP_ROOT_ENV) {
        return Ok(Some(PathBuf::from(root)));
    }

    let Some(subgroup) = std::env::var_os(DELEGATE_SUBGROUP_ENV) else {
        return Ok(None);
    };
    let membership = read_unified_membership()?;
    infer_delegate_root(Path::new(DEFAULT_CGROUP_MOUNT), &membership, &subgroup).map(Some)
}

#[cfg(any(target_os = "linux", test))]
fn infer_delegate_root(
    mount: &Path,
    membership: &Path,
    subgroup: &OsStr,
) -> Result<PathBuf, CgroupPathError> {
    if subgroup != OsStr::new(MANAGER_CGROUP_NAME) {
        return Err(CgroupPathError::UnsupportedDelegateSubgroup {
            configured: subgroup.to_os_string(),
        });
    }
    if membership.file_name() != Some(subgroup) {
        return Err(CgroupPathError::DelegateSubgroupMismatch {
            configured: subgroup.to_os_string(),
            actual: membership.to_path_buf(),
        });
    }
    let Some(parent) = membership.parent() else {
        return Err(CgroupPathError::DelegateSubgroupMismatch {
            configured: subgroup.to_os_string(),
            actual: membership.to_path_buf(),
        });
    };
    let relative = parent.strip_prefix("/").unwrap_or(parent);
    Ok(mount.join(relative))
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
    #[error(
        "cgroup 값 재확인 실패 뒤 작업 cgroup 제거도 실패했습니다: 재확인={mismatch}; 제거={cleanup}"
    )]
    ReadBackRollbackUncertain {
        mismatch: Box<CgroupError>,
        cleanup: String,
    },
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
        let (paths, placement) = Self::resolve_with_policy(root_override, false)?;
        if placement == StartupCgroupPlacement::DelegatedRoot {
            Ok(paths)
        } else {
            Err(CgroupPathError::ConfiguredRootMismatch {
                configured: paths.root,
                actual: paths.manager,
            })
        }
    }

    /// 시작 복구에서만 설정 root 또는 그 바로 아래 manager membership을 구분한다.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn resolve_startup(
        root_override: Option<&Path>,
    ) -> Result<(Self, StartupCgroupPlacement), CgroupPathError> {
        Self::resolve_with_policy(root_override, true)
    }

    fn resolve_with_policy(
        root_override: Option<&Path>,
        require_parent_for_manager: bool,
    ) -> Result<(Self, StartupCgroupPlacement), CgroupPathError> {
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
        let manager = root.join(MANAGER_CGROUP_NAME);
        let placement = classify_startup_placement(
            root_override,
            &root,
            &actual_root,
            require_parent_for_manager,
        )?;

        Ok((
            Self {
                manager,
                jobs: root.join("jobs"),
                mount,
                root,
            },
            placement,
        ))
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

    #[cfg(target_os = "linux")]
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

fn classify_startup_placement(
    root_override: Option<&Path>,
    root: &Path,
    actual_root: &Path,
    require_parent_for_manager: bool,
) -> Result<StartupCgroupPlacement, CgroupPathError> {
    if root == actual_root {
        // 기존 manager를 새 위임 root로 오인하면 형제 jobs의 잔여 실행을 건너뛸 수 있다.
        if require_parent_for_manager
            && actual_root
                .file_name()
                .is_some_and(|name| name == MANAGER_CGROUP_NAME)
        {
            return Err(CgroupPathError::ParentRootRequiredForManager {
                actual: actual_root.to_path_buf(),
            });
        }
        return Ok(StartupCgroupPlacement::DelegatedRoot);
    }

    if root_override.is_some() && root.join(MANAGER_CGROUP_NAME) == actual_root {
        return Ok(StartupCgroupPlacement::ExistingManager);
    }

    Err(CgroupPathError::ConfiguredRootMismatch {
        configured: root.to_path_buf(),
        actual: actual_root.to_path_buf(),
    })
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

pub fn validate_job_id(job_id: &str) -> Result<(), CgroupPathError> {
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

const REQUIRED_CONTROLLERS: [&str; 3] = ["cpu", "memory", "pids"];

#[cfg(target_os = "linux")]
#[derive(Debug)]
/// 검증된 위임 영역에서 작업 cgroup을 만들고 관리한다.
pub struct CgroupManager {
    paths: CgroupPaths,
    #[cfg(target_os = "linux")]
    create_faults: Option<Arc<CgroupCreateFaults>>,
    #[cfg(target_os = "linux")]
    cleanup_faults: Option<Arc<CleanupFaults>>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub struct CgroupCreateFaults {
    mode: AtomicU8,
    read_back_attempts: AtomicUsize,
    rollback_attempts: AtomicUsize,
}

#[cfg(target_os = "linux")]
impl CgroupCreateFaults {
    pub fn inject_read_back_mismatch(&self, rollback_removal_fails: bool) {
        self.mode.store(
            if rollback_removal_fails { 2 } else { 1 },
            Ordering::Release,
        );
    }

    fn take_mode(&self) -> u8 {
        self.mode.swap(0, Ordering::AcqRel)
    }

    pub(super) fn record_read_back_attempt(&self) {
        self.read_back_attempts.fetch_add(1, Ordering::AcqRel);
    }

    pub fn read_back_attempts(&self) -> usize {
        self.read_back_attempts.load(Ordering::Acquire)
    }

    pub fn rollback_attempts(&self) -> usize {
        self.rollback_attempts.load(Ordering::Acquire)
    }
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
        Ok(Self {
            paths,
            #[cfg(target_os = "linux")]
            create_faults: None,
            #[cfg(target_os = "linux")]
            cleanup_faults: None,
        })
    }

    #[cfg(target_os = "linux")]
    pub fn initialize_with_create_faults(
        environment: VerifiedEnvironment,
        faults: Arc<CgroupCreateFaults>,
    ) -> Result<Self, CgroupError> {
        let mut manager = Self::initialize(environment)?;
        manager.create_faults = Some(faults);
        Ok(manager)
    }

    #[cfg(target_os = "linux")]
    pub fn initialize_with_cleanup_faults(
        environment: VerifiedEnvironment,
        faults: Arc<CleanupFaults>,
    ) -> Result<Self, CgroupError> {
        let mut manager = Self::initialize(environment)?;
        manager.cleanup_faults = Some(faults);
        Ok(manager)
    }

    pub fn root(&self) -> &Path {
        self.paths.root()
    }

    pub fn create_job(&self, job_id: &str, limits: CgroupLimits) -> Result<JobCgroup, CgroupError> {
        #[cfg(target_os = "linux")]
        if let Some(faults) = &self.create_faults {
            match faults.take_mode() {
                1 => {
                    let configure_faults = Arc::clone(faults);
                    let rollback_faults = Arc::clone(faults);
                    return self.create_job_with_rollback(
                        job_id,
                        limits,
                        move |path, limits| {
                            configure_job_with_read_back_mismatch(path, limits, &configure_faults)
                        },
                        move |path| {
                            rollback_faults
                                .rollback_attempts
                                .fetch_add(1, Ordering::AcqRel);
                            fs::remove_dir(path)
                        },
                    );
                }
                2 => {
                    let configure_faults = Arc::clone(faults);
                    let rollback_faults = Arc::clone(faults);
                    return self.create_job_with_rollback(
                        job_id,
                        limits,
                        move |path, limits| {
                            configure_job_with_read_back_mismatch(path, limits, &configure_faults)
                        },
                        move |_path| {
                            rollback_faults
                                .rollback_attempts
                                .fetch_add(1, Ordering::AcqRel);
                            Err(io::Error::other("injected cgroup rollback removal failure"))
                        },
                    );
                }
                _ => {}
            }
        }
        self.create_job_with(job_id, limits, configure_job)
    }

    fn create_job_with<F>(
        &self,
        job_id: &str,
        limits: CgroupLimits,
        configure: F,
    ) -> Result<JobCgroup, CgroupError>
    where
        F: FnOnce(
            &Path,
            CgroupLimits,
        ) -> Result<(File, KernelEvents, VerifiedCgroupLimits), CgroupError>,
    {
        self.create_job_with_rollback(job_id, limits, configure, |path| fs::remove_dir(path))
    }

    fn create_job_with_rollback<F, R>(
        &self,
        job_id: &str,
        limits: CgroupLimits,
        configure: F,
        rollback: R,
    ) -> Result<JobCgroup, CgroupError>
    where
        F: FnOnce(
            &Path,
            CgroupLimits,
        ) -> Result<(File, KernelEvents, VerifiedCgroupLimits), CgroupError>,
        R: FnOnce(&Path) -> io::Result<()>,
    {
        let path = self.paths.new_job_path(job_id)?;
        fs::create_dir(&path)
            .map_err(|source| cgroup_io_error("작업 cgroup 만들기", &path, source))?;

        let configured = configure(&path, limits);

        match configured {
            Ok((directory, baseline, verified_limits)) => Ok(JobCgroup {
                job_id: job_id.to_owned(),
                path,
                directory,
                baseline,
                verified_limits,
                cleaned: false,
                #[cfg(target_os = "linux")]
                cleanup_faults: self.cleanup_faults.clone(),
            }),
            Err(error) => {
                let _ = write_control(&path.join("cgroup.kill"), "1\n");
                match rollback(&path) {
                    Ok(()) => Err(error),
                    Err(cleanup) => match error {
                        mismatch @ CgroupError::ValueMismatch { .. } => {
                            Err(CgroupError::ReadBackRollbackUncertain {
                                mismatch: Box::new(mismatch),
                                cleanup: cleanup.to_string(),
                            })
                        }
                        other => Err(CgroupError::CleanupCombined {
                            primary: other.to_string(),
                            cleanup: cleanup.to_string(),
                        }),
                    },
                }
            }
        }
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

pub(super) fn write_control(path: &Path, value: &str) -> Result<(), CgroupError> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| cgroup_io_error("cgroup 제어 파일 열기", path, source))?;
    file.write_all(value.as_bytes())
        .map_err(|source| cgroup_io_error("cgroup 값 쓰기", path, source))
}

#[cfg(target_os = "linux")]
pub(super) fn require_regular_file(
    path: &Path,
    capability: &'static str,
) -> Result<(), CgroupError> {
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
pub(super) fn cgroup_io_error(
    operation: &'static str,
    path: &Path,
    source: io::Error,
) -> CgroupError {
    CgroupError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::cgroup::CpuLimit;
    use crate::cgroup::limits::write_and_verify;

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
    fn delegate_subgroup_infers_its_parent_as_the_delegated_root() {
        let root = infer_delegate_root(
            Path::new(DEFAULT_CGROUP_MOUNT),
            Path::new("/system.slice/taskcaged.service/manager"),
            OsStr::new(MANAGER_CGROUP_NAME),
        )
        .unwrap();

        assert_eq!(
            root,
            PathBuf::from("/sys/fs/cgroup/system.slice/taskcaged.service")
        );
    }

    #[test]
    fn delegate_subgroup_rejects_a_membership_outside_the_configured_subgroup() {
        let actual = Path::new("/system.slice/taskcaged.service");
        let error = infer_delegate_root(
            Path::new(DEFAULT_CGROUP_MOUNT),
            actual,
            OsStr::new(MANAGER_CGROUP_NAME),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CgroupPathError::DelegateSubgroupMismatch { configured, actual: found }
                if configured == MANAGER_CGROUP_NAME && found == actual
        ));
    }

    #[test]
    fn delegate_subgroup_rejects_an_unknown_subgroup() {
        let error = infer_delegate_root(
            Path::new(DEFAULT_CGROUP_MOUNT),
            Path::new("/system.slice/taskcaged.service/other"),
            OsStr::new("other"),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CgroupPathError::UnsupportedDelegateSubgroup { configured }
                if configured == "other"
        ));
    }

    #[test]
    fn startup_placement_rejects_manager_without_its_parent_root() {
        let manager = Path::new("/sys/fs/cgroup/delegated/manager");

        let error = classify_startup_placement(None, manager, manager, true).unwrap_err();

        assert!(matches!(
            error,
            CgroupPathError::ParentRootRequiredForManager { actual }
                if actual == manager
        ));
    }

    #[test]
    fn startup_placement_rejects_manager_configured_as_the_root() {
        let manager = Path::new("/sys/fs/cgroup/delegated/manager");

        let error = classify_startup_placement(Some(manager), manager, manager, true).unwrap_err();

        assert!(matches!(
            error,
            CgroupPathError::ParentRootRequiredForManager { actual }
                if actual == manager
        ));
    }

    #[test]
    fn startup_placement_accepts_existing_manager_only_with_parent_root() {
        let root = Path::new("/sys/fs/cgroup/delegated");
        let manager = root.join(MANAGER_CGROUP_NAME);

        assert_eq!(
            classify_startup_placement(Some(root), root, &manager, true).unwrap(),
            StartupCgroupPlacement::ExistingManager
        );
    }

    #[test]
    fn startup_placement_accepts_a_delegated_root() {
        let root = Path::new("/sys/fs/cgroup/taskcaged.service");

        assert_eq!(
            classify_startup_placement(None, root, root, true).unwrap(),
            StartupCgroupPlacement::DelegatedRoot
        );
    }

    #[test]
    fn normal_preflight_can_use_a_delegated_root_named_manager() {
        let root = Path::new("/sys/fs/cgroup/supervisor/manager");

        assert_eq!(
            classify_startup_placement(Some(root), root, root, false).unwrap(),
            StartupCgroupPlacement::DelegatedRoot
        );
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
    fn failure_after_job_creation_removes_job_before_target_start() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "taskcage-cgroup-created-failure-test-{}-{sequence}",
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
            create_faults: None,
            cleanup_faults: None,
        };
        let limits = CgroupLimits {
            memory_max_bytes: NonZeroU64::new(1).unwrap(),
            max_processes: NonZeroU64::new(1).unwrap(),
            cpu: CpuLimit {
                quota_micros: NonZeroU64::new(1).unwrap(),
                period_micros: NonZeroU64::new(1).unwrap(),
            },
        };
        let target_starts = std::cell::Cell::new(0);

        let result = manager
            .create_job_with("created-failure", limits, |path, _| {
                Err::<(File, KernelEvents, VerifiedCgroupLimits), _>(cgroup_io_error(
                    "injected after creation",
                    path,
                    io::Error::other("injected"),
                ))
            })
            .map(|_| target_starts.set(target_starts.get() + 1));

        assert!(result.is_err());
        assert_eq!(target_starts.get(), 0);
        assert!(!jobs.join("job-created-failure").exists());
        fs::remove_dir(&jobs).unwrap();
        fs::remove_dir(&root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn limit_read_back_failure_removes_job_before_target_start() {
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
            create_faults: None,
            cleanup_faults: None,
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
                Err::<(File, KernelEvents, VerifiedCgroupLimits), _>(CgroupError::ValueMismatch {
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
