//! UDS를 열기 전에 이전 daemon이 남긴 작업 cgroup을 안전하게 정리한다.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::Duration;

use thiserror::Error;

use crate::cgroup::{CgroupPaths, StartupCgroupPlacement, validate_job_id};
use crate::deadline::MonotonicDeadline;

const CGROUP2_SUPER_MAGIC: libc::c_long = 0x6367_7270;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Error)]
#[error("{stage} 단계가 {path:?}에서 실패했습니다: {detail}")]
pub(crate) struct StartupCgroupError {
    stage: &'static str,
    path: PathBuf,
    detail: String,
}

impl StartupCgroupError {
    pub(crate) fn stage(&self) -> &'static str {
        self.stage
    }

    pub(crate) fn remaining_path(&self) -> &Path {
        &self.path
    }

    fn new(stage: &'static str, path: impl Into<PathBuf>, detail: impl Into<String>) -> Self {
        Self {
            stage,
            path: path.into(),
            detail: detail.into(),
        }
    }

    fn io(stage: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::new(stage, path, source.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StartupRecoveryReport {
    pub(crate) removed_jobs: usize,
    pub(crate) placement: StartupCgroupPlacement,
}

/// stale socket 소유권을 얻은 뒤, preflight보다 먼저 호출해야 한다.
pub(crate) fn recover_from_environment(
    timeout: Duration,
) -> Result<StartupRecoveryReport, StartupCgroupError> {
    let configured = std::env::var_os("TASKCAGE_CGROUP_ROOT").map(PathBuf::from);
    recover(timeout, configured)
}

fn recover(
    timeout: Duration,
    configured: Option<PathBuf>,
) -> Result<StartupRecoveryReport, StartupCgroupError> {
    let deadline = MonotonicDeadline::from_now(timeout).ok_or_else(|| {
        StartupCgroupError::new(
            "startup recovery deadline 생성",
            "/sys/fs/cgroup",
            "0이거나 표현할 수 없는 시간 예산입니다",
        )
    })?;
    let mut backend = SystemRecovery::prepare(deadline, configured)?;
    recover_with_backend(&mut backend, deadline)
}

trait RecoveryBackend {
    fn job_count(&self) -> usize;
    fn job_path(&self, index: usize) -> &Path;
    fn kill_job(
        &mut self,
        index: usize,
        deadline: MonotonicDeadline,
    ) -> Result<(), StartupCgroupError>;
    fn job_populated(
        &mut self,
        index: usize,
        deadline: MonotonicDeadline,
    ) -> Result<bool, StartupCgroupError>;
    fn remove_job(
        &mut self,
        index: usize,
        deadline: MonotonicDeadline,
    ) -> Result<(), StartupCgroupError>;
    fn finish_structure(
        &mut self,
        deadline: MonotonicDeadline,
    ) -> Result<StartupCgroupPlacement, StartupCgroupError>;

    fn pause(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

fn recover_with_backend<B: RecoveryBackend>(
    backend: &mut B,
    deadline: MonotonicDeadline,
) -> Result<StartupRecoveryReport, StartupCgroupError> {
    let job_count = backend.job_count();
    for index in 0..job_count {
        ensure_time(deadline, "잔여 작업 전체 종료", backend.job_path(index))?;
        backend.kill_job(index, deadline)?;
    }

    let mut pending = vec![true; job_count];
    while pending.iter().any(|value| *value) {
        for (index, is_pending) in pending.iter_mut().enumerate() {
            if !*is_pending {
                continue;
            }
            ensure_time(
                deadline,
                "잔여 작업 populated 0 확인",
                backend.job_path(index),
            )?;
            if !backend.job_populated(index, deadline)? {
                *is_pending = false;
            }
        }
        if pending.iter().any(|value| *value) {
            let Some(remaining) = deadline.remaining() else {
                let index = pending.iter().position(|value| *value).unwrap_or(0);
                return Err(deadline_error(
                    deadline,
                    "잔여 작업 populated 0 확인",
                    backend.job_path(index),
                ));
            };
            backend.pause(remaining.min(POLL_INTERVAL));
        }
    }

    for index in 0..job_count {
        ensure_time(deadline, "잔여 작업 cgroup 제거", backend.job_path(index))?;
        backend.remove_job(index, deadline)?;
    }
    ensure_time(deadline, "manager와 jobs 구조 정리", Path::new("jobs"))?;
    let placement = backend.finish_structure(deadline)?;

    Ok(StartupRecoveryReport {
        removed_jobs: job_count,
        placement,
    })
}

fn ensure_time(
    deadline: MonotonicDeadline,
    stage: &'static str,
    path: &Path,
) -> Result<(), StartupCgroupError> {
    if deadline.remaining().is_some() {
        Ok(())
    } else {
        Err(deadline_error(deadline, stage, path))
    }
}

fn deadline_error(
    deadline: MonotonicDeadline,
    stage: &'static str,
    path: &Path,
) -> StartupCgroupError {
    StartupCgroupError::new(
        stage,
        path,
        format!("공유 deadline {:?}을 초과했습니다", deadline.budget()),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntryMetadata {
    device: libc::dev_t,
    inode: libc::ino_t,
    mode: libc::mode_t,
}

impl EntryMetadata {
    fn from_stat(value: libc::stat) -> Self {
        Self {
            device: value.st_dev,
            inode: value.st_ino,
            mode: value.st_mode,
        }
    }

    fn file_type(self) -> libc::mode_t {
        self.mode & libc::S_IFMT
    }

    fn is_directory(self) -> bool {
        self.file_type() == libc::S_IFDIR
    }

    fn is_regular(self) -> bool {
        self.file_type() == libc::S_IFREG
    }
}

#[derive(Debug)]
struct Directory {
    descriptor: OwnedFd,
    identity: EntryMetadata,
    name: OsString,
    path: PathBuf,
}

#[derive(Debug)]
struct DirectoryEntry {
    name: OsString,
    metadata: EntryMetadata,
}

#[derive(Debug)]
struct SystemRecovery {
    root: Directory,
    manager: Option<Directory>,
    jobs: Option<Directory>,
    job_entries: Vec<Directory>,
    placement: StartupCgroupPlacement,
}

impl SystemRecovery {
    fn prepare(
        deadline: MonotonicDeadline,
        configured: Option<PathBuf>,
    ) -> Result<Self, StartupCgroupError> {
        let configured_descriptor = match configured.as_deref() {
            Some(path) => Some(open_absolute_directory(path, "위임 root 열기")?),
            None => None,
        };
        ensure_time(
            deadline,
            "cgroup 위임 경로 확인",
            configured.as_deref().unwrap_or(Path::new("/sys/fs/cgroup")),
        )?;
        let (paths, placement) =
            CgroupPaths::resolve_startup(configured.as_deref()).map_err(|error| {
                StartupCgroupError::new(
                    "cgroup 위임 경로 확인",
                    configured
                        .clone()
                        .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup")),
                    error.to_string(),
                )
            })?;
        let root_descriptor = match configured_descriptor {
            Some(descriptor) => {
                let canonical = open_absolute_directory(paths.root(), "위임 root 신원 확인")?;
                let configured_identity =
                    metadata_for_fd(descriptor.as_raw_fd(), paths.root(), "위임 root 신원 확인")?;
                let canonical_identity =
                    metadata_for_fd(canonical.as_raw_fd(), paths.root(), "위임 root 신원 확인")?;
                if configured_identity != canonical_identity {
                    return Err(StartupCgroupError::new(
                        "위임 root 신원 확인",
                        paths.root(),
                        "설정 경로와 실제 위임 root의 device/inode가 다릅니다",
                    ));
                }
                descriptor
            }
            None => open_absolute_directory(paths.root(), "위임 root 열기")?,
        };
        ensure_cgroup2(root_descriptor.as_raw_fd(), paths.root())?;
        let root = Directory {
            identity: metadata_for_fd(
                root_descriptor.as_raw_fd(),
                paths.root(),
                "위임 root 신원 확인",
            )?,
            descriptor: root_descriptor,
            name: paths
                .root()
                .file_name()
                .unwrap_or(OsStr::new("/"))
                .to_os_string(),
            path: paths.root().to_path_buf(),
        };

        ensure_time(deadline, "잔여 cgroup 구조 확인", root.path.as_path())?;
        let root_entries = read_directory(&root)?;
        validate_regular_entries(&root, &root_entries)?;
        let manager = open_expected_child(&root, &root_entries, "manager")?;
        let jobs = open_expected_child(&root, &root_entries, "jobs")?;
        reject_unexpected_directories(&root, &root_entries, &["manager", "jobs"])?;

        if let Some(manager) = &manager {
            validate_leaf_cgroup(manager, "manager cgroup 구조 확인")?;
            match placement {
                StartupCgroupPlacement::DelegatedRoot => {
                    require_direct_processes_empty(manager, "manager 직접 프로세스 확인")?;
                }
                StartupCgroupPlacement::ExistingManager => {
                    require_only_current_process(manager, "manager 직접 프로세스 확인")?;
                }
            }
        } else if placement == StartupCgroupPlacement::ExistingManager {
            return Err(StartupCgroupError::new(
                "manager cgroup 구조 확인",
                paths.manager(),
                "현재 membership에 해당하는 manager cgroup을 찾지 못했습니다",
            ));
        }

        let mut job_entries = Vec::new();
        if let Some(jobs_directory) = &jobs {
            let entries = read_directory(jobs_directory)?;
            validate_regular_entries(jobs_directory, &entries)?;
            require_direct_processes_empty(jobs_directory, "jobs 직접 프로세스 확인")?;
            for entry in entries.iter().filter(|entry| entry.metadata.is_directory()) {
                let name = entry.name.to_str().ok_or_else(|| {
                    StartupCgroupError::new(
                        "작업 cgroup 이름 확인",
                        jobs_directory.path.join(&entry.name),
                        "UTF-8이 아닌 cgroup 이름은 TaskCage 소유로 확인할 수 없습니다",
                    )
                })?;
                let Some(job_id) = name.strip_prefix("job-") else {
                    return Err(StartupCgroupError::new(
                        "작업 cgroup 이름 확인",
                        jobs_directory.path.join(&entry.name),
                        "job- 접두사가 없는 예상 밖 하위 cgroup입니다",
                    ));
                };
                validate_job_id(job_id).map_err(|error| {
                    StartupCgroupError::new(
                        "작업 cgroup 이름 확인",
                        jobs_directory.path.join(&entry.name),
                        error.to_string(),
                    )
                })?;
                let job = open_child_directory(jobs_directory, &entry.name, entry.metadata)?;
                validate_leaf_cgroup(&job, "작업 cgroup 구조 확인")?;
                require_control_file(&job, "cgroup.kill", "작업 종료 제어 파일 확인")?;
                require_control_file(&job, "cgroup.events", "작업 상태 파일 확인")?;
                job_entries.push(job);
            }
        }
        job_entries.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(Self {
            root,
            manager,
            jobs,
            job_entries,
            placement,
        })
    }

    fn remove_parent(&self, directory: &Directory) -> Result<(), StartupCgroupError> {
        require_direct_processes_empty(directory, "상위 cgroup 직접 프로세스 재확인")?;
        if is_populated(directory)? {
            return Err(StartupCgroupError::new(
                "상위 cgroup populated 0 확인",
                &directory.path,
                "하위 프로세스가 남아 있습니다",
            ));
        }
        let entries = read_directory(directory)?;
        validate_regular_entries(directory, &entries)?;
        reject_unexpected_directories(directory, &entries, &[])?;
        remove_identical_directory(&self.root, directory)
    }
}

impl RecoveryBackend for SystemRecovery {
    fn job_count(&self) -> usize {
        self.job_entries.len()
    }

    fn job_path(&self, index: usize) -> &Path {
        &self.job_entries[index].path
    }

    fn kill_job(
        &mut self,
        index: usize,
        _deadline: MonotonicDeadline,
    ) -> Result<(), StartupCgroupError> {
        write_control(
            &self.job_entries[index],
            "cgroup.kill",
            b"1\n",
            "잔여 작업 전체 종료",
        )
    }

    fn job_populated(
        &mut self,
        index: usize,
        _deadline: MonotonicDeadline,
    ) -> Result<bool, StartupCgroupError> {
        is_populated(&self.job_entries[index])
    }

    fn remove_job(
        &mut self,
        index: usize,
        _deadline: MonotonicDeadline,
    ) -> Result<(), StartupCgroupError> {
        let jobs = self.jobs.as_ref().ok_or_else(|| {
            StartupCgroupError::new(
                "작업 cgroup 제거",
                &self.job_entries[index].path,
                "jobs 상위 cgroup을 찾지 못했습니다",
            )
        })?;
        validate_leaf_cgroup(&self.job_entries[index], "작업 cgroup 제거 전 구조 재확인")?;
        if is_populated(&self.job_entries[index])? {
            return Err(StartupCgroupError::new(
                "작업 cgroup 제거 전 populated 0 재확인",
                &self.job_entries[index].path,
                "프로세스가 다시 나타났습니다",
            ));
        }
        remove_identical_directory(jobs, &self.job_entries[index])
    }

    fn finish_structure(
        &mut self,
        deadline: MonotonicDeadline,
    ) -> Result<StartupCgroupPlacement, StartupCgroupError> {
        if let Some(jobs) = &self.jobs {
            ensure_time(deadline, "jobs cgroup 제거", &jobs.path)?;
            self.remove_parent(jobs)?;
        }
        if self.placement == StartupCgroupPlacement::DelegatedRoot {
            if let Some(manager) = &self.manager {
                ensure_time(deadline, "manager cgroup 제거", &manager.path)?;
                self.remove_parent(manager)?;
            }
        } else if let Some(manager) = &self.manager {
            ensure_time(deadline, "manager cgroup 재사용 확인", &manager.path)?;
            validate_leaf_cgroup(manager, "manager cgroup 재사용 확인")?;
            require_only_current_process(manager, "manager 직접 프로세스 재확인")?;
        }
        ensure_time(deadline, "위임 root 최종 확인", &self.root.path)?;
        let entries = read_directory(&self.root)?;
        validate_regular_entries(&self.root, &entries)?;
        let allowed = match self.placement {
            StartupCgroupPlacement::DelegatedRoot => &[][..],
            StartupCgroupPlacement::ExistingManager => &["manager"][..],
        };
        reject_unexpected_directories(&self.root, &entries, allowed)?;
        Ok(self.placement)
    }
}

fn open_absolute_directory(
    path: &Path,
    stage: &'static str,
) -> Result<OwnedFd, StartupCgroupError> {
    if !path.is_absolute() {
        return Err(StartupCgroupError::new(
            stage,
            path,
            "상대 경로는 허용하지 않습니다",
        ));
    }
    let root_name = CString::new("/").expect("고정 경로에는 NUL이 없습니다");
    let raw = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw == -1 {
        return Err(StartupCgroupError::io(
            stage,
            Path::new("/"),
            io::Error::last_os_error(),
        ));
    }
    let mut current = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut opened = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                opened.push(name);
                let name = c_string(name, &opened, stage)?;
                let raw = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    )
                };
                if raw == -1 {
                    return Err(StartupCgroupError::io(
                        stage,
                        &opened,
                        io::Error::last_os_error(),
                    ));
                }
                current = unsafe { OwnedFd::from_raw_fd(raw) };
            }
            _ => {
                return Err(StartupCgroupError::new(
                    stage,
                    path,
                    "'.', '..' 또는 platform prefix가 있는 경로는 허용하지 않습니다",
                ));
            }
        }
    }
    Ok(current)
}

fn ensure_cgroup2(descriptor: RawFd, path: &Path) -> Result<(), StartupCgroupError> {
    let mut value = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(descriptor, value.as_mut_ptr()) } == -1 {
        return Err(StartupCgroupError::io(
            "cgroup v2 filesystem 확인",
            path,
            io::Error::last_os_error(),
        ));
    }
    let value = unsafe { value.assume_init() };
    if value.f_type == CGROUP2_SUPER_MAGIC {
        Ok(())
    } else {
        Err(StartupCgroupError::new(
            "cgroup v2 filesystem 확인",
            path,
            "설정 경로가 cgroup v2 filesystem이 아닙니다",
        ))
    }
}

