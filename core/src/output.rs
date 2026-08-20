//! stdout과 stderr의 마지막 raw bytes만 제한된 크기로 보관한다.

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

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
pub(crate) struct BoundedTail {
    buffer: Box<[u8]>,
    start: usize,
    len: usize,
    truncated: bool,
}

#[cfg(any(target_os = "linux", test))]
impl BoundedTail {
    pub(crate) fn new(limit: NonZeroUsize) -> Self {
        Self {
            buffer: vec![0; limit.get()].into_boxed_slice(),
            start: 0,
            len: 0,
            truncated: false,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
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

    pub(crate) fn finish(self) -> CapturedStream {
        let mut tail = self.buffer.into_vec();
        tail.rotate_left(self.start);
        tail.truncate(self.len);
        CapturedStream {
            tail,
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(limit: usize, chunks: &[&[u8]]) -> CapturedStream {
        let mut tail = BoundedTail::new(NonZeroUsize::new(limit).unwrap());
        for chunk in chunks {
            tail.push(chunk);
        }
        tail.finish()
    }

    #[test]
    fn empty_input_stays_empty() {
        let captured = tail(4, &[]);
        assert_eq!(captured.raw_tail(), b"");
        assert!(!captured.truncated());
    }

    #[test]
    fn smaller_and_exact_input_are_not_truncated() {
        let smaller = tail(4, &[b"abc"]);
        let exact = tail(4, &[b"abcd"]);

        assert_eq!(smaller.raw_tail(), b"abc");
        assert!(!smaller.truncated());
        assert_eq!(exact.raw_tail(), b"abcd");
        assert!(!exact.truncated());
    }

    #[test]
    fn one_byte_over_limit_keeps_the_exact_tail() {
        let captured = tail(4, &[b"abcde"]);
        assert_eq!(captured.raw_tail(), b"bcde");
        assert!(captured.truncated());
    }

    #[test]
    fn much_larger_input_never_retains_more_than_the_limit() {
        let input: Vec<_> = (0_u8..100).collect();
        let captured = tail(8, &[&input]);

        assert_eq!(captured.raw_tail(), &input[92..]);
        assert_eq!(captured.raw_tail().len(), 8);
        assert!(captured.tail.capacity() <= 8);
        assert!(captured.truncated());
    }

    #[test]
    fn multiple_chunks_preserve_the_last_bytes() {
        let captured = tail(6, &[b"ab", b"cde", b"f", b"gh"]);
        assert_eq!(captured.raw_tail(), b"cdefgh");
        assert!(captured.truncated());
    }

    #[test]
    fn invalid_utf8_is_preserved_until_backend_mapping() {
        let stdout = tail(3, &[&[0xff, b'a']]);
        assert_eq!(stdout.raw_tail(), &[0xff, b'a']);
        let captured = CapturedOutput {
            stdout,
            stderr: tail(1, &[]),
        };

        assert_eq!(captured.stdout.raw_tail(), &[0xff, b'a']);
    }

    #[test]
    fn split_multibyte_character_at_tail_start_is_preserved() {
        let stdout = tail(2, &["€".as_bytes()]);
        assert_eq!(stdout.raw_tail(), &[0x82, 0xac]);
        let captured = CapturedOutput {
            stdout,
            stderr: tail(1, &[]),
        };

        assert_eq!(captured.stdout.raw_tail(), &[0x82, 0xac]);
    }

    #[test]
    fn stdout_and_stderr_keep_independent_state() {
        let captured = CapturedOutput {
            stdout: tail(3, &[b"abcdef"]),
            stderr: tail(3, &[b"xy"]),
        };

        assert_eq!(captured.stdout.raw_tail(), b"def");
        assert!(captured.stdout.truncated());
        assert_eq!(captured.stderr.raw_tail(), b"xy");
        assert!(!captured.stderr.truncated());
    }
}
