//! 실제 cleanup 경계에만 연결하는 시험 전용 오류 주입 계획이다.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupFaultPoint {
    PendingCloneAbort,
    ExecGateCleanup,
    CgroupKill,
    DirectChildReap,
    PopulatedZero,
    Statistics,
    CgroupRemoval,
    StdoutReader,
    StderrReader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupFaultMode {
    Once,
    Persistent,
}

#[derive(Debug)]
pub(crate) struct CleanupFaults {
    primary: CleanupFaultPoint,
    secondary: Option<CleanupFaultPoint>,
    mode: CleanupFaultMode,
    primary_attempts: AtomicUsize,
    secondary_attempts: AtomicUsize,
    enabled: AtomicBool,
}

impl CleanupFaults {
    pub(crate) fn new(point: CleanupFaultPoint, mode: CleanupFaultMode) -> Self {
        Self {
            primary: point,
            secondary: None,
            mode,
            primary_attempts: AtomicUsize::new(0),
            secondary_attempts: AtomicUsize::new(0),
            enabled: AtomicBool::new(true),
        }
    }

    pub(crate) fn new_pair(
        primary: CleanupFaultPoint,
        secondary: CleanupFaultPoint,
        mode: CleanupFaultMode,
    ) -> Self {
        debug_assert_ne!(primary, secondary);
        Self {
            primary,
            secondary: Some(secondary),
            mode,
            primary_attempts: AtomicUsize::new(0),
            secondary_attempts: AtomicUsize::new(0),
            enabled: AtomicBool::new(true),
        }
    }

    pub(crate) fn should_fail(&self, point: CleanupFaultPoint) -> bool {
        let attempt = if self.primary == point {
            self.primary_attempts.fetch_add(1, Ordering::AcqRel)
        } else if self.secondary == Some(point) {
            self.secondary_attempts.fetch_add(1, Ordering::AcqRel)
        } else {
            return false;
        };
        self.enabled.load(Ordering::Acquire)
            && (matches!(self.mode, CleanupFaultMode::Persistent) || attempt == 0)
    }

    pub(crate) fn is(&self, point: CleanupFaultPoint) -> bool {
        self.primary == point || self.secondary == Some(point)
    }

    pub(crate) fn attempts(&self) -> usize {
        self.primary_attempts.load(Ordering::Acquire)
            + self.secondary_attempts.load(Ordering::Acquire)
    }

    pub(crate) fn attempts_for(&self, point: CleanupFaultPoint) -> usize {
        if self.primary == point {
            self.primary_attempts.load(Ordering::Acquire)
        } else if self.secondary == Some(point) {
            self.secondary_attempts.load(Ordering::Acquire)
        } else {
            0
        }
    }

    pub(crate) fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    pub(crate) fn error(point: CleanupFaultPoint) -> io::Error {
        io::Error::other(format!("injected cleanup fault at {point:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn once_and_persistent_modes_have_deterministic_call_counts() {
        let once = CleanupFaults::new(CleanupFaultPoint::CgroupKill, CleanupFaultMode::Once);
        assert!(once.should_fail(CleanupFaultPoint::CgroupKill));
        assert!(!once.should_fail(CleanupFaultPoint::CgroupKill));
        assert_eq!(once.attempts(), 2);

        let persistent =
            CleanupFaults::new(CleanupFaultPoint::CgroupKill, CleanupFaultMode::Persistent);
        assert!(persistent.should_fail(CleanupFaultPoint::CgroupKill));
        assert!(persistent.should_fail(CleanupFaultPoint::CgroupKill));
        assert_eq!(persistent.attempts(), 2);

        let pair = CleanupFaults::new_pair(
            CleanupFaultPoint::StdoutReader,
            CleanupFaultPoint::StderrReader,
            CleanupFaultMode::Persistent,
        );
        assert!(pair.should_fail(CleanupFaultPoint::StdoutReader));
        assert!(pair.should_fail(CleanupFaultPoint::StderrReader));
        assert_eq!(pair.attempts_for(CleanupFaultPoint::StdoutReader), 1);
        assert_eq!(pair.attempts_for(CleanupFaultPoint::StderrReader), 1);
    }
}
