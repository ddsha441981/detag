//! Live concurrency-slot tests (P1-053, FR-P-02).
//!
//! The slot pool is Redis-only (a sorted set per user,
//! `pf:slots:{user}` — member = job id, score =
//! lease-expiry epoch-ms), so these tests run against
//! the dev Redis (`PF_REDIS_URL`, dev default
//! `redis://localhost:6379`) with a frozen
//! [`ManualClock`] — no scratch Postgres DB is needed.
//! Every test uses fresh user UUIDs, so parallel tests
//! never share a lease set.
//!
//! - `acquire_admits_up_to_the_limit_then_denies` — the
//!   plan cap (Free 1 / Pro 3 / Business 10) is a hard
//!   ceiling on live slots.
//! - `reacquire_is_idempotent` — acquiring a slot the
//!   job already holds refreshes the lease, it does not
//!   count twice.
//! - `release_frees_the_slot` — completion frees the
//!   slot; a double release is a no-op.
//! - `expired_leases_are_reclaimed` — the ticket's AC:
//!   a slot whose worker died (never released, clock
//!   advanced past the 1 h lease) is reclaimed by the
//!   `recover` sweep, and a fresh acquire also succeeds
//!   without any sweep (lazy prune on acquire).
//! - `renew_extends_the_lease` — the worker heartbeat
//!   extends a live lease past its original expiry, and
//!   a released slot cannot be renewed back to life.
//! - `concurrent_acquires_admit_exactly_the_limit` —
//!   ten concurrent acquires under a limit of 3 admit
//!   exactly 3 (the Lua script is atomic).
//! - `slots_are_per_user` — one user's full cap never
//!   blocks another user.
//! - `limit_zero_admits_nothing` — a plan with a 0
//!   concurrency limit grants no slot, without touching
//!   Redis at all.
//! - `dead_redis_fails_closed` — an unreachable Redis
//!   errors instead of silently granting a slot.

use chrono::{Duration, TimeZone, Utc};
use detag_core::clock::ManualClock;
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use detag_core::slots::{SlotError, SlotPool};
use std::sync::Arc;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn pool() -> RedisPool {
    create_pool(&redis_url(), &RedisPoolConfig::default()).expect("pool builds")
}

/// A pool with one connection per concurrent task, so the
/// race test really runs the scripts concurrently on the
/// server.
fn wide_pool() -> RedisPool {
    create_pool(
        &redis_url(),
        &RedisPoolConfig {
            max_size: 50,
            ..RedisPoolConfig::default()
        },
    )
    .expect("pool builds")
}

fn dead_pool() -> RedisPool {
    create_pool("redis://localhost:6399", &RedisPoolConfig::default())
        .expect("pool builds from any URL")
}

/// A slot pool on a clock frozen at 2026-11-15 12:00 UTC.
fn slots_on(pool: RedisPool) -> (SlotPool, ManualClock) {
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let slots = SlotPool::new(pool, Arc::new(clock.clone()));
    (slots, clock)
}

#[tokio::test]
async fn acquire_admits_up_to_the_limit_then_denies() {
    let (slots, _clock) = slots_on(pool());
    let user = Uuid::now_v7();
    let limit = 3;

    for _ in 0..limit {
        assert!(
            slots
                .acquire(user, Uuid::now_v7(), limit)
                .await
                .expect("acquire succeeds"),
            "each acquire under the limit must be admitted"
        );
    }
    assert_eq!(slots.held(user).await.expect("held reads"), 3);
    assert!(
        !slots
            .acquire(user, Uuid::now_v7(), limit)
            .await
            .expect("acquire succeeds"),
        "the 4th acquire over a limit of 3 must be denied"
    );
    assert_eq!(slots.held(user).await.expect("held reads"), 3);
}

#[tokio::test]
async fn reacquire_is_idempotent() {
    let (slots, _clock) = slots_on(pool());
    let user = Uuid::now_v7();
    let job = Uuid::now_v7();

    assert!(slots.acquire(user, job, 1).await.expect("acquire"));
    assert!(
        slots.acquire(user, job, 1).await.expect("re-acquire"),
        "re-acquiring a slot the job already holds must still succeed"
    );
    assert_eq!(
        slots.held(user).await.expect("held reads"),
        1,
        "a re-acquire refreshes the lease, it does not count twice"
    );
}

#[tokio::test]
async fn release_frees_the_slot() {
    let (slots, _clock) = slots_on(pool());
    let user = Uuid::now_v7();
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();

    assert!(slots.acquire(user, first, 1).await.expect("acquire"));
    assert!(
        !slots
            .acquire(user, second, 1)
            .await
            .expect("acquire succeeds"),
        "the slot is held — a second job must be denied"
    );
    assert!(
        slots.release(user, first).await.expect("release"),
        "releasing a held slot frees it"
    );
    assert!(
        slots.acquire(user, second, 1).await.expect("acquire"),
        "after the release the slot is free again"
    );
    assert!(
        !slots.release(user, first).await.expect("release"),
        "releasing a slot that is not held is a no-op false"
    );
    assert!(slots.release(user, second).await.expect("release"));
    assert_eq!(slots.held(user).await.expect("held reads"), 0);
}

