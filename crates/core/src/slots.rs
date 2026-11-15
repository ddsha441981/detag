//! Concurrency slots (P1-053, FR-P-02).
//!
//! The crash-safe half of the per-user running-jobs cap.
//! The P1-051 pre-check enforces the cap by counting
//! `queued` + `running` rows in Postgres — correct, but a
//! row count cannot tell a crashed worker's job from a
//! live one, and a job that dies between "counted" and
//! "started" leaks its slot until someone reconciles the
//! rows. The slot pool is the lease layer that fixes
//! that: one Redis sorted set per user,
//! `pf:slots:{user}` (see [`crate::redis::keys::slots_key`]),
//! member = the job id, score = the lease-expiry
//! epoch-millis. A slot is held while its lease is live;
//! the lease is renewed by the worker's heartbeat while
//! the job runs, and it expires on its own when the
//! worker dies — so a crashed job's slot frees itself
//! without a sweeper, and [`SlotPool::recover`] is the
//! explicit sweep that reclaims expired leases on demand
//! (the ticket's AC: leak recovery).
//!
//! - [`SlotPool::acquire`] is one atomic Lua script:
//!   prune expired leases, then admit the job when the
//!   live count is under the plan's `concurrency` limit
//!   (Free 1 / Pro 3 / Business 10, `Entitlements`).
//!   Re-acquiring a slot the job already holds is
//!   idempotent — it refreshes the lease, it does not
//!   count twice. A limit of 0 admits nothing.
//! - [`SlotPool::release`] frees the slot at job
//!   completion (idempotent: releasing a slot that is
//!   not held is a no-op `false`).
//! - [`SlotPool::renew`] extends a live lease (the
//!   worker heartbeat calls it); renewing a released or
//!   expired slot fails — a worker cannot resurrect a
//!   slot that was already reclaimed.
//! - [`SlotPool::recover`] prunes expired leases and
//!   returns how many it reclaimed. [`SlotPool::acquire`]
//!   and [`SlotPool::held`] prune lazily too, so a new
//!   job-create after a crash succeeds even when no
//!   sweep ever ran.
//!
//! The lease TTL is [`SLOT_LEASE_TTL`] (1 h): the same
//! horizon as the `worker_heartbeats` staleness prune
//! (data-model.md — a worker unheard from for 1 h is
//! pruned as dead), so a slot never outlives its dead
//! worker by more than the horizon the system already
//! uses to declare a worker dead (ASSUMPTION-019). The
//! TTL is a lease, not a job timeout: a job that runs
//! longer renews, and the Phase 2 claim loop owns the
//! renew call.
//!
//! All times come from the injected [`Clock`] — never
//! `Utc::now()` in the service — so the expiry and
//! recovery behaviour is testable with a frozen clock
//! (the ticket's AC: acquire, advance past the TTL,
//! recover reclaims, acquire succeeds again).
//!
//! A Redis error fails closed: every method returns the
//! error and grants or frees nothing silently — the
//! caller (Phase 2 intake) decides how to answer, and a
//! slot it cannot see is not a slot it can trust.
//!
//! No production caller uses this yet (same shape as
//! P1-050's meter): job creation is still the 501 stub,
//! so nothing acquires a slot today, and the P1-051
//! pre-check stays DB-count-based until Phase 2 wires
//! acquire at job create and release at completion —
//! wiring a Redis-only gate in now would make the cap
//! never trip, because the set would always be empty.

use crate::clock::Clock;
use crate::redis::keys::{slots_key, TTL_SLOT_LEASE};
use crate::redis::RedisPool;
use chrono::{DateTime, Utc};
use deadpool_redis::redis::{cmd, Script};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

/// The Lua sources (single source of truth — the `.lua`
/// files next to this module, same shape as the usage
/// meter's scripts).
const SLOTS_ACQUIRE_LUA: &str = include_str!("slots_acquire.lua");
const SLOTS_RENEW_LUA: &str = include_str!("slots_renew.lua");
const SLOTS_HELD_LUA: &str = include_str!("slots_held.lua");

/// The default slot lease: 1 h (ASSUMPTION-019 — the
/// `worker_heartbeats` staleness horizon). A lease is a
/// heartbeat away from renewal, never a job timeout.
pub const SLOT_LEASE_TTL: Duration = TTL_SLOT_LEASE;

/// What can go wrong holding a slot. Both are transient
/// infrastructure failures — the caller decides whether
/// to fail open or closed (the Phase 2 intake fails
/// closed: a slot it cannot acquire is a job it does not
/// start).
#[derive(Debug, Error)]
pub enum SlotError {
    #[error("redis pool error: {0}")]
    Pool(#[from] deadpool_redis::PoolError),
    #[error("redis error: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),
}

/// The concurrency slot pool: a per-user lease set in
/// Redis, on the injected clock's time.
#[derive(Debug, Clone)]
pub struct SlotPool {
    redis: RedisPool,
    clock: Arc<dyn Clock>,
}

impl SlotPool {
    /// Build over a Redis pool and a clock
    /// (`SystemClock` in production, `ManualClock` in
    /// tests).
    pub fn new(redis: RedisPool, clock: Arc<dyn Clock>) -> Self {
        SlotPool { redis, clock }
    }

