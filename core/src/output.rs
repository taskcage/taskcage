//! stdout과 stderr의 제한된 raw tail을 전달하는 backend-independent 값이다.

use std::num::NonZeroUsize;

#[derive(Debug, Clone, Copy)]
/// resource budget adapter가 검증한 두 stream 상한만 실행기에 전달한다.
pub struct CaptureLimits {
    stdout_tail_max_bytes: NonZeroUsize,
    stderr_tail_max_bytes: NonZeroUsize,
}

impl CaptureLimits {
    pub fn new(stdout_tail_max_bytes: NonZeroUsize, stderr_tail_max_bytes: NonZeroUsize) -> Self {
        Self {
            stdout_tail_max_bytes,
            stderr_tail_max_bytes,
        }
    }

    pub fn stdout_tail_max_bytes(self) -> NonZeroUsize {
        self.stdout_tail_max_bytes
    }

    pub fn stderr_tail_max_bytes(self) -> NonZeroUsize {
        self.stderr_tail_max_bytes
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedStream {
    tail: Vec<u8>,
    truncated: bool,
}

impl CapturedStream {
    pub fn raw_tail(&self) -> &[u8] {
        &self.tail
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedOutput {
    pub stdout: CapturedStream,
    pub stderr: CapturedStream,
}

impl CapturedOutput {
    #[cfg(target_os = "linux")]
    pub fn for_test(
        stdout_tail: Vec<u8>,
        stdout_truncated: bool,
        stderr_tail: Vec<u8>,
        stderr_truncated: bool,
    ) -> Self {
        Self {
            stdout: CapturedStream {
                tail: stdout_tail,
                truncated: stdout_truncated,
            },
            stderr: CapturedStream {
                tail: stderr_tail,
                truncated: stderr_truncated,
            },
        }
    }
}