#[tokio::test]
async fn expired_leases_are_reclaimed() {
    let (slots, clock) = slots_on(pool());

    // The sweep path: a crashed job never releases, the
    // clock moves past the 1 h lease, recover reclaims it.
    let user = Uuid::now_v7();
    let crashed = Uuid::now_v7();
    assert!(slots.acquire(user, crashed, 1).await.expect("acquire"));
    clock.advance(Duration::minutes(61));
    assert_eq!(
        slots.recover(user).await.expect("recover"),
        1,
        "the sweep must reclaim exactly the one expired lease"
    );
    assert_eq!(slots.held(user).await.expect("held reads"), 0);
    assert!(
        slots
            .acquire(user, Uuid::now_v7(), 1)
            .await
            .expect("acquire"),
        "after recovery a new job is admitted"
    );
    assert_eq!(slots.held(user).await.expect("held reads"), 1);

    // The lazy path: no sweep ever runs, the next acquire
    // prunes the expired lease itself.
    let other = Uuid::now_v7();
    let crashed_other = Uuid::now_v7();
    assert!(slots
        .acquire(other, crashed_other, 1)
        .await
        .expect("acquire"));
    clock.advance(Duration::minutes(61));
    assert!(
        slots
            .acquire(other, Uuid::now_v7(), 1)
            .await
            .expect("acquire"),
        "an acquire after the lease expired must succeed with no sweep"
    );
    assert_eq!(slots.held(other).await.expect("held reads"), 1);
    assert_eq!(
        slots.recover(other).await.expect("recover"),
        0,
        "nothing is left expired to reclaim"
    );
}

#[tokio::test]
async fn renew_extends_the_lease() {
    let (slots, clock) = slots_on(pool());
    let user = Uuid::now_v7();
    let job = Uuid::now_v7();
    let other = Uuid::now_v7();

    assert!(slots.acquire(user, job, 1).await.expect("acquire"));
    // 30 min in, the heartbeat renews — the lease now
    // runs to t=90, not t=60.
    clock.advance(Duration::minutes(30));
    assert!(
        slots.renew(user, job).await.expect("renew"),
        "renewing a live lease must succeed"
    );
    // t=70: past the ORIGINAL expiry, inside the renewed
    // one — the slot is still held.
    clock.advance(Duration::minutes(40));
    assert_eq!(slots.held(user).await.expect("held reads"), 1);
    assert!(
        !slots.acquire(user, other, 1).await.expect("acquire"),
        "the renewed slot still blocks a second job"
    );
    assert!(
        !slots.renew(user, other).await.expect("renew"),
        "renewing a slot that is not held must fail"
    );
    // t=95: past the renewed expiry — the slot is dead
    // and reclaims.
    clock.advance(Duration::minutes(25));
    assert_eq!(
        slots.recover(user).await.expect("recover"),
        1,
        "the renewed lease expires too, once the heartbeats stop"
    );
    assert!(
        slots.acquire(user, other, 1).await.expect("acquire"),
        "after the renewed lease expires the slot is free"
    );
}

#[tokio::test]
async fn concurrent_acquires_admit_exactly_the_limit() {
    let (slots, _clock) = slots_on(wide_pool());
    let user = Uuid::now_v7();
    let limit = 3;

    let mut tasks = Vec::new();
    for _ in 0..10 {
        let slots = slots.clone();
        tasks.push(tokio::spawn(async move {
            slots.acquire(user, Uuid::now_v7(), limit).await
        }));
    }
    let mut admitted = 0;
    for task in tasks {
        let result = task.await.expect("task joins");
        if result.expect("acquire succeeds") {
            admitted += 1;
        }
    }
    assert_eq!(
        admitted, 3,
        "exactly the limit of 3 must be admitted under a 10-way race, got {admitted}"
    );
    assert_eq!(slots.held(user).await.expect("held reads"), 3);
}

#[tokio::test]
async fn slots_are_per_user() {
    let (slots, _clock) = slots_on(pool());
    let full = Uuid::now_v7();
    let other = Uuid::now_v7();

    assert!(slots
        .acquire(full, Uuid::now_v7(), 1)
        .await
        .expect("acquire"));
    assert!(
        !slots
            .acquire(full, Uuid::now_v7(), 1)
            .await
            .expect("acquire succeeds"),
        "the first user is at their cap"
    );
    assert!(
        slots
            .acquire(other, Uuid::now_v7(), 1)
            .await
            .expect("acquire"),
        "a second user has their own cap"
    );
}

#[tokio::test]
async fn limit_zero_admits_nothing() {
    // Against a DEAD pool, on purpose: a 0 limit must
    // deny before touching Redis, so no pool error can
    // surface here.
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let slots = SlotPool::new(dead_pool(), Arc::new(clock));
    assert!(
        !slots
            .acquire(Uuid::now_v7(), Uuid::now_v7(), 0)
            .await
            .expect("a 0 limit denies without a Redis call"),
        "a plan with a 0 concurrency limit grants no slot"
    );
}

#[tokio::test]
async fn dead_redis_fails_closed() {
    let (slots, _clock) = slots_on(dead_pool());
    let err = slots
        .acquire(Uuid::now_v7(), Uuid::now_v7(), 1)
        .await
        .expect_err("acquire against a dead Redis must error");
    assert!(
        matches!(err, SlotError::Pool(_) | SlotError::Redis(_)),
        "a dead Redis is a Pool/Redis error, not a silent grant: {err:?}"
    );
}
