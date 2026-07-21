//! `clone3(CLONE_INTO_CGROUP)`로 target을 생성 순간부터 작업 cgroup 안에 둔다.
//!
//! clone3 뒤의 자식 실행 흐름에서는 메모리 할당, 잠금, 비동기 실행기와 구조화 로그를
//! 사용하지 않는다. 필요한 문자열과 포인터는 부모가 모두 준비한다.

use std::collections::BTreeMap;
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use thiserror::Error;
use tokio::time::sleep;

use crate::output::{BoundedTail, CaptureLimits, CapturedOutput, CapturedStream};

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
    #[error("실행 파일은 절대 경로여야 합니다: {0:?}")]
    ProgramNotAbsolute(OsString),
    #[error("작업 디렉터리는 절대 경로여야 합니다: {0:?}")]
    WorkingDirectoryNotAbsolute(OsString),
    #[error("환경 변수 이름은 비어 있거나 '=' 문자를 포함할 수 없습니다: {0:?}")]
    InvalidEnvironmentKey(OsString),
    #[error("실행 오류 전달용 관을 만들지 못했습니다")]
    ExecPipe(#[source] io::Error),
    #[error("{stream} 출력 관을 준비하지 못했습니다")]
    OutputPipe {
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{stream} 출력 reader thread를 시작하지 못했습니다")]
    OutputReaderStart {
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("clone3로 작업 cgroup 안에 프로세스를 만들지 못했습니다")]
    Clone(#[source] io::Error),
    #[error("target execve가 실패했습니다: errno {0}")]
    Exec(i32),
    #[error("자식 프로세스 종료 상태를 회수하지 못했습니다")]
    Wait(#[source] io::Error),
    #[error("자식이 잘못된 실행 오류 값을 보냈습니다")]
    InvalidExecPayload,
    #[error("{stream} 출력을 읽지 못했습니다")]
    OutputRead {
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{stream} 출력 reader thread가 panic으로 종료됐습니다")]
    OutputReaderPanicked { stream: &'static str },
    #[error("출력 reader가 {0:?} 안에 종료되지 않았습니다")]
    OutputReaderTimeout(Duration),
}

#[derive(Debug)]
/// clone3 뒤의 자식이 추가 준비 없이 바로 execve를 호출할 수 있게 만든 명령이다.
pub struct PreparedCommand {
    executable: CString,
    argv: Vec<CString>,
    argv_pointers: Vec<*const libc::c_char>,
    environment: Vec<CString>,
    environment_pointers: Vec<*const libc::c_char>,
    working_directory: CString,
}

impl PreparedCommand {
    pub fn new(
        command: Vec<OsString>,
        working_directory: &Path,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self, ExecutorError> {
        let original_executable = command.first().ok_or(ExecutorError::EmptyCommand)?;
        if !Path::new(original_executable).is_absolute() {
            return Err(ExecutorError::ProgramNotAbsolute(
                original_executable.to_owned(),
            ));
        }
        if !working_directory.is_absolute() {
            return Err(ExecutorError::WorkingDirectoryNotAbsolute(
                working_directory.as_os_str().to_owned(),
            ));
        }
        // 셸 문자열로 다시 해석하지 않고 실행 파일과 각 인자를 그대로 보존한다.
        // API의 program은 절대 경로이므로 daemon PATH 검색이나 사전 canonicalize를 하지 않는다.
        let executable = os_string_to_cstring(original_executable.to_owned())?;
        let argv: Vec<_> = command
            .into_iter()
            .map(os_string_to_cstring)
            .collect::<Result<_, _>>()?;
        // target은 daemon 환경을 상속하지 않는다. 호출자가 명시한 환경만 execve에 전달한다.
        let environment: Vec<_> = environment
            .into_iter()
            .map(|(key, value)| {
                if key.is_empty() || key.as_bytes().contains(&b'=') {
                    return Err(ExecutorError::InvalidEnvironmentKey(key));
                }
                let mut entry = key.into_vec();
                entry.push(b'=');
                entry.extend(value.into_vec());
                CString::new(entry).map_err(|_| ExecutorError::NulByte)
            })
            .collect::<Result<_, _>>()?;
        let working_directory = os_string_to_cstring(working_directory.as_os_str().to_owned())?;

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

#[derive(Debug)]
pub struct SpawnedProcess {
    pid: libc::pid_t,
    output_readers: OutputReaders,
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

    pub async fn finish_output(
        self,
        timeout_duration: Duration,
    ) -> Result<CapturedOutput, ExecutorError> {
        self.output_readers.collect(timeout_duration).await
    }
}

#[derive(Debug)]
struct PreparedOutputReader {
    descriptor: OwnedFd,
    limit: std::num::NonZeroUsize,
}

impl PreparedOutputReader {
    fn new(
        descriptor: OwnedFd,
        limit: std::num::NonZeroUsize,
        stream: &'static str,
    ) -> Result<Self, ExecutorError> {
        set_nonblocking(descriptor.as_raw_fd())
            .map_err(|source| ExecutorError::OutputPipe { stream, source })?;
        Ok(Self { descriptor, limit })
    }

    fn start(
        self,
        stream: &'static str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<thread::JoinHandle<Result<CapturedStream, io::Error>>, ExecutorError> {
        thread::Builder::new()
            .name(format!("taskcage-{stream}-reader"))
            .spawn(move || drain_output(self.descriptor, self.limit, &cancelled))
            .map_err(|source| ExecutorError::OutputReaderStart { stream, source })
    }
}

#[derive(Debug)]
struct OutputReaders {
    cancelled: Arc<AtomicBool>,
    stdout: Option<thread::JoinHandle<Result<CapturedStream, io::Error>>>,
    stderr: Option<thread::JoinHandle<Result<CapturedStream, io::Error>>>,
}

impl OutputReaders {
    fn start(
        stdout: PreparedOutputReader,
        stderr: PreparedOutputReader,
    ) -> Result<Self, ExecutorError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let stdout = stdout.start("stdout", Arc::clone(&cancelled))?;
        let stderr = match stderr.start("stderr", Arc::clone(&cancelled)) {
            Ok(stderr) => stderr,
            Err(error) => {
                cancelled.store(true, Ordering::Release);
                let _ = stdout.join();
                return Err(error);
            }
        };
        Ok(Self {
            cancelled,
            stdout: Some(stdout),
            stderr: Some(stderr),
        })
    }

    async fn collect(
        mut self,
        timeout_duration: Duration,
    ) -> Result<CapturedOutput, ExecutorError> {
        let deadline = Instant::now() + timeout_duration;
        let timed_out = loop {
            let stdout_finished = self
                .stdout
                .as_ref()
                .expect("stdout reader가 존재합니다")
                .is_finished();
            let stderr_finished = self
                .stderr
                .as_ref()
                .expect("stderr reader가 존재합니다")
                .is_finished();
            if stdout_finished && stderr_finished {
                break false;
            }
            if Instant::now() >= deadline {
                self.cancelled.store(true, Ordering::Release);
                break true;
            }
            sleep(Duration::from_millis(10)).await;
        };

        // reader는 nonblocking read와 50 ms poll만 사용하므로 취소 뒤 join이 제한 없이
        // 막히지 않는다. 반환 전에 반드시 join해서 thread와 FD를 남기지 않는다.
        let stdout_result = self
            .stdout
            .take()
            .expect("stdout reader가 존재합니다")
            .join();
        let stderr_result = self
            .stderr
            .take()
            .expect("stderr reader가 존재합니다")
            .join();
        if timed_out {
            return Err(ExecutorError::OutputReaderTimeout(timeout_duration));
        }

        let stdout = join_output_reader("stdout", stdout_result)?;
        let stderr = join_output_reader("stderr", stderr_result)?;
        Ok(CapturedOutput { stdout, stderr })
    }

    fn cancel_and_join(mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
}

impl Drop for OutputReaders {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
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
    capture_limits: CaptureLimits,
) -> Result<SpawnedProcess, ExecutorError> {
    let (exec_read_end, exec_write_end) = pipe_cloexec().map_err(ExecutorError::ExecPipe)?;
    let (stdout_read_end, stdout_write_end) =
        pipe_cloexec().map_err(|source| ExecutorError::OutputPipe {
            stream: "stdout",
            source,
        })?;
    let (stderr_read_end, stderr_write_end) =
        pipe_cloexec().map_err(|source| ExecutorError::OutputPipe {
            stream: "stderr",
            source,
        })?;
    let stdout_reader = PreparedOutputReader::new(
        stdout_read_end,
        capture_limits.stdout_tail_max_bytes(),
        "stdout",
    )?;
    let stderr_reader = PreparedOutputReader::new(
        stderr_read_end,
        capture_limits.stderr_tail_max_bytes(),
        "stderr",
    )?;
    let output_readers = OutputReaders::start(stdout_reader, stderr_reader)?;
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
        let error = io::Error::last_os_error();
        output_readers.cancel_and_join();
        return Err(ExecutorError::Clone(error));
    }
    if result == 0 {
        child_exec(
            command,
            exec_read_end.as_raw_fd(),
            exec_write_end.as_raw_fd(),
            stdout_write_end.as_raw_fd(),
            stderr_write_end.as_raw_fd(),
            parent_pid,
        );
    }

    drop(exec_write_end);
    drop(stdout_write_end);
    drop(stderr_write_end);
    let pid = result as libc::pid_t;
    let mut payload = Vec::with_capacity(size_of::<i32>());
    if let Err(error) = File::from(exec_read_end).read_to_end(&mut payload) {
        // 오류 통로를 읽지 못해도 직접 만든 자식을 좀비로 남기지 않는다. 작업 cgroup의
        // 다른 프로세스는 호출자가 이어서 전체 종료한다.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let _ = wait_blocking(pid);
        output_readers.cancel_and_join();
        return Err(ExecutorError::ExecPipe(error));
    }
    if payload.is_empty() {
        Ok(SpawnedProcess {
            pid,
            output_readers,
        })
    } else if payload.len() == size_of::<i32>() {
        let errno = i32::from_ne_bytes(payload.try_into().expect("길이를 먼저 확인했습니다"));
        let _ = wait_blocking(pid);
        output_readers.cancel_and_join();
        Err(ExecutorError::Exec(errno))
    } else {
        let _ = wait_blocking(pid);
        output_readers.cancel_and_join();
        Err(ExecutorError::InvalidExecPayload)
    }
}

fn pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    for descriptor in &mut descriptors {
        if *descriptor <= libc::STDERR_FILENO {
            let replacement = unsafe { libc::fcntl(*descriptor, libc::F_DUPFD_CLOEXEC, 3) };
            if replacement == -1 {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::close(descriptors[0]);
                    libc::close(descriptors[1]);
                }
                return Err(error);
            }
            unsafe { libc::close(*descriptor) };
            *descriptor = replacement;
        }
    }
    let read_end = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    Ok((read_end, write_end))
}

fn child_exec(
    command: &PreparedCommand,
    read_fd: RawFd,
    write_fd: RawFd,
    stdout_fd: RawFd,
    stderr_fd: RawFd,
    expected_parent: libc::pid_t,
) -> ! {
    // 이 아래의 자식 경로는 할당, 잠금, 로그 없이 낮은 수준의 시스템 호출만 사용한다.
    unsafe {
        libc::close(read_fd);
        if libc::dup2(stdout_fd, libc::STDOUT_FILENO) == -1 {
            write_errno_and_exit(write_fd, current_errno_or(libc::EBADF));
        }
        if libc::dup2(stderr_fd, libc::STDERR_FILENO) == -1 {
            write_errno_and_exit(write_fd, current_errno_or(libc::EBADF));
        }
        libc::close(stdout_fd);
        libc::close(stderr_fd);
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1
            || libc::getppid() != expected_parent
        {
            write_errno_and_exit(write_fd, current_errno_or(libc::ESRCH));
        }
        if libc::chdir(command.working_directory.as_ptr()) == -1 {
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

fn set_nonblocking(descriptor: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain_output(
    descriptor: OwnedFd,
    limit: std::num::NonZeroUsize,
    cancelled: &AtomicBool,
) -> Result<CapturedStream, io::Error> {
    let mut tail = BoundedTail::new(limit);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(tail.finish());
        }
        let mut ready = libc::pollfd {
            fd: descriptor.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let poll_result = unsafe { libc::poll(&mut ready, 1, 50) };
        if poll_result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if poll_result == 0 {
            continue;
        }
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(tail.finish());
            }
            let read = unsafe {
                libc::read(
                    descriptor.as_raw_fd(),
                    buffer.as_mut_ptr().cast::<libc::c_void>(),
                    buffer.len(),
                )
            };
            if read == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if read == 0 {
                return Ok(tail.finish());
            }
            tail.push(&buffer[..read as usize]);
        }
    }
}

fn join_output_reader(
    stream: &'static str,
    result: thread::Result<Result<CapturedStream, io::Error>>,
) -> Result<CapturedStream, ExecutorError> {
    match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(source)) => Err(ExecutorError::OutputRead { stream, source }),
        Err(_) => Err(ExecutorError::OutputReaderPanicked { stream }),
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

fn os_string_to_cstring(value: OsString) -> Result<CString, ExecutorError> {
    CString::new(value.into_vec()).map_err(|_| ExecutorError::NulByte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::num::NonZeroUsize;

    #[test]
    fn prepares_shell_free_argv() {
        let command = PreparedCommand::new(
            vec![OsString::from("/bin/echo"), OsString::from("hello world")],
            Path::new("/tmp"),
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(command.argv.len(), 2);
        assert_eq!(command.argv[1].to_bytes(), b"hello world");
        assert!(command.argv_pointers.last().unwrap().is_null());
    }

    #[test]
    fn rejects_empty_command() {
        assert!(matches!(
            PreparedCommand::new(Vec::new(), Path::new("/tmp"), BTreeMap::new()),
            Err(ExecutorError::EmptyCommand)
        ));
    }

    #[test]
    fn rejects_relative_program_and_working_directory() {
        assert!(matches!(
            PreparedCommand::new(
                vec![OsString::from("echo")],
                Path::new("/tmp"),
                BTreeMap::new(),
            ),
            Err(ExecutorError::ProgramNotAbsolute(_))
        ));
        assert!(matches!(
            PreparedCommand::new(
                vec![OsString::from("/bin/echo")],
                Path::new("relative"),
                BTreeMap::new(),
            ),
            Err(ExecutorError::WorkingDirectoryNotAbsolute(_))
        ));
    }

    #[test]
    fn uses_only_explicit_target_environment() {
        let environment = BTreeMap::from([(OsString::from("LANG"), OsString::from("C.UTF-8"))]);
        let command = PreparedCommand::new(
            vec![OsString::from("/bin/echo")],
            Path::new("/tmp"),
            environment,
        )
        .unwrap();

        assert_eq!(command.environment.len(), 1);
        assert_eq!(command.environment[0].as_bytes(), b"LANG=C.UTF-8");
    }

    #[test]
    fn rejects_invalid_environment_keys() {
        let environment = BTreeMap::from([(OsString::new(), OsString::from("value"))]);
        assert!(matches!(
            PreparedCommand::new(
                vec![OsString::from("/bin/echo")],
                Path::new("/tmp"),
                environment,
            ),
            Err(ExecutorError::InvalidEnvironmentKey(_))
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drains_large_stdout_and_stderr_concurrently() {
        let (stdout_read, stdout_write) = pipe_cloexec().unwrap();
        let (stderr_read, stderr_write) = pipe_cloexec().unwrap();
        let readers = OutputReaders::start(
            PreparedOutputReader::new(stdout_read, NonZeroUsize::new(32).unwrap(), "stdout")
                .unwrap(),
            PreparedOutputReader::new(stderr_read, NonZeroUsize::new(32).unwrap(), "stderr")
                .unwrap(),
        )
        .unwrap();

        let stdout = thread::spawn(move || write_test_flood(stdout_write, b'O', b"STDOUT-END\n"));
        let stderr = thread::spawn(move || write_test_flood(stderr_write, b'E', b"STDERR-END\n"));
        stdout.join().unwrap().unwrap();
        stderr.join().unwrap().unwrap();

        let output = readers.collect(Duration::from_secs(2)).await.unwrap();
        assert!(output.stdout.raw_tail().ends_with(b"STDOUT-END\n"));
        assert!(output.stderr.raw_tail().ends_with(b"STDERR-END\n"));
        assert_eq!(output.stdout.raw_tail().len(), 32);
        assert_eq!(output.stderr.raw_tail().len(), 32);
        assert!(output.stdout.truncated());
        assert!(output.stderr.truncated());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reader_timeout_cancels_threads_with_open_writers() {
        let (stdout_read, stdout_write) = pipe_cloexec().unwrap();
        let (stderr_read, stderr_write) = pipe_cloexec().unwrap();
        let readers = OutputReaders::start(
            PreparedOutputReader::new(stdout_read, NonZeroUsize::new(8).unwrap(), "stdout")
                .unwrap(),
            PreparedOutputReader::new(stderr_read, NonZeroUsize::new(8).unwrap(), "stderr")
                .unwrap(),
        )
        .unwrap();

        let error = readers
            .collect(Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(matches!(error, ExecutorError::OutputReaderTimeout(_)));
        drop(stdout_write);
        drop(stderr_write);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reader_descriptors_close_after_collection() {
        let (stdout_read, stdout_write) = pipe_cloexec().unwrap();
        let (stderr_read, stderr_write) = pipe_cloexec().unwrap();
        let stdout_fd = stdout_read.as_raw_fd();
        let stderr_fd = stderr_read.as_raw_fd();
        let readers = OutputReaders::start(
            PreparedOutputReader::new(stdout_read, NonZeroUsize::new(8).unwrap(), "stdout")
                .unwrap(),
            PreparedOutputReader::new(stderr_read, NonZeroUsize::new(8).unwrap(), "stderr")
                .unwrap(),
        )
        .unwrap();
        drop(stdout_write);
        drop(stderr_write);

        readers.collect(Duration::from_secs(1)).await.unwrap();
        assert_eq!(unsafe { libc::fcntl(stdout_fd, libc::F_GETFD) }, -1);
        assert_eq!(unsafe { libc::fcntl(stderr_fd, libc::F_GETFD) }, -1);
    }

    fn write_test_flood(descriptor: OwnedFd, byte: u8, marker: &[u8]) -> io::Result<()> {
        let mut output = File::from(descriptor);
        let chunk = [byte; 8 * 1024];
        for _ in 0..256 {
            output.write_all(&chunk)?;
        }
        output.write_all(marker)
    }
}
