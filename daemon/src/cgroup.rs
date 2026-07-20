//! 데몬이 제어할 cgroup v2 경로를 찾고 경로 이탈을 막는다.
//!
//! 이 모듈은 아직 작업을 실행하지 않는다. 실행 기능이 추가될 때도 여기서 확인한
//! 위임 경로 안에서만 작업 cgroup을 만들도록 경로 규칙을 한곳에 모아 둔다.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

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
}
