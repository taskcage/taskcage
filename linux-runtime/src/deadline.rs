//! 정리 단계가 함께 사용하는 단조시간 절대 기한이다.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonotonicDeadline {
    at: Instant,
    budget: Duration,
}

impl MonotonicDeadline {
    pub fn from_now(budget: Duration) -> Option<Self> {
        Self::from_start(Instant::now(), budget)
    }

    pub fn from_start(start: Instant, budget: Duration) -> Option<Self> {
        if budget.is_zero() {
            return None;
        }
        Some(Self {
            at: start.checked_add(budget)?,
            budget,
        })
    }

    pub fn expired_at(now: Instant) -> Self {
        Self {
            at: now,
            budget: Duration::ZERO,
        }
    }

    pub fn at(self) -> Instant {
        self.at
    }

    pub fn budget(self) -> Duration {
        self.budget
    }

    pub fn remaining(self) -> Option<Duration> {
        self.remaining_at(Instant::now())
    }

    pub fn remaining_at(self, now: Instant) -> Option<Duration> {
        self.at
            .checked_duration_since(now)
            .filter(|value| !value.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_absolute_deadline_only_loses_remaining_time() {
        let start = Instant::now();
        let deadline = MonotonicDeadline::from_start(start, Duration::from_secs(5)).unwrap();

        assert_eq!(
            deadline.remaining_at(start + Duration::from_secs(2)),
            Some(Duration::from_secs(3))
        );
        assert_eq!(deadline.remaining_at(start + Duration::from_secs(5)), None);
    }

    #[test]
    fn zero_and_unrepresentable_deadlines_are_rejected() {
        let start = Instant::now();
        assert!(MonotonicDeadline::from_start(start, Duration::ZERO).is_none());
        assert!(MonotonicDeadline::from_start(start, Duration::MAX).is_none());
    }
}
