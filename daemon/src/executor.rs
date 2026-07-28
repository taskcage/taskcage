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
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

use serde::Serialize;
use thiserror::Error;
use tokio::time::sleep;

#[cfg(test)]
use crate::cleanup_fault::{CleanupFaultPoint, CleanupFaults};
use crate::deadline::MonotonicDeadline;
use crate::output::{BoundedTail, CaptureLimits, CapturedOutput, CapturedStream};

#[cfg(test)]
type CleanupFaultHandle = Option<Arc<CleanupFaults>>;
#[cfg(not(test))]
#[derive(Clone, Debug)]
struct CleanupFaultHandle;

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

#[derive(Clone, Copy)]
struct ChildDescriptors {
    exec_read: RawFd,
    exec_write: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    start_read: RawFd,
    start_write: RawFd,
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
    #[error("exec 시작 게이트용 관을 만들지 못했습니다")]
    StartGatePipe(#[source] io::Error),
    #[error("검증이 끝난 target의 exec 시작을 허용하지 못했습니다")]
    StartGateSignal(#[source] io::Error),
    #[error("exec 시작 게이트가 이미 처리됐습니다")]
    StartGateAlreadyResolved,
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
    environment: Vec<CString>,
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

        Ok(Self {
            executable,
            argv,
            environment,
            working_directory,
        })
    }
}

fn nul_terminated_pointers(values: &[CString]) -> Vec<*const libc::c_char> {
    values
        .iter()
        .map(|value| value.as_ptr())
        .chain(std::iter::once(ptr::null()))
        .collect()
}

#[derive(Debug)]
pub struct SpawnedProcess {
    pid: libc::pid_t,
    output_readers: OutputReaders,
    #[cfg(test)]
    cleanup_faults: CleanupFaultHandle,
    #[cfg(test)]
    reaped_exit: Mutex<Option<ProcessExit>>,
}

#[derive(Debug)]
pub struct PendingProcess {
    pid: libc::pid_t,
    exec_read_end: Option<OwnedFd>,
    start_write_end: Option<OwnedFd>,
    output_readers: Option<OutputReaders>,
    #[cfg(test)]
    cleanup_faults: CleanupFaultHandle,
    reaped: bool,
}

#[derive(Debug)]
pub(crate) struct StartCommitToken {
    pid: libc::pid_t,
}

#[derive(Debug)]
pub(crate) struct StartCommittedProcess {
    pid: libc::pid_t,
    exec_read_end: Option<OwnedFd>,
    output_readers: Option<OutputReaders>,
    #[cfg(test)]
    cleanup_faults: CleanupFaultHandle,
}

impl PendingProcess {
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// fail-stop 상태 잠금 안에서는 gate 신호 한 번만 기록하고 정리는 수행하지 않는다.
    pub(crate) fn commit_start_signal(&mut self) -> Result<StartCommitToken, ExecutorError> {
        #[cfg(test)]
        if self.cleanup_faults.as_ref().is_some_and(|faults| {
            faults.is(CleanupFaultPoint::PendingCloneAbort)
                || faults.should_fail(CleanupFaultPoint::ExecGateCleanup)
        }) {
            return Err(ExecutorError::StartGateSignal(CleanupFaults::error(
                CleanupFaultPoint::ExecGateCleanup,
            )));
        }
        let start_write_end = self
            .start_write_end
            .take()
            .ok_or(ExecutorError::StartGateAlreadyResolved)?;
        if let Err(source) = write_start_signal_once(start_write_end.as_raw_fd()) {
            drop(start_write_end);
            return Err(ExecutorError::StartGateSignal(source));
        }
        drop(start_write_end);
        Ok(StartCommitToken { pid: self.pid })
    }

    pub(crate) fn into_start_committed(mut self, token: StartCommitToken) -> StartCommittedProcess {
        debug_assert_eq!(self.pid, token.pid);
        let committed = StartCommittedProcess {
            pid: self.pid,
            exec_read_end: self.exec_read_end.take(),
            output_readers: self.output_readers.take(),
            #[cfg(test)]
            cleanup_faults: self.cleanup_faults.clone(),
        };
        // child 회수 책임은 start-committed owner로 이동했다.
        self.reaped = true;
        committed
    }

