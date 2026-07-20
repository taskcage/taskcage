//! `clone3(CLONE_INTO_CGROUP)`로 새 프로세스를 처음부터 작업 cgroup 안에 만든 뒤
//! `execve`로 요청받은 프로그램을 실행한다.
//!
//! `clone3` 뒤의 자식 프로세스는 메모리 할당이나 잠금을 건드리지 않는다.
//! 여러 실행 흐름이 있던 부모를 복제한 직후에는 이런 동작이 멈춤 상태를 만들 수 있으므로,
//! 자식에서는 안전하다고 정해진 낮은 수준의 시스템 호출만 사용한다.

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

const CLONE_INTO_CGROUP: u64 = 0x200000000;

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
    #[error("command must contain an executable")]
    EmptyCommand,
    #[error("command contains a NUL byte")]
    NulByte,
    #[error("executable was not found or is not executable: {0:?}")]
    ExecutableNotFound(OsString),
    #[error("create exec error pipe failed")]
    Pipe(#[source] io::Error),
    #[error("clone3(CLONE_INTO_CGROUP) failed")]
    Clone(#[source] io::Error),
    #[error("target execve failed with errno {0}")]
    Exec(i32),
    #[error("waitpid failed")]
    Wait(#[source] io::Error),
    #[error("exec error pipe returned an invalid payload")]
    InvalidExecPayload,
}

#[derive(Debug)]
/// 자식 프로세스가 별도 준비 작업 없이 바로 실행할 수 있도록 미리 만든 명령 정보다.
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
        // 셸을 거치지 않고 실행 파일을 직접 찾는다. 따라서 공백이나 특수 문자가 명령으로
        // 다시 해석되지 않고 각각의 인자가 입력받은 그대로 전달된다.
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
        // `execve`는 끝에 빈 포인터가 붙은 C 형식 배열을 요구한다.
        // 포인터가 가리키는 문자열은 이후 바꾸지 않아 자식 프로세스에서도 그대로 쓸 수 있다.
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
/// 부모가 기다리고 종료 상태를 회수해야 하는 자식 프로세스다.
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
            // 실행 흐름을 막지 않도록 종료 여부만 확인하고, 아직 실행 중이면 잠깐 양보한다.
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
        // cgroup 전체에 종료 신호를 보낸 뒤에도 부모는 직접 만든 자식의 종료 상태를 회수해야 한다.
        // 그렇지 않으면 종료된 프로세스 정보가 커널에 남을 수 있다.
        match self.wait_for(timeout).await? {
            WaitOutcome::Exited(status) => Ok(status),
            WaitOutcome::TimedOut => Err(ExecutorError::Wait(io::Error::new(
                io::ErrorKind::TimedOut,
                "target did not become waitable after cgroup.kill",
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
    // 이 관은 자식이 `execve`에 실패한 경우 오류 번호를 부모에게 전하는 용도다.
    // 실행에 성공하면 `O_CLOEXEC` 때문에 쓰기 쪽이 자동으로 닫혀 부모는 빈 값을 읽는다.
    let (read_end, write_end) = pipe_cloexec()?;
    // 부모가 너무 일찍 죽는 짧은 틈도 잡아내기 위해 복제 전에 PID를 기억한다.
    let parent_pid = unsafe { libc::getpid() };
    let args = CloneArgs {
        flags: CLONE_INTO_CGROUP,
        exit_signal: libc::SIGCHLD as u64,
        cgroup: cgroup_fd as u64,
        ..CloneArgs::default()
    };

    // 일반적인 `fork` 뒤에 cgroup으로 옮기면 아주 짧게 제한 밖에서 실행될 수 있다.
    // `CLONE_INTO_CGROUP`은 생성 순간부터 지정한 cgroup 안에 넣어 이 틈을 없앤다.
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
        // 반환값 0은 자식 실행 흐름이다. 이 함수는 성공하든 실패하든 부모 코드로 돌아오지 않는다.
        child_exec(
            command,
            read_end.as_raw_fd(),
            write_end.as_raw_fd(),
            parent_pid,
        );
    }

    // 부모가 쓰기 쪽을 들고 있으면 자식이 성공해 관을 닫아도 읽기가 끝나지 않는다.
    drop(write_end);
    let pid = result as libc::pid_t;
    let mut payload = Vec::with_capacity(size_of::<i32>());
    File::from(read_end)
        .read_to_end(&mut payload)
        .map_err(ExecutorError::Pipe)?;
    if payload.is_empty() {
        // 빈 값은 `execve`가 성공해 오류 전달용 관이 자동으로 닫혔다는 뜻이다.
        Ok(SpawnedProcess { pid })
    } else if payload.len() == size_of::<i32>() {
        // 자식이 보낸 오류 번호를 읽고, 종료 상태까지 회수한 뒤 실행 실패로 돌려준다.
        let errno = i32::from_ne_bytes(payload.try_into().expect("length checked"));
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
    // 이 지점은 `clone3` 직후의 자식이다. Rust의 메모리 할당, 로그, 비동기 실행기를
    // 사용하지 않고 아래의 시스템 호출만 거친 뒤 즉시 새 프로그램으로 바뀌거나 종료한다.
    unsafe {
        libc::close(read_fd);
        // 부모가 먼저 죽으면 자식도 함께 끝낸다. 신호 설정 직전에 부모가 죽는 경쟁 상황은
        // 바로 이어지는 `getppid` 비교로 한 번 더 확인한다.
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1
            || libc::getppid() != expected_parent
        {
            write_errno_and_exit(write_fd, current_errno_or(libc::ESRCH));
        }
        if let Some(directory) = &command.working_directory {
            if libc::chdir(directory.as_ptr()) == -1 {
                write_errno_and_exit(write_fd, current_errno_or(libc::ENOENT));
            }
        }
        // 성공하면 현재 자식 프로세스가 요청한 프로그램으로 완전히 바뀌며 이 아래로 돌아오지 않는다.
        libc::execve(
            command.executable.as_ptr(),
            command.argv_pointers.as_ptr(),
            command.environment_pointers.as_ptr(),
        );
        write_errno_and_exit(write_fd, current_errno_or(libc::ENOEXEC));
    }
}

unsafe fn write_errno_and_exit(write_fd: RawFd, errno: i32) -> ! {
    // 복잡한 오류 처리를 하지 않고 고정 크기 숫자만 부모에게 보낸 뒤 즉시 종료한다.
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
        // 신호 때문에 잠깐 끊긴 경우에는 다시 시도하고, 실제 오류만 호출자에게 돌려준다.
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
    // 경로 구분자가 있으면 입력한 경로만 확인하고, 단순 이름이면 환경 변수 `PATH`를 차례로 찾는다.
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
