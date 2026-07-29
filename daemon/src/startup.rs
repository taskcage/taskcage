//! daemon socket을 열기 전에 단일 시작 소유권과 stale socket 신원을 확인한다.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::{MaybeUninit, offset_of, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use thiserror::Error;

const LOCK_FILE_NAME: &str = ".taskcaged.lock";
const SOCKET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub(crate) enum StartupError {
    #[error("daemon socket 경로는 절대 경로여야 합니다: {0}")]
    RelativeSocketPath(PathBuf),
    #[error("daemon socket 경로를 안전하게 해석할 수 없습니다: {0}")]
    InvalidSocketPath(PathBuf),
    #[error("runtime 디렉터리를 symlink 없이 열지 못했습니다: {path}")]
    OpenRuntimeDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "runtime 디렉터리 owner가 안전하지 않습니다: {path}, owner={owner}, expected={expected}"
    )]
    UnsafeDirectoryOwner {
        path: PathBuf,
        owner: libc::uid_t,
        expected: libc::uid_t,
    },
    #[error("runtime 디렉터리를 group 또는 other가 쓸 수 있습니다: {path}, mode={mode:o}")]
    UnsafeDirectoryMode { path: PathBuf, mode: libc::mode_t },
    #[error("daemon lock 파일을 열지 못했습니다: {path}")]
    OpenLock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("daemon lock 경로가 안전한 일반 파일이 아닙니다: {0}")]
    InvalidLockFile(PathBuf),
    #[error("daemon lock owner가 현재 daemon과 다릅니다: {path}, owner={owner}")]
    InvalidLockOwner { path: PathBuf, owner: libc::uid_t },
    #[error("daemon lock mode가 0600이 아닙니다: {path}, mode={mode:o}")]
    InvalidLockMode { path: PathBuf, mode: libc::mode_t },
    #[error("daemon lock의 link count가 1이 아닙니다: {path}, links={links}")]
    InvalidLockLinks { path: PathBuf, links: libc::nlink_t },
    #[error("다른 taskcaged가 runtime 디렉터리 소유권을 가지고 있습니다: {0}")]
    LockHeld(PathBuf),
    #[error("daemon lock을 획득하지 못했습니다: {path}")]
    Lock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("daemon socket 경로를 확인하지 못했습니다: {path}")]
    InspectSocket {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("기존 경로가 검증된 Unix domain socket이 아닙니다: {0}")]
    UnknownSocketObject(PathBuf),
    #[error("기존 socket owner가 현재 daemon과 다릅니다: {path}, owner={owner}")]
    UnknownSocketOwner { path: PathBuf, owner: libc::uid_t },
    #[error("기존 socket mode가 0600이 아닙니다: {path}, mode={mode:o}")]
    UnknownSocketMode { path: PathBuf, mode: libc::mode_t },
    #[error("기존 socket의 link count가 1이 아닙니다: {path}, links={links}")]
    UnknownSocketLinks { path: PathBuf, links: libc::nlink_t },
    #[error("기존 daemon socket이 활성 상태입니다: {0}")]
    ActiveSocket(PathBuf),
    #[error("기존 daemon socket의 활성 상태를 확정하지 못했습니다: {path}")]
    ProbeSocket {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("기존 daemon socket 활성 확인 시간이 끝났습니다: {0}")]
    ProbeTimeout(PathBuf),
    #[error("확인 중 daemon socket 경로가 바뀌어 삭제하지 않았습니다: {0}")]
    SocketChanged(PathBuf),
    #[error("검증된 stale daemon socket을 제거하지 못했습니다: {path}")]
    RemoveSocket {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stale socket 제거 뒤 같은 경로가 다시 생겨 시작을 중단합니다: {0}")]
    SocketReappeared(PathBuf),
}

#[derive(Debug)]
pub(crate) struct StartupOwnership {
    socket_path: PathBuf,
    socket_name: OsString,
    parent: File,
    _lock: File,
}

impl StartupOwnership {
    pub(crate) fn acquire(socket_path: &Path) -> Result<Self, StartupError> {
        Self::acquire_with_hook(socket_path, || {})
    }

