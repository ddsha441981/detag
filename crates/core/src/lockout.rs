//! Login lockout curve (P1-004, FR-A-01): the exponential
//! backoff after failed logins.
//!
//! The numbers are product-policy constants, not config (the
//! P1-001 precedent — no doc specified them):
//!
//! | constant | value | meaning |
//! | --- | --- | --- |
//! | [`LOCKOUT_THRESHOLD`] | 5 | failures before the first lock |
//! | [`LOCKOUT_WINDOW_SECS`] | 900 (15 min) | failures older than this don't count — a failure after 15 min of quiet starts a new window |
//! | [`LOCKOUT_BASE_SECS`] | 60 | the first lock lasts 1 minute |
//! | [`LOCKOUT_MAX_SECS`] | 3600 (1 h) | the lock never exceeds 1 hour |
//!
//! The curve ([`lock_seconds_for`]): the first lock (5
//! failures) is 60 s, then it doubles per further failure —
//! 120, 240, 480, 960, 1920 — capped at 3600 s. The doubling
//! is the backoff: a legitimate user who mistypes five times
//! waits a minute; a brute-force that keeps failing waits
//! longer and longer, and the account is never locked
//! forever (the cap) — a lockout must not become a denial of
//! service against the real owner.
//!
//! The curve lives here (pure, tested) so the repo is its only
//! caller and the SQL never re-implements the math: the repo
//! calls [`lock_seconds_for`] and stores the result.

/// Failures before the first lock.
pub const LOCKOUT_THRESHOLD: i64 = 5;
/// The failure window: a failure more than this many seconds
/// after the previous one starts a new window (attempts = 1).
pub const LOCKOUT_WINDOW_SECS: i64 = 15 * 60;
/// The first lock's duration, in seconds.
pub const LOCKOUT_BASE_SECS: i64 = 60;
/// The longest any lock lasts, in seconds.
pub const LOCKOUT_MAX_SECS: i64 = 60 * 60;

/// The lock duration for a failure count, in seconds — or
/// `None` when the count is below the threshold (no lock).
/// Doubling from the base, capped at the max: 5 → 60, 6 → 120,
/// 7 → 240, 8 → 480, 9 → 960, 10 → 1920, 11+ → 3600.
pub fn lock_seconds_for(attempts: i64) -> Option<i64> {
    if attempts < LOCKOUT_THRESHOLD {
        return None;
    }
    let shift = attempts - LOCKOUT_THRESHOLD;
    // shift >= 0; cap the shift so the multiply cannot overflow
    // (the cap makes anything past 11 identical anyway).
    let shift = shift.min(6);
    let seconds = LOCKOUT_BASE_SECS.saturating_mul(1_i64 << shift);
    Some(seconds.min(LOCKOUT_MAX_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_curve_is_exponential_then_capped() {
        assert_eq!(lock_seconds_for(0), None);
        assert_eq!(lock_seconds_for(4), None);
        assert_eq!(lock_seconds_for(5), Some(60));
        assert_eq!(lock_seconds_for(6), Some(120));
        assert_eq!(lock_seconds_for(7), Some(240));
        assert_eq!(lock_seconds_for(8), Some(480));
        assert_eq!(lock_seconds_for(9), Some(960));
        assert_eq!(lock_seconds_for(10), Some(1920));
        assert_eq!(lock_seconds_for(11), Some(3600));
        assert_eq!(lock_seconds_for(12), Some(3600));
        assert_eq!(lock_seconds_for(100), Some(3600));
    }

    #[test]
    fn constants_are_the_documented_values() {
        assert_eq!(LOCKOUT_THRESHOLD, 5);
        assert_eq!(LOCKOUT_WINDOW_SECS, 900);
        assert_eq!(LOCKOUT_BASE_SECS, 60);
        assert_eq!(LOCKOUT_MAX_SECS, 3600);
    }
}