    pub(crate) async fn abort_until(
        &mut self,
        deadline: MonotonicDeadline,
    ) -> Result<(), ExecutorError> {
        self.start_write_end.take();
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        #[cfg(test)]
        let injected = self
            .cleanup_faults
            .as_ref()
            .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::PendingCloneAbort));
        #[cfg(not(test))]
        let injected = false;
        let wait_result = if injected {
            #[cfg(test)]
            {
                Err(ExecutorError::Wait(CleanupFaults::error(
                    CleanupFaultPoint::PendingCloneAbort,
                )))
            }
            #[cfg(not(test))]
            {
                unreachable!("운영 빌드에는 cleanup fault가 없습니다")
            }
        } else {
            loop {
                if let Some(status) = wait_nohang(self.pid)? {
                    break Ok(status);
                }
                let Some(remaining) = deadline.remaining() else {
                    break Err(ExecutorError::Wait(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "pending clone3 child의 종료 상태를 회수하지 못했습니다",
                    )));
                };
                sleep(remaining.min(Duration::from_millis(10))).await;
            }
        };
        self.exec_read_end.take();
        if let Some(output_readers) = self.output_readers.take() {
            output_readers.cancel_and_join();
        }
        if wait_result.is_ok() {
            self.reaped = true;
        }
        wait_result.map(|_| ())
    }

    fn stop_and_reap(&mut self) -> Result<(), ExecutorError> {
        self.start_write_end.take();
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let wait_result = wait_blocking(self.pid);
        self.exec_read_end.take();
        if let Some(output_readers) = self.output_readers.take() {
            output_readers.cancel_and_join();
        }
        if wait_result.is_ok() {
            self.reaped = true;
        }
        wait_result.map(|_| ())
    }
}

impl StartCommittedProcess {
    /// gate commit 뒤에만 exec 결과를 읽으며 이 대기 동안 coordinator 잠금은 잡지 않는다.
    pub(crate) fn wait_for_exec(mut self) -> Result<SpawnOutcome, ExecutorError> {
        let exec_read_end = self
            .exec_read_end
            .take()
            .expect("실행 오류 통로가 존재합니다");
        let mut payload = Vec::with_capacity(size_of::<i32>());
        if let Err(error) = File::from(exec_read_end).read_to_end(&mut payload) {
            self.stop_and_reap()?;
            return Err(ExecutorError::ExecPipe(error));
        }
        let output_readers = self
            .output_readers
            .take()
            .expect("출력 reader가 존재합니다");
        if payload.is_empty() {
            Ok(SpawnOutcome::Started(SpawnedProcess {
                pid: self.pid,
                output_readers,
                #[cfg(test)]
                cleanup_faults: self.cleanup_faults.clone(),
                #[cfg(test)]
                reaped_exit: Mutex::new(None),
            }))
        } else if payload.len() == size_of::<i32>() {
            let errno = i32::from_ne_bytes(payload.try_into().expect("길이를 먼저 확인했습니다"));
            let wait_result = wait_blocking(self.pid);
            let output_result = output_readers.cancel_and_collect();
            wait_result?;
            let output = output_result?;
            Ok(SpawnOutcome::ExecFailed(ExecFailure {
                pid: self.pid,
                errno,
                output,
            }))
        } else {
            let _ = wait_blocking(self.pid);
            output_readers.cancel_and_join();
            Err(ExecutorError::InvalidExecPayload)
        }
    }

    fn stop_and_reap(&mut self) -> Result<(), ExecutorError> {
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let wait_result = wait_blocking(self.pid);
        self.exec_read_end.take();
        if let Some(output_readers) = self.output_readers.take() {
            output_readers.cancel_and_join();
        }
        wait_result.map(|_| ())
    }
}

impl Drop for PendingProcess {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.stop_and_reap();
        }
    }
}

impl Drop for StartCommittedProcess {
    fn drop(&mut self) {
        if self.output_readers.is_some() {
            let _ = self.stop_and_reap();
        }
    }
}

impl SpawnedProcess {
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    pub(crate) async fn wait_until(
        &self,
        deadline: MonotonicDeadline,
    ) -> Result<WaitOutcome, ExecutorError> {
        loop {
            let Some(remaining) = deadline.remaining() else {
                return Ok(WaitOutcome::TimedOut);
            };
            if let Some(status) = wait_nohang(self.pid)? {
                return Ok(WaitOutcome::Exited(status));
            }
            sleep(remaining.min(Duration::from_millis(10))).await;
        }
    }