    /// The lease expiry for a lease taken now (pure, so
    /// the TTL arithmetic is unit-testable without Redis).
    pub fn lease_expiry(now: DateTime<Utc>, ttl: Duration) -> i64 {
        now.timestamp_millis() + ttl.as_millis() as i64
    }

    /// Acquire one concurrency slot for `job_id` under
    /// the plan's `limit`: `true` when the job now holds
    /// a slot (newly acquired, or already held — the
    /// re-acquire refreshes the lease), `false` when the
    /// cap is full. A limit of 0 admits nothing and
    /// touches no Redis key.
    pub async fn acquire(
        &self,
        user_id: Uuid,
        job_id: Uuid,
        limit: i64,
    ) -> Result<bool, SlotError> {
        if limit <= 0 {
            return Ok(false);
        }
        let now = self.clock.now();
        let key = slots_key(&user_id);
        let mut conn = self.redis.get().await?;
        let held: i32 = Script::new(SLOTS_ACQUIRE_LUA)
            .key(&key)
            .arg(job_id.to_string())
            .arg(now.timestamp_millis())
            .arg(Self::lease_expiry(now, SLOT_LEASE_TTL))
            .arg(limit)
            .arg(SLOT_LEASE_TTL.as_secs() as i64)
            .invoke_async(&mut conn)
            .await?;
        Ok(held == 1)
    }

    /// Release the slot `job_id` holds: `true` when a
    /// slot was freed, `false` when none was held (a
    /// double release is a no-op, not an error).
    pub async fn release(&self, user_id: Uuid, job_id: Uuid) -> Result<bool, SlotError> {
        let key = slots_key(&user_id);
        let mut conn = self.redis.get().await?;
        let freed: i32 = cmd("ZREM")
            .arg(&key)
            .arg(job_id.to_string())
            .query_async(&mut conn)
            .await?;
        Ok(freed == 1)
    }

    /// Extend the lease on a slot `job_id` holds: `true`
    /// when renewed, `false` when the slot is not held
    /// (released, expired, or never acquired — a worker
    /// cannot resurrect a reclaimed slot).
    pub async fn renew(&self, user_id: Uuid, job_id: Uuid) -> Result<bool, SlotError> {
        let now = self.clock.now();
        let key = slots_key(&user_id);
        let mut conn = self.redis.get().await?;
        let renewed: i32 = Script::new(SLOTS_RENEW_LUA)
            .key(&key)
            .arg(job_id.to_string())
            .arg(now.timestamp_millis())
            .arg(Self::lease_expiry(now, SLOT_LEASE_TTL))
            .arg(SLOT_LEASE_TTL.as_secs() as i64)
            .invoke_async(&mut conn)
            .await?;
        Ok(renewed == 1)
    }

    /// Reclaim expired leases for `user_id`: prune every
    /// slot whose lease has passed and return how many
    /// were reclaimed. The crash-recovery sweep — a job
    /// whose worker died without releasing frees its slot
    /// here (and lazily on the next acquire, so a sweep
    /// that never runs still unblocks the user).
    pub async fn recover(&self, user_id: Uuid) -> Result<i64, SlotError> {
        let now = self.clock.now();
        let key = slots_key(&user_id);
        let mut conn = self.redis.get().await?;
        let reclaimed: i64 = cmd("ZREMRANGEBYSCORE")
            .arg(&key)
            .arg(0)
            .arg(now.timestamp_millis())
            .query_async(&mut conn)
            .await?;
        Ok(reclaimed)
    }

    /// How many live slots `user_id` holds (expired
    /// leases pruned first, so a dead worker's slot never
    /// counts against the cap).
    pub async fn held(&self, user_id: Uuid) -> Result<i64, SlotError> {
        let now = self.clock.now();
        let key = slots_key(&user_id);
        let mut conn = self.redis.get().await?;
        let count: i64 = Script::new(SLOTS_HELD_LUA)
            .key(&key)
            .arg(now.timestamp_millis())
            .invoke_async(&mut conn)
            .await?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn lease_expiry_is_now_plus_ttl() {
        let now = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        assert_eq!(
            SlotPool::lease_expiry(now, SLOT_LEASE_TTL),
            now.timestamp_millis() + 3_600_000
        );
        assert_eq!(
            SlotPool::lease_expiry(now, Duration::from_secs(0)),
            now.timestamp_millis()
        );
    }

    #[test]
    fn lease_ttl_is_one_hour() {
        assert_eq!(SLOT_LEASE_TTL, Duration::from_secs(3600));
    }
}
