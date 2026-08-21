//! Per-server reconnect backoff.

use std::time::{Duration, Instant};

/// The first delay, and the one a server with no recorded failures gets.
pub(super) const BACKOFF_BASE: Duration = Duration::from_secs(5);

/// The longest delay between attempts.
///
/// Capped rather than unbounded because the reason a server is down is usually
/// somewhere else entirely — its operator may fix it without anyone touching
/// this host, and an exponential curve with no ceiling would leave it
/// unreachable for hours afterwards.
pub(super) const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// How long to wait after `failures` consecutive failed reconnects.
///
/// `BACKOFF_BASE * 2^(failures - 1)`, capped. Zero failures yields the base
/// delay, which is the "nothing has gone wrong yet" case.
pub(super) fn delay_after(failures: u32) -> Duration {
    if failures == 0 {
        return BACKOFF_BASE;
    }

    // Saturating throughout: a server that has been failing for weeks must
    // produce a long delay, not an overflowed short one.
    let shifted = BACKOFF_BASE
        .as_secs()
        .saturating_mul(1_u64.checked_shl(failures - 1).unwrap_or(u64::MAX));

    Duration::from_secs(shifted.min(BACKOFF_MAX.as_secs()))
}

/// What is known about one server's recent reconnect attempts.
#[derive(Debug, Default, Clone)]
pub(super) struct BackoffState {
    /// Consecutive failures.
    pub(super) failures: u32,
    /// The earliest time another attempt may be made.
    next_attempt_at: Option<Instant>,
}

impl BackoffState {
    /// Whether an attempt may be made at `now`.
    ///
    /// A state with no recorded failure is always ready.
    pub(super) fn ready(&self, now: Instant) -> bool {
        self.next_attempt_at.is_none_or(|earliest| now >= earliest)
    }

    /// Records a failure and schedules the next attempt.
    pub(super) fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.next_attempt_at = Some(now + delay_after(self.failures));
    }

    /// The delay this state's failure count currently implies.
    pub(super) fn current_delay(&self) -> Duration {
        delay_after(self.failures)
    }
}