    pub(crate) async fn reap_after_kill_until(
        &self,
        deadline: MonotonicDeadline,
    ) -> Result<ProcessExit, ExecutorError> {
        #[cfg(test)]
        {
            if self
                .cleanup_faults
                .as_ref()
                .is_some_and(|faults| faults.should_fail(CleanupFaultPoint::DirectChildReap))
            {
                let mut cached = self.reaped_exit.lock().expect("시험용 child 상태 잠금");
                if cached.is_none() {
                    *cached = Some(wait_blocking(self.pid)?);
                }
                return Err(ExecutorError::Wait(CleanupFaults::error(
                    CleanupFaultPoint::DirectChildReap,
                )));
            }
            if let Some(exit) = *self.reaped_exit.lock().expect("시험용 child 상태 잠금") {
                return Ok(exit);
            }
        }
        loop {
            if let Some(status) = wait_nohang(self.pid)? {
                return Ok(status);
            }
            let Some(remaining) = deadline.remaining() else {
                return Err(ExecutorError::Wait(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "cgroup 전체 종료 뒤 target의 종료 상태를 회수하지 못했습니다",
                )));
            };
            sleep(remaining.min(Duration::from_millis(10))).await;
        }
    }

    pub(crate) async fn finish_output_until(
        self,
        deadline: MonotonicDeadline,
    ) -> Result<CapturedOutput, ExecutorError> {
        self.output_readers.collect_until(deadline).await
    }
}

#[derive(Debug)]
struct PreparedOutputReader {
    descriptor: OwnedFd,
    limit: std::num::NonZeroUsize,
    cleanup_faults: CleanupFaultHandle,
}

impl PreparedOutputReader {
    fn new(
        descriptor: OwnedFd,
        limit: std::num::NonZeroUsize,
        stream: &'static str,
        cleanup_faults: CleanupFaultHandle,
    ) -> Result<Self, ExecutorError> {
        set_nonblocking(descriptor.as_raw_fd())
            .map_err(|source| ExecutorError::OutputPipe { stream, source })?;
        Ok(Self {
            descriptor,
            limit,
            cleanup_faults,
        })
    }

    fn start(
        self,
        stream: &'static str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<thread::JoinHandle<Result<CapturedStream, io::Error>>, ExecutorError> {
        let cleanup_faults = self.cleanup_faults;
        thread::Builder::new()
            .name(format!("taskcage-{stream}-reader"))
            .spawn(move || {
                drain_output(
                    self.descriptor,
                    self.limit,
                    &cancelled,
                    stream,
                    cleanup_faults,
                )
            })
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

    async fn collect_until(
        mut self,
        deadline: MonotonicDeadline,
    ) -> Result<CapturedOutput, ExecutorError> {
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
            let Some(remaining) = deadline.remaining() else {
                self.cancelled.store(true, Ordering::Release);
                break true;
            };
            sleep(remaining.min(Duration::from_millis(10))).await;
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
            return Err(ExecutorError::OutputReaderTimeout(deadline.budget()));
        }

        let stdout = join_output_reader("stdout", stdout_result)?;
        let stderr = join_output_reader("stderr", stderr_result)?;
        Ok(CapturedOutput { stdout, stderr })
    }

