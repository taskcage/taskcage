//! shell-free argv를 준비하고 clone3로 target을 작업 cgroup 안에서 생성한다.

use std::collections::BTreeMap;
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};

use taskcage_core::output::{CaptureLimits, CapturedOutput};

use crate::cleanup_fault::{CleanupFaultPoint, CleanupFaults};

use super::capture::{OutputReaders, PreparedOutputReader, set_nonblocking};
use super::wait::{ProcessExit, wait_blocking};
use super::{CleanupFaultHandle, ExecutorError};

const CLONE_INTO_CGROUP: u64 = 0x0002_0000_0000;
const EMPTY_EXEC_PATH: &[u8] = b"\0";

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

#[derive(Debug)]
/// clone3 뒤의 자식이 추가 준비 없이 바로 execve를 호출할 수 있게 만든 명령이다.
pub struct PreparedCommand {
    executable: PreparedExecutable,
    pub(super) argv: Vec<CString>,
    pub(super) environment: Vec<CString>,
    working_directory: CString,
}

#[derive(Debug)]
enum PreparedExecutable {
    Path(CString),
    Descriptor(Arc<File>),
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
        // 셸 문자열로 다시 해석하지 않고 실행 파일과 각 인자를 그대로 보존한다.
        // API의 program은 절대 경로이므로 daemon PATH 검색이나 사전 canonicalize를 하지 않는다.
        let executable =
            PreparedExecutable::Path(os_string_to_cstring(original_executable.to_owned())?);
        Self::prepare(executable, command, working_directory, environment)
    }

    /// 검증된 Runtime Package entrypoint descriptor를 PATH 재해석 없이 준비한다.
    pub fn new_pinned(
        descriptor: Arc<File>,
        command: Vec<OsString>,
        working_directory: &Path,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self, ExecutorError> {
        if command.is_empty() {
            return Err(ExecutorError::EmptyCommand);
        }
        Self::prepare(
            PreparedExecutable::Descriptor(descriptor),
            command,
            working_directory,
            environment,
        )
    }

    fn prepare(
        executable: PreparedExecutable,
        command: Vec<OsString>,
        working_directory: &Path,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self, ExecutorError> {
        if !working_directory.is_absolute() {
            return Err(ExecutorError::WorkingDirectoryNotAbsolute(
                working_directory.as_os_str().to_owned(),
            ));
        }
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

pub(super) fn nul_terminated_pointers(values: &[CString]) -> Vec<*const libc::c_char> {
    values
        .iter()
        .map(|value| value.as_ptr())
        .chain(std::iter::once(ptr::null()))
        .collect()
}

#[derive(Debug)]
pub struct SpawnedProcess {
    pub(super) pid: libc::pid_t,
    pub(super) output_readers: OutputReaders,
    #[cfg(target_os = "linux")]
    pub(super) cleanup_faults: CleanupFaultHandle,
    #[cfg(target_os = "linux")]
    pub(super) reaped_exit: Mutex<Option<ProcessExit>>,
}

#[derive(Debug)]
pub struct PendingProcess {
    pub(super) pid: libc::pid_t,
    pub(super) exec_read_end: Option<OwnedFd>,
    pub(super) start_write_end: Option<OwnedFd>,
    pub(super) output_readers: Option<OutputReaders>,
    #[cfg(target_os = "linux")]
    pub(super) cleanup_faults: CleanupFaultHandle,
    pub(super) reaped: bool,
}

#[derive(Debug)]
pub struct StartCommitToken {
    pid: libc::pid_t,
}

#[derive(Debug)]
pub struct StartCommittedProcess {
    pub(super) pid: libc::pid_t,
    pub(super) exec_read_end: Option<OwnedFd>,
    pub(super) output_readers: Option<OutputReaders>,
    #[cfg(target_os = "linux")]
    pub(super) cleanup_faults: CleanupFaultHandle,
}

impl PendingProcess {
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// fail-stop 상태 잠금 안에서는 gate 신호 한 번만 기록하고 정리는 수행하지 않는다.
    pub fn commit_start_signal(&mut self) -> Result<StartCommitToken, ExecutorError> {
        #[cfg(target_os = "linux")]
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

    pub fn into_start_committed(mut self, token: StartCommitToken) -> StartCommittedProcess {
        debug_assert_eq!(self.pid, token.pid);
        let committed = StartCommittedProcess {
            pid: self.pid,
            exec_read_end: self.exec_read_end.take(),
            output_readers: self.output_readers.take(),
            #[cfg(target_os = "linux")]
            cleanup_faults: self.cleanup_faults.clone(),
        };
        // child 회수 책임은 start-committed owner로 이동했다.
        self.reaped = true;
        committed
    }
}
impl StartCommittedProcess {
    /// gate commit 뒤에만 exec 결과를 읽으며 이 대기 동안 coordinator 잠금은 잡지 않는다.
    pub fn wait_for_exec(mut self) -> Result<SpawnOutcome, ExecutorError> {
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
                #[cfg(target_os = "linux")]
                cleanup_faults: self.cleanup_faults.clone(),
                #[cfg(target_os = "linux")]
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

    pub(super) fn stop_and_reap(&mut self) -> Result<(), ExecutorError> {
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let wait_result = wait_blocking(self.pid);
        self.exec_read_end.take();
        if let Some(output_readers) = self.output_readers.take() {
            output_readers.cancel_and_join();
        }
        wait_result.map(|_| ())
    }
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
    #[cfg(target_os = "linux")]
    let cleanup_faults = None;
    #[cfg(not(target_os = "linux"))]
    let cleanup_faults = CleanupFaultHandle;
    spawn_in_cgroup_with_fault_handle(command, cgroup_fd, capture_limits, cleanup_faults)
}

#[cfg(target_os = "linux")]
pub fn spawn_in_cgroup_with_cleanup_faults(
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
        #[cfg(target_os = "linux")]
        cleanup_faults,
        reaped: false,
    })
}

pub(super) fn pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)> {
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
        match &command.executable {
            PreparedExecutable::Path(executable) => {
                libc::execve(executable.as_ptr(), argv_pointers, environment_pointers);
            }
            PreparedExecutable::Descriptor(descriptor) => {
                libc::syscall(
                    libc::SYS_execveat,
                    descriptor.as_raw_fd(),
                    EMPTY_EXEC_PATH.as_ptr().cast::<libc::c_char>(),
                    argv_pointers,
                    environment_pointers,
                    libc::AT_EMPTY_PATH,
                );
            }
        }
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

fn os_string_to_cstring(value: OsString) -> Result<CString, ExecutorError> {
    CString::new(value.into_vec()).map_err(|_| ExecutorError::NulByte)
}