    fn acquire_with_hook<F>(socket_path: &Path, before_unlink: F) -> Result<Self, StartupError>
    where
        F: FnOnce(),
    {
        let (parent, parent_path, socket_name) = open_protected_parent(socket_path)?;
        let lock = acquire_lock(&parent, &parent_path)?;
        let ownership = Self {
            socket_path: socket_path.to_path_buf(),
            socket_name,
            parent,
            _lock: lock,
        };
        ownership.recover_stale_socket(before_unlink)?;
        Ok(ownership)
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn recover_stale_socket<F>(&self, before_unlink: F) -> Result<(), StartupError>
    where
        F: FnOnce(),
    {
        let Some(first) = inspect_at(
            self.parent.as_raw_fd(),
            &self.socket_name,
            &self.socket_path,
        )?
        else {
            return Ok(());
        };
        validate_socket_metadata(first, unsafe { libc::geteuid() }, &self.socket_path)?;

        match probe_socket(&self.socket_path)? {
            SocketProbe::Active => {
                return Err(StartupError::ActiveSocket(self.socket_path.clone()));
            }
            SocketProbe::Disappeared => {
                return match inspect_at(
                    self.parent.as_raw_fd(),
                    &self.socket_name,
                    &self.socket_path,
                )? {
                    None => Ok(()),
                    Some(_) => Err(StartupError::SocketChanged(self.socket_path.clone())),
                };
            }
            SocketProbe::Refused => {}
        }

        before_unlink();
        let Some(current) = inspect_at(
            self.parent.as_raw_fd(),
            &self.socket_name,
            &self.socket_path,
        )?
        else {
            return Err(StartupError::SocketChanged(self.socket_path.clone()));
        };
        if current != first {
            return Err(StartupError::SocketChanged(self.socket_path.clone()));
        }

        let name = c_string(&self.socket_name, &self.socket_path)?;
        let removed = unsafe { libc::unlinkat(self.parent.as_raw_fd(), name.as_ptr(), 0) };
        if removed == -1 {
            return Err(StartupError::RemoveSocket {
                path: self.socket_path.clone(),
                source: io::Error::last_os_error(),
            });
        }
        match inspect_at(
            self.parent.as_raw_fd(),
            &self.socket_name,
            &self.socket_path,
        )? {
            None => Ok(()),
            Some(_) => Err(StartupError::SocketReappeared(self.socket_path.clone())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathMetadata {
    device: libc::dev_t,
    inode: libc::ino_t,
    mode: libc::mode_t,
    owner: libc::uid_t,
    links: libc::nlink_t,
}

impl PathMetadata {
    fn from_stat(value: libc::stat) -> Self {
        Self {
            device: value.st_dev,
            inode: value.st_ino,
            mode: value.st_mode,
            owner: value.st_uid,
            links: value.st_nlink,
        }
    }

    fn permissions(self) -> libc::mode_t {
        self.mode & 0o777
    }

    fn is_directory(self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }

    fn is_regular(self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }

    fn is_socket(self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFSOCK
    }
}

fn open_protected_parent(socket_path: &Path) -> Result<(File, PathBuf, OsString), StartupError> {
    if !socket_path.is_absolute() {
        return Err(StartupError::RelativeSocketPath(socket_path.to_path_buf()));
    }
    if socket_path.as_os_str().as_bytes().last() == Some(&b'/') {
        return Err(StartupError::InvalidSocketPath(socket_path.to_path_buf()));
    }
    let socket_name = socket_path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| StartupError::InvalidSocketPath(socket_path.to_path_buf()))?
        .to_os_string();
    let parent_path = socket_path
        .parent()
        .ok_or_else(|| StartupError::InvalidSocketPath(socket_path.to_path_buf()))?;

    let mut directory = open_root()?;
    let mut opened_path = PathBuf::from("/");
    let effective_uid = unsafe { libc::geteuid() };
    // 각 component를 앞에서 연 directory FD 기준으로 열어 중간 symlink 교체를 따라가지 않는다.
    for component in parent_path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                opened_path.push(name);
                directory = open_directory_at(&directory, name, &opened_path)?;
                validate_directory(
                    metadata_for_fd(&directory, &opened_path)?,
                    &opened_path,
                    effective_uid,
                    false,
                )?;
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(StartupError::InvalidSocketPath(socket_path.to_path_buf()));
            }
        }
    }
    validate_directory(
        metadata_for_fd(&directory, parent_path)?,
        parent_path,
        effective_uid,
        true,
    )?;
    Ok((directory, parent_path.to_path_buf(), socket_name))
}

fn open_root() -> Result<File, StartupError> {
    let root = CString::new("/").expect("root 경로에는 NUL이 없습니다");
    let descriptor = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor == -1 {
        return Err(StartupError::OpenRuntimeDirectory {
            path: PathBuf::from("/"),
            source: io::Error::last_os_error(),
        });
    }
    // 성공한 open이 반환한 FD의 소유권을 File로 옮겨 모든 오류 경로에서 닫히게 한다.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn open_directory_at(parent: &File, name: &OsStr, path: &Path) -> Result<File, StartupError> {
    let name = c_string(name, path)?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor == -1 {
        return Err(StartupError::OpenRuntimeDirectory {
            path: path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    // 성공한 openat FD는 이 File만 소유한다.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn validate_directory(
    metadata: PathMetadata,
    path: &Path,
    effective_uid: libc::uid_t,
    final_parent: bool,
) -> Result<(), StartupError> {
    if !metadata.is_directory() {
        return Err(StartupError::OpenRuntimeDirectory {
            path: path.to_path_buf(),
            source: io::Error::other("경로가 디렉터리가 아닙니다"),
        });
    }
    let owner_is_allowed = if final_parent {
        metadata.owner == effective_uid
    } else {
        metadata.owner == 0 || metadata.owner == effective_uid
    };
    if !owner_is_allowed {
        return Err(StartupError::UnsafeDirectoryOwner {
            path: path.to_path_buf(),
            owner: metadata.owner,
            expected: effective_uid,
        });
    }
    if metadata.permissions() & 0o022 != 0 {
        return Err(StartupError::UnsafeDirectoryMode {
            path: path.to_path_buf(),
            mode: metadata.permissions(),
        });
    }
    Ok(())
}

fn acquire_lock(parent: &File, parent_path: &Path) -> Result<File, StartupError> {
    let lock_path = parent_path.join(LOCK_FILE_NAME);
    let lock_name = CString::new(LOCK_FILE_NAME).expect("lock 파일 이름에는 NUL이 없습니다");
    let created = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            lock_name.as_ptr(),
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    };
    let descriptor = if created != -1 {
        created
    } else {
        let create_error = io::Error::last_os_error();
        if create_error.kind() != io::ErrorKind::AlreadyExists {
            return Err(StartupError::OpenLock {
                path: lock_path,
                source: create_error,
            });
        }
        let existing = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                lock_name.as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if existing == -1 {
            return Err(StartupError::OpenLock {
                path: lock_path,
                source: io::Error::last_os_error(),
            });
        }
        existing
    };
    // 성공한 openat FD를 lock guard가 daemon 생존 기간 동안 소유한다.
    let lock = unsafe { File::from_raw_fd(descriptor) };
    let metadata = metadata_for_fd(&lock, &lock_path)?;
    if !metadata.is_regular() {
        return Err(StartupError::InvalidLockFile(lock_path));
    }
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.owner != effective_uid {
        return Err(StartupError::InvalidLockOwner {
            path: lock_path,
            owner: metadata.owner,
        });
    }
    if metadata.permissions() != 0o600 {
        return Err(StartupError::InvalidLockMode {
            path: lock_path,
            mode: metadata.permissions(),
        });
    }
    if metadata.links != 1 {
        return Err(StartupError::InvalidLockLinks {
            path: lock_path,
            links: metadata.links,
        });
    }

    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked == -1 {
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::WouldBlock {
            return Err(StartupError::LockHeld(lock_path));
        }
        return Err(StartupError::Lock {
            path: lock_path,
            source,
        });
    }
    Ok(lock)
}

fn metadata_for_fd(file: &File, path: &Path) -> Result<PathMetadata, StartupError> {
    let mut value = MaybeUninit::<libc::stat>::zeroed();
    let result = unsafe { libc::fstat(file.as_raw_fd(), value.as_mut_ptr()) };
    if result == -1 {
        return Err(StartupError::OpenRuntimeDirectory {
            path: path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    // fstat 성공 뒤에만 커널이 채운 stat 값을 읽는다.
    Ok(PathMetadata::from_stat(unsafe { value.assume_init() }))
}

fn inspect_at(
    parent: RawFd,
    name: &OsStr,
    path: &Path,
) -> Result<Option<PathMetadata>, StartupError> {
    let name = c_string(name, path)?;
    let mut value = MaybeUninit::<libc::stat>::zeroed();
    let result = unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            value.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // fstatat 성공 뒤에만 커널이 채운 stat 값을 읽는다.
        return Ok(Some(PathMetadata::from_stat(unsafe {
            value.assume_init()
        })));
    }
    let source = io::Error::last_os_error();
    if source.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(StartupError::InspectSocket {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn validate_socket_metadata(
    metadata: PathMetadata,
    effective_uid: libc::uid_t,
    path: &Path,
) -> Result<(), StartupError> {
    if !metadata.is_socket() {
        return Err(StartupError::UnknownSocketObject(path.to_path_buf()));
    }
    if metadata.owner != effective_uid {
        return Err(StartupError::UnknownSocketOwner {
            path: path.to_path_buf(),
            owner: metadata.owner,
        });
    }
    if metadata.permissions() != 0o600 {
        return Err(StartupError::UnknownSocketMode {
            path: path.to_path_buf(),
            mode: metadata.permissions(),
        });
    }
    if metadata.links != 1 {
        return Err(StartupError::UnknownSocketLinks {
            path: path.to_path_buf(),
            links: metadata.links,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketProbe {
    Active,
    Refused,
    Disappeared,
}

fn probe_socket(path: &Path) -> Result<SocketProbe, StartupError> {
    let (address, length) = unix_address(path)?;
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if descriptor == -1 {
        return Err(StartupError::ProbeSocket {
            path: path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    // socket syscall이 만든 FD를 OwnedFd로 옮겨 probe가 끝나면 항상 닫는다.
    let socket = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let connected = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            length,
        )
    };
    if connected == 0 {
        return Ok(SocketProbe::Active);
    }
    let source = io::Error::last_os_error();
    match source.raw_os_error() {
        Some(libc::ECONNREFUSED) => Ok(SocketProbe::Refused),
        Some(libc::ENOENT) => Ok(SocketProbe::Disappeared),
        Some(libc::EINPROGRESS) => wait_for_connect(&socket, path),
        _ => Err(StartupError::ProbeSocket {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn wait_for_connect(socket: &OwnedFd, path: &Path) -> Result<SocketProbe, StartupError> {
    let deadline = Instant::now() + SOCKET_PROBE_TIMEOUT;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(StartupError::ProbeTimeout(path.to_path_buf()));
        };
        let milliseconds = remaining.as_millis().max(1);
        let timeout = i32::try_from(milliseconds).unwrap_or(i32::MAX);
        let mut event = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut event, 1, timeout) };
        if result == 0 {
            return Err(StartupError::ProbeTimeout(path.to_path_buf()));
        }
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(StartupError::ProbeSocket {
                path: path.to_path_buf(),
                source,
            });
        }
        if event.revents & libc::POLLNVAL != 0 {
            return Err(StartupError::ProbeSocket {
                path: path.to_path_buf(),
                source: io::Error::other("socket probe file descriptor가 유효하지 않습니다"),
            });
        }

        let mut socket_error: libc::c_int = 0;
        let mut length = libc::socklen_t::try_from(size_of::<libc::c_int>())
            .expect("SO_ERROR 값 크기는 socklen_t로 표현할 수 있습니다");
        let result = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut socket_error as *mut libc::c_int).cast(),
                &mut length,
            )
        };
        if result == -1 {
            return Err(StartupError::ProbeSocket {
                path: path.to_path_buf(),
                source: io::Error::last_os_error(),
            });
        }
        return match socket_error {
            0 => Ok(SocketProbe::Active),
            libc::ECONNREFUSED => Ok(SocketProbe::Refused),
            libc::ENOENT => Ok(SocketProbe::Disappeared),
            other => Err(StartupError::ProbeSocket {
                path: path.to_path_buf(),
                source: io::Error::from_raw_os_error(other),
            }),
        };
    }
}

fn unix_address(path: &Path) -> Result<(libc::sockaddr_un, libc::socklen_t), StartupError> {
    let bytes = path.as_os_str().as_bytes();
    // sockaddr_un은 C ABI 구조체이므로 먼저 0으로 채워 pathname 뒤 NUL과 padding을 고정한다.
    let mut address = unsafe { MaybeUninit::<libc::sockaddr_un>::zeroed().assume_init() };
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() + 1 > address.sun_path.len() {
        return Err(StartupError::InvalidSocketPath(path.to_path_buf()));
    }
    address.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX)
        .expect("AF_UNIX 값은 sa_family_t로 표현할 수 있습니다");
    // 위 길이 검사로 sun_path 범위 안에만 pathname bytes를 복사한다.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            address.sun_path.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }
    let length = offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    let length = libc::socklen_t::try_from(length)
        .map_err(|_| StartupError::InvalidSocketPath(path.to_path_buf()))?;
    Ok((address, length))
}

fn c_string(value: &OsStr, path: &Path) -> Result<CString, StartupError> {
    CString::new(value.as_bytes()).map_err(|_| StartupError::InvalidSocketPath(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink};
    use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    const CRASH_HELPER_ENV: &str = "TASKCAGE_STALE_SOCKET_CRASH_HELPER";

    struct TestRuntime {
        directory: PathBuf,
        socket: PathBuf,
    }

    impl TestRuntime {
        fn new(label: &str) -> Self {
            let base = std::env::current_dir()
                .unwrap()
                .join("target")
                .join("taskcage-startup-tests");
            fs::create_dir_all(&base).unwrap();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let directory = base.join(format!("{label}-{}-{sequence}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let socket = directory.join("taskcaged.sock");
            Self { directory, socket }
        }

        fn lock(&self) -> PathBuf {
            self.directory.join(LOCK_FILE_NAME)
        }
    }

    impl Drop for TestRuntime {
        fn drop(&mut self) {
            if let Ok(metadata) = fs::symlink_metadata(&self.socket) {
                if metadata.file_type().is_dir() {
                    let _ = fs::remove_dir(&self.socket);
                } else {
                    let _ = fs::remove_file(&self.socket);
                }
            }
            let _ = fs::remove_file(self.lock());
            let _ = fs::remove_dir(&self.directory);
        }
    }

    fn bind_owner_only(path: &Path) -> StdUnixListener {
        let listener = StdUnixListener::bind(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        listener
    }

    #[test]
    fn rejects_relative_paths() {
        assert!(matches!(
            StartupOwnership::acquire(Path::new("taskcaged.sock")),
            Err(StartupError::RelativeSocketPath(_))
        ));
    }

    #[test]
    fn recovers_only_a_verified_stale_socket_without_opening_a_listener() {
        let runtime = TestRuntime::new("stale");
        drop(bind_owner_only(&runtime.socket));

        let ownership = StartupOwnership::acquire(&runtime.socket).unwrap();

        assert!(!runtime.socket.exists());
        assert_eq!(
            fs::symlink_metadata(runtime.lock()).unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(
            StdUnixStream::connect(&runtime.socket).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        drop(ownership);
    }

    #[test]
    fn preserves_an_active_socket() {
        let runtime = TestRuntime::new("active");
        let listener = bind_owner_only(&runtime.socket);

        assert!(matches!(
            StartupOwnership::acquire(&runtime.socket),
            Err(StartupError::ActiveSocket(_))
        ));
        assert!(
            fs::symlink_metadata(&runtime.socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        drop(listener);
    }

    #[test]
    fn preserves_regular_files_directories_and_symlinks() {
        let regular = TestRuntime::new("regular");
        fs::write(&regular.socket, b"owner data").unwrap();
        assert!(matches!(
            StartupOwnership::acquire(&regular.socket),
            Err(StartupError::UnknownSocketObject(_))
        ));
        assert_eq!(fs::read(&regular.socket).unwrap(), b"owner data");

        let directory = TestRuntime::new("directory");
        fs::create_dir(&directory.socket).unwrap();
        assert!(matches!(
            StartupOwnership::acquire(&directory.socket),
            Err(StartupError::UnknownSocketObject(_))
        ));
        assert!(directory.socket.is_dir());

        let linked = TestRuntime::new("symlink");
        let target = linked.directory.join("target");
        fs::write(&target, b"target data").unwrap();
        symlink(&target, &linked.socket).unwrap();
        assert!(matches!(
            StartupOwnership::acquire(&linked.socket),
            Err(StartupError::UnknownSocketObject(_))
        ));
        assert_eq!(fs::read(&target).unwrap(), b"target data");
        fs::remove_file(target).unwrap();
    }

    #[test]
    fn refuses_wrong_socket_owner_or_mode() {
        let runtime = TestRuntime::new("wrong-mode");
        let listener = bind_owner_only(&runtime.socket);
        fs::set_permissions(&runtime.socket, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(matches!(
            StartupOwnership::acquire(&runtime.socket),
            Err(StartupError::UnknownSocketMode { .. })
        ));
        assert!(runtime.socket.exists());
        drop(listener);

        let effective_uid = unsafe { libc::geteuid() };
        let metadata = PathMetadata {
            device: 1,
            inode: 1,
            mode: libc::S_IFSOCK | 0o600,
            owner: effective_uid.wrapping_add(1),
            links: 1,
        };
        assert!(matches!(
            validate_socket_metadata(metadata, effective_uid, Path::new("/socket")),
            Err(StartupError::UnknownSocketOwner { .. })
        ));
    }

    #[test]
    fn rejects_unsafe_parent_directories_and_parent_symlinks() {
        let runtime = TestRuntime::new("unsafe-parent");
        fs::set_permissions(&runtime.directory, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(matches!(
            StartupOwnership::acquire(&runtime.socket),
            Err(StartupError::UnsafeDirectoryMode { .. })
        ));
        assert!(!runtime.socket.exists());

        let linked = TestRuntime::new("parent-link");
        let actual = linked.directory.join("actual");
        fs::create_dir(&actual).unwrap();
        fs::set_permissions(&actual, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = linked.directory.join("alias");
        symlink(&actual, &alias).unwrap();
        let socket = alias.join("taskcaged.sock");
        assert!(matches!(
            StartupOwnership::acquire(&socket),
            Err(StartupError::OpenRuntimeDirectory { .. })
        ));
        assert!(!actual.join("taskcaged.sock").exists());
        fs::remove_file(alias).unwrap();
        fs::remove_dir(actual).unwrap();
    }

    #[test]
    fn refuses_untrusted_lock_files_without_replacing_them() {
        let wrong_mode = TestRuntime::new("lock-mode");
        fs::write(wrong_mode.lock(), b"operator data").unwrap();
        fs::set_permissions(wrong_mode.lock(), fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            StartupOwnership::acquire(&wrong_mode.socket),
            Err(StartupError::InvalidLockMode { .. })
        ));
        assert_eq!(fs::read(wrong_mode.lock()).unwrap(), b"operator data");

        let linked = TestRuntime::new("lock-link");
        let target = linked.directory.join("lock-target");
        fs::write(&target, b"target data").unwrap();
        symlink(&target, linked.lock()).unwrap();
        assert!(matches!(
            StartupOwnership::acquire(&linked.socket),
            Err(StartupError::OpenLock { .. })
        ));
        assert_eq!(fs::read(&target).unwrap(), b"target data");
        fs::remove_file(target).unwrap();
    }

    #[test]
    fn preserves_a_socket_replaced_before_unlink() {
        let runtime = TestRuntime::new("replaced");
        drop(bind_owner_only(&runtime.socket));
        let original_inode = fs::symlink_metadata(&runtime.socket).unwrap().ino();
        let replacement_path = runtime.directory.join("replacement.sock");
        let replacement = bind_owner_only(&replacement_path);
        assert_ne!(
            original_inode,
            fs::symlink_metadata(&replacement_path).unwrap().ino()
        );

        let error = StartupOwnership::acquire_with_hook(&runtime.socket, || {
            fs::remove_file(&runtime.socket).unwrap();
            fs::rename(&replacement_path, &runtime.socket).unwrap();
        })
        .unwrap_err();

        assert!(matches!(error, StartupError::SocketChanged(_)));
        assert!(
            fs::symlink_metadata(&runtime.socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        drop(replacement);
    }

    #[test]
    fn only_one_daemon_owns_the_runtime_directory() {
        let runtime = TestRuntime::new("exclusive");
        let first = StartupOwnership::acquire(&runtime.socket).unwrap();

        assert!(matches!(
            StartupOwnership::acquire(&runtime.socket),
            Err(StartupError::LockHeld(_))
        ));
        drop(first);
        StartupOwnership::acquire(&runtime.socket).unwrap();
    }

    #[test]
    fn abrupt_exit_releases_lock_and_leaves_only_a_recoverable_socket() {
        let runtime = TestRuntime::new("crash");
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("startup::tests::crash_helper_leaves_socket")
            .arg("--nocapture")
            .env(CRASH_HELPER_ENV, &runtime.socket)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(99));
        assert!(runtime.socket.exists());
        assert!(runtime.lock().exists());

        let ownership = StartupOwnership::acquire(&runtime.socket).unwrap();
        assert!(!runtime.socket.exists());
        assert!(runtime.lock().exists());
        drop(ownership);
    }

    #[test]
    fn crash_helper_leaves_socket() {
        let Some(socket) = std::env::var_os(CRASH_HELPER_ENV).map(PathBuf::from) else {
            return;
        };
        let _ownership = StartupOwnership::acquire(&socket).unwrap();
        let _listener = bind_owner_only(&socket);
        // 실제 crash처럼 Rust Drop을 건너뛰고 커널의 FD close로 lock이 풀리는지 확인한다.
        unsafe { libc::_exit(99) };
    }
}
