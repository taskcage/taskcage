//! clone3 기반 실행의 준비, 시작, 대기, 출력 수집과 정리를 제공한다.

use std::ffi::OsString;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::cleanup_fault::CleanupFaults;

mod capture;
mod cleanup;
mod spawn;
mod wait;

type CleanupFaultHandle = Option<Arc<CleanupFaults>>;

pub use spawn::{
    ExecFailure, PendingProcess, PreparedCommand, SpawnOutcome, SpawnedProcess, StartCommitToken,
    StartCommittedProcess, spawn_in_cgroup, spawn_in_cgroup_with_cleanup_faults,
};
pub use wait::{ProcessExit, WaitOutcome};

#[cfg(test)]
use capture::{OutputReaders, PreparedOutputReader, join_output_reader};
#[cfg(test)]
use spawn::{nul_terminated_pointers, pipe_cloexec};
#[cfg(test)]
use wait::decode_wait_status;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::io::Write;
    use std::num::NonZeroUsize;
    use std::os::fd::{AsRawFd, OwnedFd, RawFd};
    use std::path::Path;
    use std::thread;
    use std::time::Instant;

    use crate::deadline::MonotonicDeadline;

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