fn open_expected_child(
    parent: &Directory,
    entries: &[DirectoryEntry],
    name: &str,
) -> Result<Option<Directory>, StartupCgroupError> {
    let Some(entry) = entries.iter().find(|entry| entry.name == OsStr::new(name)) else {
        return Ok(None);
    };
    if !entry.metadata.is_directory() {
        return Err(StartupCgroupError::new(
            "TaskCage cgroup 구조 확인",
            parent.path.join(name),
            "예상한 cgroup directory가 아닙니다",
        ));
    }
    open_child_directory(parent, &entry.name, entry.metadata).map(Some)
}

fn open_child_directory(
    parent: &Directory,
    name: &OsStr,
    expected: EntryMetadata,
) -> Result<Directory, StartupCgroupError> {
    let path = parent.path.join(name);
    let name_c = c_string(name, &path, "cgroup directory 열기")?;
    let raw = unsafe {
        libc::openat(
            parent.descriptor.as_raw_fd(),
            name_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw == -1 {
        return Err(StartupCgroupError::io(
            "cgroup directory 열기",
            &path,
            io::Error::last_os_error(),
        ));
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    let actual = metadata_for_fd(descriptor.as_raw_fd(), &path, "cgroup directory 신원 확인")?;
    if actual != expected {
        return Err(StartupCgroupError::new(
            "cgroup directory 신원 확인",
            &path,
            "검사와 open 사이에 device/inode/mode가 바뀌었습니다",
        ));
    }
    Ok(Directory {
        descriptor,
        identity: actual,
        name: name.to_os_string(),
        path,
    })
}

fn read_directory(directory: &Directory) -> Result<Vec<DirectoryEntry>, StartupCgroupError> {
    let duplicated =
        unsafe { libc::fcntl(directory.descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated == -1 {
        return Err(StartupCgroupError::io(
            "cgroup directory 열거 준비",
            &directory.path,
            io::Error::last_os_error(),
        ));
    }
    let stream = unsafe { libc::fdopendir(duplicated) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(duplicated) };
        return Err(StartupCgroupError::io(
            "cgroup directory 열거 준비",
            &directory.path,
            error,
        ));
    }

    let mut entries = Vec::new();
    loop {
        unsafe { *libc::__errno_location() = 0 };
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            let close_result = unsafe { libc::closedir(stream) };
            if error.raw_os_error().unwrap_or(0) != 0 {
                return Err(StartupCgroupError::io(
                    "cgroup directory 열거",
                    &directory.path,
                    error,
                ));
            }
            if close_result == -1 {
                return Err(StartupCgroupError::io(
                    "cgroup directory 열거 종료",
                    &directory.path,
                    io::Error::last_os_error(),
                ));
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = OsStr::from_bytes(name).to_os_string();
        let path = directory.path.join(&name);
        let metadata = metadata_at(
            directory.descriptor.as_raw_fd(),
            &name,
            &path,
            "cgroup entry 확인",
        )?;
        entries.push(DirectoryEntry { name, metadata });
    }
    Ok(entries)
}

fn validate_regular_entries(
    directory: &Directory,
    entries: &[DirectoryEntry],
) -> Result<(), StartupCgroupError> {
    for entry in entries
        .iter()
        .filter(|entry| !entry.metadata.is_directory())
    {
        let path = directory.path.join(&entry.name);
        if !entry.metadata.is_regular() {
            return Err(StartupCgroupError::new(
                "예상 밖 cgroup entry 확인",
                path,
                "regular cgroup 제어 파일이나 directory가 아닙니다",
            ));
        }
        let Some(name) = entry.name.to_str() else {
            return Err(StartupCgroupError::new(
                "cgroup 제어 파일 이름 확인",
                path,
                "UTF-8이 아닌 파일 이름입니다",
            ));
        };
        if !is_cgroup_interface_name(name) {
            return Err(StartupCgroupError::new(
                "예상 밖 cgroup entry 확인",
                path,
                "커널 cgroup interface 이름으로 확인할 수 없습니다",
            ));
        }
    }
    Ok(())
}

fn is_cgroup_interface_name(name: &str) -> bool {
    let Some((prefix, suffix)) = name.split_once('.') else {
        return false;
    };
    !prefix.is_empty()
        && !suffix.is_empty()
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && suffix.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.')
        })
}

fn reject_unexpected_directories(
    directory: &Directory,
    entries: &[DirectoryEntry],
    allowed: &[&str],
) -> Result<(), StartupCgroupError> {
    for entry in entries.iter().filter(|entry| entry.metadata.is_directory()) {
        if !allowed.iter().any(|name| entry.name == OsStr::new(name)) {
            return Err(StartupCgroupError::new(
                "예상 밖 하위 cgroup 확인",
                directory.path.join(&entry.name),
                "TaskCage가 소유한다고 증명할 수 없는 하위 cgroup입니다",
            ));
        }
    }
    Ok(())
}

fn validate_leaf_cgroup(
    directory: &Directory,
    stage: &'static str,
) -> Result<(), StartupCgroupError> {
    let entries = read_directory(directory)?;
    validate_regular_entries(directory, &entries)?;
    reject_unexpected_directories(directory, &entries, &[])
        .map_err(|error| StartupCgroupError::new(stage, error.remaining_path(), error.to_string()))
}

fn require_direct_processes_empty(
    directory: &Directory,
    stage: &'static str,
) -> Result<(), StartupCgroupError> {
    let processes = read_control(directory, "cgroup.procs", stage)?;
    if processes.trim().is_empty() {
        Ok(())
    } else {
        Err(StartupCgroupError::new(
            stage,
            directory.path.join("cgroup.procs"),
            "TaskCage job 바깥의 직접 프로세스가 있어 종료할 수 없습니다",
        ))
    }
}

fn require_only_current_process(
    directory: &Directory,
    stage: &'static str,
) -> Result<(), StartupCgroupError> {
    let processes = read_control(directory, "cgroup.procs", stage)?;
    let mut entries = processes.split_whitespace();
    let expected = std::process::id().to_string();
    if entries.next() == Some(expected.as_str()) && entries.next().is_none() {
        Ok(())
    } else {
        Err(StartupCgroupError::new(
            stage,
            directory.path.join("cgroup.procs"),
            "현재 daemon 외의 직접 프로세스가 있어 manager를 재사용할 수 없습니다",
        ))
    }
}

fn require_control_file(
    directory: &Directory,
    name: &str,
    stage: &'static str,
) -> Result<(), StartupCgroupError> {
    let metadata = metadata_at(
        directory.descriptor.as_raw_fd(),
        OsStr::new(name),
        &directory.path.join(name),
        stage,
    )?;
    if metadata.is_regular() {
        Ok(())
    } else {
        Err(StartupCgroupError::new(
            stage,
            directory.path.join(name),
            "regular cgroup 제어 파일이 아닙니다",
        ))
    }
}

fn is_populated(directory: &Directory) -> Result<bool, StartupCgroupError> {
    let contents = read_control(directory, "cgroup.events", "cgroup populated 상태 읽기")?;
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("populated") {
            return match (fields.next(), fields.next()) {
                (Some("0"), None) => Ok(false),
                (Some("1"), None) => Ok(true),
                _ => Err(StartupCgroupError::new(
                    "cgroup populated 상태 해석",
                    directory.path.join("cgroup.events"),
                    "populated 값이 0 또는 1이 아닙니다",
                )),
            };
        }
    }
    Err(StartupCgroupError::new(
        "cgroup populated 상태 해석",
        directory.path.join("cgroup.events"),
        "populated 항목이 없습니다",
    ))
}

fn read_control(
    directory: &Directory,
    name: &str,
    stage: &'static str,
) -> Result<String, StartupCgroupError> {
    let descriptor = open_control(directory, name, libc::O_RDONLY, stage)?;
    let mut contents = String::new();
    File::from(descriptor)
        .read_to_string(&mut contents)
        .map_err(|source| StartupCgroupError::io(stage, directory.path.join(name), source))?;
    Ok(contents)
}

fn write_control(
    directory: &Directory,
    name: &str,
    value: &[u8],
    stage: &'static str,
) -> Result<(), StartupCgroupError> {
    let descriptor = open_control(directory, name, libc::O_WRONLY, stage)?;
    File::from(descriptor)
        .write_all(value)
        .map_err(|source| StartupCgroupError::io(stage, directory.path.join(name), source))
}

fn open_control(
    directory: &Directory,
    name: &str,
    access: libc::c_int,
    stage: &'static str,
) -> Result<OwnedFd, StartupCgroupError> {
    let path = directory.path.join(name);
    let name = c_string(OsStr::new(name), &path, stage)?;
    let raw = unsafe {
        libc::openat(
            directory.descriptor.as_raw_fd(),
            name.as_ptr(),
            access | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw == -1 {
        return Err(StartupCgroupError::io(
            stage,
            &path,
            io::Error::last_os_error(),
        ));
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    let metadata = metadata_for_fd(descriptor.as_raw_fd(), &path, stage)?;
    if !metadata.is_regular() {
        return Err(StartupCgroupError::new(
            stage,
            path,
            "regular cgroup 제어 파일이 아닙니다",
        ));
    }
    Ok(descriptor)
}

fn remove_identical_directory(
    parent: &Directory,
    child: &Directory,
) -> Result<(), StartupCgroupError> {
    let current = metadata_at(
        parent.descriptor.as_raw_fd(),
        &child.name,
        &child.path,
        "cgroup 제거 전 신원 재확인",
    )?;
    if current != child.identity {
        return Err(StartupCgroupError::new(
            "cgroup 제거 전 신원 재확인",
            &child.path,
            "검사한 directory와 현재 device/inode/mode가 다릅니다",
        ));
    }
    let name = c_string(&child.name, &child.path, "cgroup 제거")?;
    if unsafe {
        libc::unlinkat(
            parent.descriptor.as_raw_fd(),
            name.as_ptr(),
            libc::AT_REMOVEDIR,
        )
    } == -1
    {
        return Err(StartupCgroupError::io(
            "cgroup 제거",
            &child.path,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

fn metadata_at(
    parent: RawFd,
    name: &OsStr,
    path: &Path,
    stage: &'static str,
) -> Result<EntryMetadata, StartupCgroupError> {
    let name = c_string(name, path, stage)?;
    let mut value = MaybeUninit::<libc::stat>::zeroed();
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            value.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == -1
    {
        return Err(StartupCgroupError::io(
            stage,
            path,
            io::Error::last_os_error(),
        ));
    }
    Ok(EntryMetadata::from_stat(unsafe { value.assume_init() }))
}

fn metadata_for_fd(
    descriptor: RawFd,
    path: &Path,
    stage: &'static str,
) -> Result<EntryMetadata, StartupCgroupError> {
    let mut value = MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(descriptor, value.as_mut_ptr()) } == -1 {
        return Err(StartupCgroupError::io(
            stage,
            path,
            io::Error::last_os_error(),
        ));
    }
    Ok(EntryMetadata::from_stat(unsafe { value.assume_init() }))
}

fn c_string(
    value: &OsStr,
    path: &Path,
    stage: &'static str,
) -> Result<CString, StartupCgroupError> {
    CString::new(value.as_bytes()).map_err(|_| {
        StartupCgroupError::new(stage, path, "경로에 NUL byte가 있어 사용할 수 없습니다")
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::os::unix::fs::symlink;
    use std::time::Instant;
    use std::time::SystemTime;

    use super::*;

    #[derive(Debug)]
    struct FakeBackend {
        paths: Vec<PathBuf>,
        populated: Vec<VecDeque<bool>>,
        events: Vec<String>,
        deadlines: Vec<Instant>,
        fail_kill: Option<usize>,
        fail_remove: Option<usize>,
        finished: bool,
    }

    impl FakeBackend {
        fn new(populated: Vec<Vec<bool>>) -> Self {
            let paths = (0..populated.len())
                .map(|index| PathBuf::from(format!("/delegated/jobs/job-{index}")))
                .collect();
            Self {
                paths,
                populated: populated.into_iter().map(VecDeque::from).collect(),
                events: Vec::new(),
                deadlines: Vec::new(),
                fail_kill: None,
                fail_remove: None,
                finished: false,
            }
        }

        fn record(&mut self, deadline: MonotonicDeadline) {
            self.deadlines.push(deadline.at());
        }
    }

    impl RecoveryBackend for FakeBackend {
        fn job_count(&self) -> usize {
            self.paths.len()
        }

        fn job_path(&self, index: usize) -> &Path {
            &self.paths[index]
        }

        fn kill_job(
            &mut self,
            index: usize,
            deadline: MonotonicDeadline,
        ) -> Result<(), StartupCgroupError> {
            self.record(deadline);
            self.events.push(format!("kill:{index}"));
            if self.fail_kill == Some(index) {
                Err(StartupCgroupError::new(
                    "잔여 작업 전체 종료",
                    self.paths[index].clone(),
                    "injected",
                ))
            } else {
                Ok(())
            }
        }

        fn job_populated(
            &mut self,
            index: usize,
            deadline: MonotonicDeadline,
        ) -> Result<bool, StartupCgroupError> {
            self.record(deadline);
            self.events.push(format!("poll:{index}"));
            Ok(self.populated[index].pop_front().unwrap_or(false))
        }

        fn remove_job(
            &mut self,
            index: usize,
            deadline: MonotonicDeadline,
        ) -> Result<(), StartupCgroupError> {
            self.record(deadline);
            self.events.push(format!("remove:{index}"));
            if self.fail_remove == Some(index) {
                Err(StartupCgroupError::new(
                    "잔여 작업 cgroup 제거",
                    self.paths[index].clone(),
                    "injected",
                ))
            } else {
                Ok(())
            }
        }

        fn finish_structure(
            &mut self,
            deadline: MonotonicDeadline,
        ) -> Result<StartupCgroupPlacement, StartupCgroupError> {
            self.record(deadline);
            self.events.push("finish".to_owned());
            self.finished = true;
            Ok(StartupCgroupPlacement::DelegatedRoot)
        }

        fn pause(&mut self, _duration: Duration) {}
    }

    #[test]
    fn no_residual_jobs_finishes_the_parent_structure() {
        let deadline = MonotonicDeadline::from_now(Duration::from_secs(1)).unwrap();
        let mut backend = FakeBackend::new(Vec::new());

        let report = recover_with_backend(&mut backend, deadline).unwrap();

        assert_eq!(report.removed_jobs, 0);
        assert_eq!(backend.events, ["finish"]);
        assert!(backend.finished);
    }

    #[test]
    fn kills_every_job_before_waiting_and_uses_one_deadline() {
        let deadline = MonotonicDeadline::from_now(Duration::from_secs(1)).unwrap();
        let mut backend = FakeBackend::new(vec![vec![true, false], vec![false], vec![false]]);

        let report = recover_with_backend(&mut backend, deadline).unwrap();

        assert_eq!(report.removed_jobs, 3);
        assert_eq!(&backend.events[..3], ["kill:0", "kill:1", "kill:2"]);
        assert!(
            backend
                .events
                .iter()
                .position(|event| event == "remove:0")
                .unwrap()
                > backend
                    .events
                    .iter()
                    .rposition(|event| event.starts_with("poll:"))
                    .unwrap()
        );
        assert!(
            backend
                .deadlines
                .iter()
                .all(|value| *value == deadline.at())
        );
    }

    #[test]
    fn kill_and_remove_failures_stop_before_startup_can_continue() {
        let deadline = MonotonicDeadline::from_now(Duration::from_secs(1)).unwrap();
        let mut kill_failure = FakeBackend::new(vec![vec![false]]);
        kill_failure.fail_kill = Some(0);
        assert_eq!(
            recover_with_backend(&mut kill_failure, deadline)
                .unwrap_err()
                .stage(),
            "잔여 작업 전체 종료"
        );
        assert!(!kill_failure.finished);

        let mut remove_failure = FakeBackend::new(vec![vec![false]]);
        remove_failure.fail_remove = Some(0);
        assert_eq!(
            recover_with_backend(&mut remove_failure, deadline)
                .unwrap_err()
                .stage(),
            "잔여 작업 cgroup 제거"
        );
        assert!(!remove_failure.finished);
    }

    #[test]
    fn expired_shared_deadline_does_not_start_a_side_effect() {
        let deadline = MonotonicDeadline::expired_at(Instant::now());
        let mut backend = FakeBackend::new(vec![vec![false]]);

        assert!(recover_with_backend(&mut backend, deadline).is_err());
        assert!(backend.events.is_empty());
    }

    #[test]
    fn cgroup_interface_names_and_job_names_are_narrow() {
        assert!(is_cgroup_interface_name("cgroup.events"));
        assert!(is_cgroup_interface_name("memory.events.local"));
        assert!(!is_cgroup_interface_name("unexpected"));
        assert!(validate_job_id("33333333-3333-3333-3333-333333333333").is_ok());
        assert!(validate_job_id("../outside").is_err());
    }

    #[test]
    fn absolute_directory_open_rejects_relative_escape_and_symlink() {
        assert!(open_absolute_directory(Path::new("relative"), "test").is_err());
        assert!(open_absolute_directory(Path::new("/tmp/../tmp"), "test").is_err());

        let root = temporary_path("symlink");
        let target = root.join("target");
        let link = root.join("link");
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, &link).unwrap();
        assert!(open_absolute_directory(&link, "test").is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir(target).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn existing_manager_requires_only_the_current_daemon_and_is_preserved() {
        let root_path = temporary_path("existing-manager");
        let manager_path = root_path.join("manager");
        std::fs::create_dir_all(&manager_path).unwrap();
        std::fs::write(
            manager_path.join("cgroup.procs"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();

        let root = directory_for_test(&root_path);
        let manager = directory_for_test(&manager_path);
        require_only_current_process(&manager, "test").unwrap();
        let mut recovery = SystemRecovery {
            root,
            manager: Some(manager),
            jobs: None,
            job_entries: Vec::new(),
            placement: StartupCgroupPlacement::ExistingManager,
        };
        let deadline = MonotonicDeadline::from_now(Duration::from_secs(1)).unwrap();

        assert_eq!(
            recovery.finish_structure(deadline).unwrap(),
            StartupCgroupPlacement::ExistingManager
        );
        assert!(manager_path.is_dir());

        std::fs::write(
            manager_path.join("cgroup.procs"),
            format!("{}\n999999\n", std::process::id()),
        )
        .unwrap();
        let manager = directory_for_test(&manager_path);
        assert!(require_only_current_process(&manager, "test").is_err());

        std::fs::remove_file(manager_path.join("cgroup.procs")).unwrap();
        std::fs::remove_dir(manager_path).unwrap();
        std::fs::remove_dir(root_path).unwrap();
    }

    #[test]
    fn unexpected_file_directory_and_symlink_are_preserved() {
        let root_path = temporary_path("unexpected");
        std::fs::create_dir_all(&root_path).unwrap();
        let unexpected_file = root_path.join("unexpected");
        let unexpected_directory = root_path.join("foreign");
        let unexpected_symlink = root_path.join("link");
        std::fs::write(&unexpected_file, b"not a cgroup interface").unwrap();
        std::fs::create_dir(&unexpected_directory).unwrap();
        symlink(&unexpected_directory, &unexpected_symlink).unwrap();
        let root = directory_for_test(&root_path);
        let entries = read_directory(&root).unwrap();

        assert!(validate_regular_entries(&root, &entries).is_err());
        assert!(unexpected_file.exists());
        assert!(unexpected_directory.exists());
        assert!(std::fs::symlink_metadata(&unexpected_symlink).is_ok());

        std::fs::remove_file(unexpected_symlink).unwrap();
        std::fs::remove_file(unexpected_file).unwrap();
        std::fs::remove_dir(unexpected_directory).unwrap();
        std::fs::remove_dir(root_path).unwrap();
    }

    #[test]
    fn replaced_inode_is_not_removed() {
        let root_path = temporary_path("inode");
        let original_path = root_path.join("job-safe");
        let displaced_path = root_path.join("job-original");
        std::fs::create_dir_all(&original_path).unwrap();
        let root = directory_for_test(&root_path);
        let original = directory_for_test(&original_path);

        std::fs::rename(&original_path, &displaced_path).unwrap();
        std::fs::create_dir(&original_path).unwrap();

        assert!(remove_identical_directory(&root, &original).is_err());
        assert!(original_path.exists());
        assert!(displaced_path.exists());

        std::fs::remove_dir(original_path).unwrap();
        std::fs::remove_dir(displaced_path).unwrap();
        std::fs::remove_dir(root_path).unwrap();
    }

    fn directory_for_test(path: &Path) -> Directory {
        let descriptor = open_absolute_directory(path, "test").unwrap();
        Directory {
            identity: metadata_for_fd(descriptor.as_raw_fd(), path, "test").unwrap(),
            descriptor,
            name: path.file_name().unwrap().to_os_string(),
            path: path.to_path_buf(),
        }
    }

    fn temporary_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "taskcage-startup-cgroup-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_recovery_kills_descendants_removes_jobs_and_allows_preflight() {
        use std::collections::BTreeMap;
        use std::num::NonZeroUsize;
        use std::os::unix::ffi::OsStringExt;

        use crate::cgroup::CgroupManager;
        use crate::executor::{PreparedCommand, SpawnOutcome, spawn_in_cgroup};
        use crate::output::CaptureLimits;
        use crate::preflight::{CapabilityProbe, SystemProbe};

        if std::env::var_os("TASKCAGE_RUN_LINUX_STARTUP_RECOVERY_INTEGRATION").is_none() {
            return;
        }
        let ghost = std::env::var_os("TASKCAGE_STARTUP_RECOVERY_GHOST_BIN")
            .expect("ghost fixture 경로가 필요합니다");
        let paths = CgroupPaths::resolve(None).unwrap();
        let environment = SystemProbe::from_environment().check().unwrap();
        let manager = CgroupManager::initialize(environment).unwrap();
        assert_eq!(manager.root(), paths.root());
        let empty = paths.jobs().join("job-empty");
        let running = paths.jobs().join("job-running");
        std::fs::create_dir(&empty).unwrap();
        std::fs::create_dir(&running).unwrap();

        let command = PreparedCommand::new(
            vec![ghost, OsString::from_vec(b"--hold-parent".to_vec())],
            &std::env::current_dir().unwrap(),
            BTreeMap::new(),
        )
        .unwrap();
        let directory = File::open(&running).unwrap();
        let limits = CaptureLimits::new(
            NonZeroUsize::new(1024).unwrap(),
            NonZeroUsize::new(1024).unwrap(),
        );
        let pending = spawn_in_cgroup(&command, directory.as_raw_fd(), limits).unwrap();
        let process = match pending.start().unwrap() {
            SpawnOutcome::Started(process) => process,
            SpawnOutcome::ExecFailed(failure) => panic!("ghost exec 실패: {}", failure.errno),
        };
        thread::sleep(Duration::from_millis(100));

        let report = recover(Duration::from_secs(5), Some(paths.root().to_path_buf())).unwrap();
        assert_eq!(report.removed_jobs, 2);
        assert_eq!(report.placement, StartupCgroupPlacement::ExistingManager);
        assert!(paths.manager().exists());
        assert!(!paths.jobs().exists());
        assert!(!running.exists());

        let cleanup_deadline = MonotonicDeadline::from_now(Duration::from_secs(2)).unwrap();
        process
            .reap_after_kill_until(cleanup_deadline)
            .await
            .unwrap();
        process.finish_output_until(cleanup_deadline).await.unwrap();

        let environment = SystemProbe::with_root(paths.root())
            .check_after_recovery(report.placement)
            .unwrap();
        let manager = CgroupManager::initialize(environment).unwrap();
        assert_eq!(manager.root(), paths.root());
    }
}
