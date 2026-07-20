//! `clone3(CLONE_INTO_CGROUP)`로 target을 생성 순간부터 작업 cgroup 안에 둔다.
//!
//! clone3 뒤의 자식 실행 흐름에서는 메모리 할당, 잠금, 비동기 실행기와 구조화 로그를
//! 사용하지 않는다. 필요한 문자열과 포인터는 부모가 모두 준비한다.

use std::env;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use serde::Serialize;
use thiserror::Error;
use tokio::time::sleep;

const CLONE_INTO_CGROUP: u64 = 0x0002_0000_0000;

#[repr(C)]
#[derive(Debug, Default)]
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

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("실행할 프로그램이 없습니다")]
    EmptyCommand,
    #[error("명령이나 환경 변수에 NUL 문자가 있습니다")]
    NulByte,
    #[error("실행 파일을 찾을 수 없거나 실행 권한이 없습니다: {0:?}")]
    ExecutableNotFound(OsString),
    #[error("실행 오류 전달용 관을 만들지 못했습니다")]
    Pipe(#[source] io::Error),
    #[error("clone3로 작업 cgroup 안에 프로세스를 만들지 못했습니다")]
    Clone(#[source] io::Error),
    #[error("target execve가 실패했습니다: errno {0}")]
    Exec(i32),
    #[error("자식 프로세스 종료 상태를 회수하지 못했습니다")]
    Wait(#[source] io::Error),
    #[error("자식이 잘못된 실행 오류 값을 보냈습니다")]
    InvalidExecPayload,
}

#[derive(Debug)]
/// clone3 뒤의 자식이 추가 준비 없이 바로 execve를 호출할 수 있게 만든 명령이다.
pub struct PreparedCommand {
    executable: CString,
    argv: Vec<CString>,
    argv_pointers: Vec<*const libc::c_char>,
    environment: Vec<CString>,
    environment_pointers: Vec<*const libc::c_char>,
    working_directory: Option<CString>,
}

impl PreparedCommand {
    pub fn new(
        command: Vec<OsString>,
        working_directory: Option<&Path>,
    ) -> Result<Self, ExecutorError> {
        let original_executable = command.first().ok_or(ExecutorError::EmptyCommand)?;
        // 셸 문자열로 다시 해석하지 않고 실행 파일과 각 인자를 그대로 보존한다.
        let executable_path = resolve_executable(original_executable)?;
        let executable = os_string_to_cstring(executable_path.into_os_string())?;
        let argv: Vec<_> = command
            .into_iter()
            .map(os_string_to_cstring)
            .collect::<Result<_, _>>()?;
        let environment: Vec<_> = env::vars_os()
            .map(|(key, value)| {
                let mut entry = key.into_vec();
                entry.push(b'=');
                entry.extend(value.into_vec());
                CString::new(entry).map_err(|_| ExecutorError::NulByte)
            })
            .collect::<Result<_, _>>()?;
        let working_directory = working_directory
            .map(|path| os_string_to_cstring(path.as_os_str().to_owned()))
            .transpose()?;

        let mut prepared = Self {
            executable,
            argv,
            argv_pointers: Vec::new(),
            environment,
            environment_pointers: Vec::new(),
            working_directory,
        };
        prepared.argv_pointers = prepared
            .argv
            .iter()
            .map(|value| value.as_ptr())
            .chain(std::iter::once(ptr::null()))
            .collect();
        prepared.environment_pointers = prepared
            .environment
            .iter()
            .map(|value| value.as_ptr())
            .chain(std::iter::once(ptr::null()))
            .collect();
        Ok(prepared)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SpawnedProcess {
    pid: libc::pid_t,
}

impl SpawnedProcess {
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    pub async fn wait_for(&self, timeout: Duration) -> Result<WaitOutcome, ExecutorError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = wait_nohang(self.pid)? {
                return Ok(WaitOutcome::Exited(status));
            }
            if Instant::now() >= deadline {
                return Ok(WaitOutcome::TimedOut);
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn reap_after_kill(&self, timeout: Duration) -> Result<ProcessExit, ExecutorError> {
        match self.wait_for(timeout).await? {
            WaitOutcome::Exited(status) => Ok(status),
            WaitOutcome::TimedOut => Err(ExecutorError::Wait(io::Error::new(
                io::ErrorKind::TimedOut,
                "cgroup 전체 종료 뒤 target의 종료 상태를 회수하지 못했습니다",
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum WaitOutcome {
    Exited(ProcessExit),
    TimedOut,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExit {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

pub fn spawn_in_cgroup(
    command: &PreparedCommand,
    cgroup_fd: RawFd,
) -> Result<SpawnedProcess, ExecutorError> {
    let (read_end, write_end) = pipe_cloexec()?;
    let parent_pid = unsafe { libc::getpid() };
    let args = CloneArgs {
        flags: CLONE_INTO_CGROUP,
        exit_signal: libc::SIGCHLD as u64,
        cgroup: cgroup_fd as u64,
        ..CloneArgs::default()
    };

    // 프로세스를 만든 뒤 옮기는 경쟁 구간을 만들지 않고 생성 순간부터 제한 안에 둔다.
    let result = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &args as *const CloneArgs,
            size_of::<CloneArgs>(),
        )
    };
    if result == -1 {
        return Err(ExecutorError::Clone(io::Error::last_os_error()));
    }
    if result == 0 {
        child_exec(
            command,
            read_end.as_raw_fd(),
            write_end.as_raw_fd(),
            parent_pid,
        );
    }

    drop(write_end);
    let pid = result as libc::pid_t;
    let mut payload = Vec::with_capacity(size_of::<i32>());
    if let Err(error) = File::from(read_end).read_to_end(&mut payload) {
        // 오류 통로를 읽지 못해도 직접 만든 자식을 좀비로 남기지 않는다. 작업 cgroup의
        // 다른 프로세스는 호출자가 이어서 전체 종료한다.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let _ = wait_blocking(pid);
        return Err(ExecutorError::Pipe(error));
    }
    if payload.is_empty() {
        Ok(SpawnedProcess { pid })
    } else if payload.len() == size_of::<i32>() {
        let errno = i32::from_ne_bytes(payload.try_into().expect("길이를 먼저 확인했습니다"));
        let _ = wait_blocking(pid);
        Err(ExecutorError::Exec(errno))
    } else {
        let _ = wait_blocking(pid);
        Err(ExecutorError::InvalidExecPayload)
    }
}

fn pipe_cloexec() -> Result<(OwnedFd, OwnedFd), ExecutorError> {
    let mut descriptors = [-1; 2];
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    if result == -1 {
        return Err(ExecutorError::Pipe(io::Error::last_os_error()));
    }
    let read_end = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    Ok((read_end, write_end))
}

fn child_exec(
    command: &PreparedCommand,
    read_fd: RawFd,
    write_fd: RawFd,
    expected_parent: libc::pid_t,
) -> ! {
    // 이 아래의 자식 경로는 할당, 잠금, 로그 없이 낮은 수준의 시스템 호출만 사용한다.
    unsafe {
        libc::close(read_fd);
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1
            || libc::getppid() != expected_parent
        {
            write_errno_and_exit(write_fd, current_errno_or(libc::ESRCH));
        }
        if let Some(directory) = &command.working_directory
            && libc::chdir(directory.as_ptr()) == -1
        {
            write_errno_and_exit(write_fd, current_errno_or(libc::ENOENT));
        }
        libc::execve(
            command.executable.as_ptr(),
            command.argv_pointers.as_ptr(),
            command.environment_pointers.as_ptr(),
        );
        write_errno_and_exit(write_fd, current_errno_or(libc::ENOEXEC));
    }
}

unsafe fn write_errno_and_exit(write_fd: RawFd, errno: i32) -> ! {
    let bytes = errno.to_ne_bytes();
    unsafe {
        libc::write(write_fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len());
        libc::_exit(126);
    }
}

unsafe fn current_errno_or(fallback: i32) -> i32 {
    let errno = unsafe { *libc::__errno_location() };
    if errno == 0 { fallback } else { errno }
}

fn wait_nohang(pid: libc::pid_t) -> Result<Option<ProcessExit>, ExecutorError> {
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result == pid {
            return Ok(Some(decode_wait_status(status)));
        }
        if result == 0 {
            return Ok(None);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(ExecutorError::Wait(error));
        }
    }
}

fn wait_blocking(pid: libc::pid_t) -> Result<ProcessExit, ExecutorError> {
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return Ok(decode_wait_status(status));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(ExecutorError::Wait(error));
        }
    }
}

fn decode_wait_status(status: libc::c_int) -> ProcessExit {
    if libc::WIFEXITED(status) {
        ProcessExit {
            exit_code: Some(libc::WEXITSTATUS(status)),
            signal: None,
        }
    } else if libc::WIFSIGNALED(status) {
        ProcessExit {
            exit_code: None,
            signal: Some(libc::WTERMSIG(status)),
        }
    } else {
        ProcessExit {
            exit_code: None,
            signal: None,
        }
    }
}

fn resolve_executable(executable: &OsStr) -> Result<PathBuf, ExecutorError> {
    if executable.as_bytes().contains(&b'/') {
        return executable_candidate(PathBuf::from(executable))
            .ok_or_else(|| ExecutorError::ExecutableNotFound(executable.to_owned()));
    }

    let path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path) {
        if let Some(candidate) = executable_candidate(directory.join(executable)) {
            return Ok(candidate);
        }
    }
    Err(ExecutorError::ExecutableNotFound(executable.to_owned()))
}

fn executable_candidate(path: PathBuf) -> Option<PathBuf> {
    let path = fs::canonicalize(path).ok()?;
    let metadata = fs::metadata(&path).ok()?;
    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
        Some(path)
    } else {
        None
    }
}

fn os_string_to_cstring(value: OsString) -> Result<CString, ExecutorError> {
    CString::new(value.into_vec()).map_err(|_| ExecutorError::NulByte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepares_shell_free_argv() {
        let command = PreparedCommand::new(
            vec![OsString::from("/bin/echo"), OsString::from("hello world")],
            None,
        )
        .unwrap();
        assert_eq!(command.argv.len(), 2);
        assert_eq!(command.argv[1].to_bytes(), b"hello world");
        assert!(command.argv_pointers.last().unwrap().is_null());
    }

    #[test]
    fn rejects_empty_command() {
        assert!(matches!(
            PreparedCommand::new(Vec::new(), None),
            Err(ExecutorError::EmptyCommand)
        ));
    }

    #[test]
    fn decodes_exit_and_signal_status() {
        assert_eq!(decode_wait_status(7 << 8).exit_code, Some(7));
        assert_eq!(
            decode_wait_status(libc::SIGKILL).signal,
            Some(libc::SIGKILL)
        );
    }
}