    #[cfg(test)]
    async fn collect(self, timeout: Duration) -> Result<CapturedOutput, ExecutorError> {
        let deadline = MonotonicDeadline::from_now(timeout)
            .ok_or(ExecutorError::OutputReaderTimeout(timeout))?;
        self.collect_until(deadline).await
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

    fn cancel_and_collect(mut self) -> Result<CapturedOutput, ExecutorError> {
        self.cancelled.store(true, Ordering::Release);
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

        let stdout = join_output_reader("stdout", stdout_result)?;
        let stderr = join_output_reader("stderr", stderr_result)?;
        Ok(CapturedOutput { stdout, stderr })
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

#[derive(Debug)]
pub struct ExecFailure {
    pub pid: libc::pid_t,
    pub errno: i32,
    pub output: CapturedOutput,
}

#[derive(Debug)]
pub enum SpawnOutcome {
    Started(SpawnedProcess),
    ExecFailed(ExecFailure),
}

pub fn spawn_in_cgroup(
    command: &PreparedCommand,
    cgroup_fd: RawFd,
    capture_limits: CaptureLimits,
) -> Result<PendingProcess, ExecutorError> {
    #[cfg(test)]
    let cleanup_faults = None;
    #[cfg(not(test))]
    let cleanup_faults = CleanupFaultHandle;
    spawn_in_cgroup_with_fault_handle(command, cgroup_fd, capture_limits, cleanup_faults)
}

#[cfg(test)]
pub(crate) fn spawn_in_cgroup_with_cleanup_faults(
    command: &PreparedCommand,
    cgroup_fd: RawFd,
    capture_limits: CaptureLimits,
    cleanup_faults: Option<Arc<CleanupFaults>>,
) -> Result<PendingProcess, ExecutorError> {
    spawn_in_cgroup_with_fault_handle(command, cgroup_fd, capture_limits, cleanup_faults)
}

fn spawn_in_cgroup_with_fault_handle(
    command: &PreparedCommand,
    cgroup_fd: RawFd,
    capture_limits: CaptureLimits,
    cleanup_faults: CleanupFaultHandle,
) -> Result<PendingProcess, ExecutorError> {
    let (exec_read_end, exec_write_end) = pipe_cloexec().map_err(ExecutorError::ExecPipe)?;
    let (start_read_end, start_write_end) = pipe_cloexec().map_err(ExecutorError::StartGatePipe)?;
    // coordinator 잠금 안의 한 바이트 기록이 대기 상태에 빠지지 않게 부모 쓰기 끝만 비차단으로 둔다.
    set_nonblocking(start_write_end.as_raw_fd()).map_err(ExecutorError::StartGatePipe)?;
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
        cleanup_faults.clone(),
    )?;
    let stderr_reader = PreparedOutputReader::new(
        stderr_read_end,
        capture_limits.stderr_tail_max_bytes(),
        "stderr",
        cleanup_faults.clone(),
    )?;
    let output_readers = OutputReaders::start(stdout_reader, stderr_reader)?;
    // clone3 전에만 할당한다. 자식은 복제된 포인터 배열을 그대로 execve에 사용한다.
    let argv_pointers = nul_terminated_pointers(&command.argv);
    let environment_pointers = nul_terminated_pointers(&command.environment);
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
            argv_pointers.as_ptr(),
            environment_pointers.as_ptr(),
            ChildDescriptors {
                exec_read: exec_read_end.as_raw_fd(),
                exec_write: exec_write_end.as_raw_fd(),
                stdout: stdout_write_end.as_raw_fd(),
                stderr: stderr_write_end.as_raw_fd(),
                start_read: start_read_end.as_raw_fd(),
                start_write: start_write_end.as_raw_fd(),
            },
            parent_pid,
        );
    }

    drop(exec_write_end);
    drop(start_read_end);
    drop(stdout_write_end);
    drop(stderr_write_end);
    let pid = result as libc::pid_t;
    Ok(PendingProcess {
        pid,
        exec_read_end: Some(exec_read_end),
        start_write_end: Some(start_write_end),
        output_readers: Some(output_readers),
        #[cfg(test)]
        cleanup_faults,
        reaped: false,
    })
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
    argv_pointers: *const *const libc::c_char,
    environment_pointers: *const *const libc::c_char,
    descriptors: ChildDescriptors,
    expected_parent: libc::pid_t,
) -> ! {
    let ChildDescriptors {
        exec_read: read_fd,
        exec_write: write_fd,
        stdout: stdout_fd,
        stderr: stderr_fd,
        start_read: start_read_fd,
        start_write: start_write_fd,
    } = descriptors;
    // 이 아래의 자식 경로는 할당, 잠금, 로그 없이 낮은 수준의 시스템 호출만 사용한다.
    unsafe {
        libc::close(read_fd);
        libc::close(start_write_fd);
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
        let mut start = 0_u8;
        loop {
            let read = libc::read(
                start_read_fd,
                (&mut start as *mut u8).cast::<libc::c_void>(),
                1,
            );
            if read == 1 {
                break;
            }
            if read == -1 && current_errno_or(libc::EIO) == libc::EINTR {
                continue;
            }
            write_errno_and_exit(write_fd, libc::ECANCELED);
        }
        libc::close(start_read_fd);
        if libc::chdir(command.working_directory.as_ptr()) == -1 {
            write_errno_and_exit(write_fd, current_errno_or(libc::ENOENT));
        }
        libc::execve(
            command.executable.as_ptr(),
            argv_pointers,
            environment_pointers,
        );
        write_errno_and_exit(write_fd, current_errno_or(libc::ENOEXEC));
    }
}

