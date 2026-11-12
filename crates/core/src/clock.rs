//! Clocks (P0-063, E0.7).
//!
//! Time in this codebase is `chrono` with explicit `Utc` (coding
//! standards): never `SystemTime::now()` scattered, and never
//! `Utc::now()` inside a service — inject a [`Clock`] instead, so
//! tests can freeze time. Handlers and services take
//! `Arc<dyn Clock>` from [`AppState`](crate::state) — in
//! production that is [`SystemClock`]; tests use [`ManualClock`].

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration, Utc};

/// A source of "now". Object-safe (`Arc<dyn Clock>` is the state
/// field) and `Debug` (so `AppState` can derive `Debug`/`Clone`).
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The current time, always in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// The real clock: the system time, in UTC.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A frozen clock for tests: `now()` returns the same instant
/// until [`set`](ManualClock::set) or
/// [`advance`](ManualClock::advance) moves it. Cheap to clone —
/// clones share the same instant (one `Arc` behind them), so a
/// test can move time while the code under test holds a clone.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<RwLock<DateTime<Utc>>>,
}

impl ManualClock {
    /// A clock frozen at `now`.
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(RwLock::new(now)),
        }
    }

    /// Move the clock to `now`.
    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.write().expect("manual clock lock") = now;
    }

    /// Move the clock forward by `delta` (negative deltas move it
    /// backwards — a test that needs the past says so explicitly).
    pub fn advance(&self, delta: Duration) {
        let mut now = self.now.write().expect("manual clock lock");
        *now += delta;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.read().expect("manual clock lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_is_utc_and_current() {
        let before = Utc::now();
        let now = SystemClock.now();
        let after = Utc::now();
        assert!(now >= before && now <= after, "system clock must be now");
        assert_eq!(now.offset(), &Utc, "system clock must be UTC");
    }

    #[test]
    fn manual_clock_is_frozen_until_moved() {
        let start = DateTime::parse_from_rfc3339("2026-11-12T00:00:00Z")
            .expect("fixture parses")
            .with_timezone(&Utc);
        let clock = ManualClock::new(start);
        assert_eq!(clock.now(), start);
        assert_eq!(clock.now(), start, "frozen: two reads agree");

        clock.advance(Duration::hours(2));
        assert_eq!(clock.now(), start + Duration::hours(2));

        let later = start + Duration::days(1);
        clock.set(later);
        assert_eq!(clock.now(), later);

        // Clones share the instant.
        let clone = clock.clone();
        clock.advance(Duration::minutes(5));
        assert_eq!(clone.now(), later + Duration::minutes(5));
    }
}
