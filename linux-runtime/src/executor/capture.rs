//! stdout과 stderr를 동시에 비우고 각 stream의 제한된 raw tail을 수집한다.

use std::io;
use std::num::NonZeroUsize;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use taskcage_core::output::CapturedOutput;
use tokio::time::sleep;

use crate::cleanup_fault::{CleanupFaultPoint, CleanupFaults};
use crate::deadline::MonotonicDeadline;

use super::{CleanupFaultHandle, ExecutorError};

#[derive(Debug)]
pub(super) struct CapturedTail {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug)]
struct BoundedTail {
    buffer: Box<[u8]>,
    start: usize,
    len: usize,
    truncated: bool,
}

impl BoundedTail {
    fn new(limit: NonZeroUsize) -> Self {
        Self {
            buffer: vec![0; limit.get()].into_boxed_slice(),
            start: 0,
            len: 0,
            truncated: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let capacity = self.buffer.len();
        if bytes.len() >= capacity {
            if self.len > 0 || bytes.len() > capacity {
                self.truncated = true;
            }
            self.buffer
                .copy_from_slice(&bytes[bytes.len() - capacity..]);
            self.start = 0;
            self.len = capacity;
            return;
        }
        let free = capacity - self.len;
        if bytes.len() > free {
            let discarded = bytes.len() - free;
            self.start = (self.start + discarded) % capacity;
            self.len -= discarded;
            self.truncated = true;
        }
        let write_start = (self.start + self.len) % capacity;
        let first = bytes.len().min(capacity - write_start);
        self.buffer[write_start..write_start + first].copy_from_slice(&bytes[..first]);
        self.buffer[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        self.len += bytes.len();
    }

    fn finish(self) -> CapturedTail {
        let mut bytes = self.buffer.into_vec();
        bytes.rotate_left(self.start);
        bytes.truncate(self.len);
        CapturedTail {
            bytes,
            truncated: self.truncated,
        }
    }
}

#[derive(Debug)]
pub(super) struct PreparedOutputReader {
    descriptor: OwnedFd,
    limit: std::num::NonZeroUsize,
    cleanup_faults: CleanupFaultHandle,
}

impl PreparedOutputReader {
    pub(super) fn new(
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
    ) -> Result<thread::JoinHandle<Result<CapturedTail, io::Error>>, ExecutorError> {
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
pub(super) struct OutputReaders {
    cancelled: Arc<AtomicBool>,
    stdout: Option<thread::JoinHandle<Result<CapturedTail, io::Error>>>,
    stderr: Option<thread::JoinHandle<Result<CapturedTail, io::Error>>>,
}

impl OutputReaders {
    pub(super) fn start(
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

    pub(super) async fn collect_until(
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
        Ok(CapturedOutput::for_test(
            stdout.bytes,
            stdout.truncated,
            stderr.bytes,
            stderr.truncated,
        ))
    }

    #[cfg(test)]
    pub(super) async fn collect(self, timeout: Duration) -> Result<CapturedOutput, ExecutorError> {
        let deadline = MonotonicDeadline::from_now(timeout)
            .ok_or(ExecutorError::OutputReaderTimeout(timeout))?;
        self.collect_until(deadline).await
    }

    pub(super) fn cancel_and_join(mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }

    pub(super) fn cancel_and_collect(mut self) -> Result<CapturedOutput, ExecutorError> {
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
        Ok(CapturedOutput::for_test(
            stdout.bytes,
            stdout.truncated,
            stderr.bytes,
            stderr.truncated,
        ))
    }
}

impl Drop for OutputReaders {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

pub(super) fn set_nonblocking(descriptor: RawFd) -> io::Result<()> {
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
) -> Result<CapturedTail, io::Error> {
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
            #[cfg(target_os = "linux")]
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

pub(super) fn join_output_reader(
    stream: &'static str,
    result: thread::Result<Result<CapturedTail, io::Error>>,
) -> Result<CapturedTail, ExecutorError> {
    match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(source)) => Err(ExecutorError::OutputRead { stream, source }),
        Err(_) => Err(ExecutorError::OutputReaderPanicked { stream }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(limit: usize, chunks: &[&[u8]]) -> CapturedTail {
        let mut tail = BoundedTail::new(NonZeroUsize::new(limit).unwrap());
        for chunk in chunks {
            tail.push(chunk);
        }
        tail.finish()
    }

    #[test]
    fn empty_input_stays_empty() {
        let captured = tail(4, &[]);
        assert_eq!(captured.bytes, b"");
        assert!(!captured.truncated);
    }

    #[test]
    fn smaller_and_exact_input_are_not_truncated() {
        let smaller = tail(4, &[b"abc"]);
        let exact = tail(4, &[b"abcd"]);

        assert_eq!(smaller.bytes, b"abc");
        assert!(!smaller.truncated);
        assert_eq!(exact.bytes, b"abcd");
        assert!(!exact.truncated);
    }

    #[test]
    fn one_byte_over_limit_keeps_the_exact_tail() {
        let captured = tail(4, &[b"abcde"]);
        assert_eq!(captured.bytes, b"bcde");
        assert!(captured.truncated);
    }

    #[test]
    fn much_larger_input_never_retains_more_than_the_limit() {
        let input: Vec<_> = (0_u8..100).collect();
        let captured = tail(8, &[&input]);

        assert_eq!(captured.bytes, &input[92..]);
        assert_eq!(captured.bytes.len(), 8);
        assert!(captured.bytes.capacity() <= 8);
        assert!(captured.truncated);
    }

    #[test]
    fn multiple_chunks_preserve_the_last_bytes() {
        let captured = tail(6, &[b"ab", b"cde", b"f", b"gh"]);
        assert_eq!(captured.bytes, b"cdefgh");
        assert!(captured.truncated);
    }

    #[test]
    fn invalid_utf8_is_preserved() {
        let captured = tail(3, &[&[0xff, b'a']]);
        assert_eq!(captured.bytes, &[0xff, b'a']);
    }

    #[test]
    fn split_multibyte_character_at_tail_start_is_preserved() {
        let captured = tail(2, &["€".as_bytes()]);
        assert_eq!(captured.bytes, &[0x82, 0xac]);
    }

    #[test]
    fn stdout_and_stderr_keep_independent_state() {
        let stdout = tail(3, &[b"abcdef"]);
        let stderr = tail(3, &[b"xy"]);

        assert_eq!(stdout.bytes, b"def");
        assert!(stdout.truncated);
        assert_eq!(stderr.bytes, b"xy");
        assert!(!stderr.truncated);
    }
}