fn write_start_signal_once(descriptor: RawFd) -> io::Result<()> {
    let signal = [1_u8];
    let written = unsafe {
        libc::write(
            descriptor,
            signal.as_ptr().cast::<libc::c_void>(),
            signal.len(),
        )
    };
    match written {
        1 => Ok(()),
        -1 => Err(io::Error::last_os_error()),
        _ => Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "exec 시작 게이트에 신호를 쓰지 못했습니다",
        )),
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
    _stream: &'static str,
    _cleanup_faults: CleanupFaultHandle,
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
            #[cfg(test)]
            {
                let point = if _stream == "stdout" {
                    CleanupFaultPoint::StdoutReader
                } else {
                    CleanupFaultPoint::StderrReader
                };
                if _cleanup_faults
                    .as_ref()
                    .is_some_and(|faults| faults.should_fail(point))
                {
                    return Err(CleanupFaults::error(point));
                }
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
    fn injected_stdout_and_stderr_reader_failures_stay_independent() {
        for stream in ["stdout", "stderr"] {
            let error = join_output_reader(
                stream,
                Ok(Err(io::Error::other(format!("injected {stream} failure")))),
            )
            .unwrap_err();

            assert!(matches!(
                error,
                ExecutorError::OutputRead {
                    stream: actual,
                    ..
                } if actual == stream
            ));
        }
    }

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
        let pointers = nul_terminated_pointers(&command.argv);
        assert!(pointers.last().unwrap().is_null());
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
            PreparedOutputReader::new(stdout_read, NonZeroUsize::new(32).unwrap(), "stdout", None)
                .unwrap(),
            PreparedOutputReader::new(stderr_read, NonZeroUsize::new(32).unwrap(), "stderr", None)
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
            PreparedOutputReader::new(stdout_read, NonZeroUsize::new(8).unwrap(), "stdout", None)
                .unwrap(),
            PreparedOutputReader::new(stderr_read, NonZeroUsize::new(8).unwrap(), "stderr", None)
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
        let stdout_identity = descriptor_identity(stdout_fd).unwrap();
        let stderr_identity = descriptor_identity(stderr_fd).unwrap();
        let readers = OutputReaders::start(
            PreparedOutputReader::new(stdout_read, NonZeroUsize::new(8).unwrap(), "stdout", None)
                .unwrap(),
            PreparedOutputReader::new(stderr_read, NonZeroUsize::new(8).unwrap(), "stderr", None)
                .unwrap(),
        )
        .unwrap();
        drop(stdout_write);
        drop(stderr_write);

        readers.collect(Duration::from_secs(1)).await.unwrap();
        assert_descriptor_released(stdout_fd, stdout_identity);
        assert_descriptor_released(stderr_fd, stderr_identity);
    }

    fn descriptor_identity(descriptor: RawFd) -> io::Result<(libc::dev_t, libc::ino_t)> {
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(descriptor, metadata.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let metadata = unsafe { metadata.assume_init() };
        Ok((metadata.st_dev, metadata.st_ino))
    }

    fn assert_descriptor_released(
        descriptor: RawFd,
        original_identity: (libc::dev_t, libc::ino_t),
    ) {
        match descriptor_identity(descriptor) {
            Err(error) if error.raw_os_error() == Some(libc::EBADF) => {}
            Ok(current_identity) => assert_ne!(
                current_identity, original_identity,
                "원래 output pipe descriptor가 아직 열려 있습니다"
            ),
            Err(error) => panic!("output pipe descriptor 상태를 확인하지 못했습니다: {error}"),
        }
    }

    fn write_test_flood(descriptor: OwnedFd, byte: u8, marker: &[u8]) -> io::Result<()> {
        let mut output = File::from(descriptor);
        let chunk = [byte; 8 * 1024];
        for _ in 0..256 {
            output.write_all(&chunk)?;
        }
        output.write_all(marker)
    }

    #[test]
    fn expired_execution_deadline_has_no_remaining_budget() {
        let now = Instant::now();
        let deadline = MonotonicDeadline::expired_at(now);

        assert_eq!(deadline.remaining_at(now), None);
    }
}
