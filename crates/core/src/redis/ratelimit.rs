//! Rate-limit primitives (P0-048): fixed-window + token-bucket,
//! both as atomic Lua scripts (one round-trip, no TOCTOU).
//!
//! These are primitives, not policy: the limits themselves are
//! per-plan defaults in config (open question #17, tuned in
//! Phase 5) and the HTTP layer (429 + `X-RateLimit-*` headers,
//! api spec) lands with Phase 1 auth. Callers pick a scope (the
//! `{scope}` in the `pf:ratelimit:{scope}:{id}` key — e.g.
//! `jobs_create`, `auth_login`, `api_key:{prefix}`) and an id
//! (user id, key prefix, or IP hash), and the primitive answers
//! allowed / remaining / reset-in.
//!
//! Both scripts run through `redis::Script` (EVALSHA with EVAL
//! fallback), so the script body is sent once and then addressed
//! by hash.

use crate::redis::keys::rate_limit_key;
use crate::redis::RedisPool;
use deadpool_redis::redis::Script;
use std::time::{Duration, SystemTime};
use thiserror::Error;

/// The Lua sources (single source of truth — the `.lua` files
/// next to this module).
const FIXED_WINDOW_LUA: &str = include_str!("fixed_window.lua");
const TOKEN_BUCKET_LUA: &str = include_str!("token_bucket.lua");

/// The answer to one rate-limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    /// Whether this request is admitted.
    pub allowed: bool,
    /// Whole requests/tokens left after this check (0 when denied).
    pub remaining: u64,
    /// Until the next request may be allowed: the window TTL for
    /// fixed-window, the time until one token refills for the
    /// bucket (0 when a token is already available, the bucket
    /// TTL when the bucket never refills).
    pub reset_in: Duration,
}

/// Rate-limit failures: bad config (caller error) or Redis
/// unavailable (transient — the caller decides whether to fail
/// open or closed; see the module docs of the middleware that
/// will use this in Phase 1).
#[derive(Debug, Error)]
pub enum RateLimitError {
    /// `FixedWindow::new` / `TokenBucket::new` rejected the config.
    #[error("invalid rate-limit config: {0}")]
    InvalidConfig(String),
    /// The script answered something the code cannot parse.
    #[error("unexpected script reply: {0}")]
    UnexpectedReply(String),
    /// Checkout from the pool failed.
    #[error("redis pool error: {0}")]
    Pool(#[from] deadpool_redis::PoolError),
    /// The script (or the connection) failed.
    #[error("redis error: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),
}

/// Fixed-window limiter: at most `limit` requests per `window`.
/// The window is anchored at the first request (not wall-clock
/// aligned) — the standard shape for per-account/IP abuse limits
/// (FR-A-04).
#[derive(Debug, Clone, Copy)]
pub struct FixedWindow {
    limit: u64,
    window: Duration,
}

impl FixedWindow {
    /// `limit` requests per `window` (window must be >= 1 s).
    pub fn new(limit: u64, window: Duration) -> Result<Self, RateLimitError> {
        if limit == 0 {
            return Err(RateLimitError::InvalidConfig(
                "limit must be > 0".to_string(),
            ));
        }
        if window.as_secs() < 1 {
            return Err(RateLimitError::InvalidConfig(
                "window must be >= 1 s".to_string(),
            ));
        }
        Ok(FixedWindow { limit, window })
    }

    /// Check one request for (`scope`, `id`) against the pool.
    pub async fn check(
        &self,
        pool: &RedisPool,
        scope: &str,
        id: &str,
    ) -> Result<RateLimit, RateLimitError> {
        let mut conn = pool.get().await?;
        let (count, ttl): (i64, i64) = Script::new(FIXED_WINDOW_LUA)
            .key(rate_limit_key(scope, id))
            .arg(self.window.as_secs() as i64)
            .invoke_async(&mut conn)
            .await?;
        let count = count.max(0) as u64;
        Ok(RateLimit {
            allowed: count <= self.limit,
            remaining: self.limit.saturating_sub(count),
            // TTL -1 (no expiry) cannot happen — the script sets
            // it on the first INCR — but fall back to the full
            // window rather than reporting 0.
            reset_in: Duration::from_secs(if ttl > 0 {
                ttl as u64
            } else {
                self.window.as_secs()
            }),
        })
    }
}

/// Token-bucket limiter: bursts up to `capacity`, refilling at
/// `refill_per_sec` tokens per second. The standard shape for
/// per-key API limits (FR-A-06) — a key may burst, but its
/// long-run rate is bounded.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
}

impl TokenBucket {
    /// `capacity` burst tokens, refilling at `refill_per_sec`
    /// tokens/second (0 = never refills: a fixed allowance).
    pub fn new(capacity: f64, refill_per_sec: f64) -> Result<Self, RateLimitError> {
        if !capacity.is_finite() || capacity <= 0.0 {
            return Err(RateLimitError::InvalidConfig(
                "capacity must be a finite number > 0".to_string(),
            ));
        }
        if !refill_per_sec.is_finite() || refill_per_sec < 0.0 {
            return Err(RateLimitError::InvalidConfig(
                "refill_per_sec must be a finite number >= 0".to_string(),
            ));
        }
        Ok(TokenBucket {
            capacity,
            refill_per_sec,
        })
    }

    /// TTL for an idle bucket key: long enough for an empty bucket
    /// to refill to full, plus a 60 s margin. A never-refilling
    /// bucket gets 24 h (it is a fixed allowance, not a leak).
    pub fn bucket_ttl(&self) -> Duration {
        if self.refill_per_sec > 0.0 {
            Duration::from_secs((self.capacity / self.refill_per_sec).ceil() as u64 + 60)
        } else {
            Duration::from_secs(24 * 3600)
        }
    }

    /// Check one request for (`scope`, `id`), spending `tokens`
    /// (usually 1.0).
    pub async fn check(
        &self,
        pool: &RedisPool,
        scope: &str,
        id: &str,
        tokens: f64,
    ) -> Result<RateLimit, RateLimitError> {
        if !tokens.is_finite() || tokens <= 0.0 {
            return Err(RateLimitError::InvalidConfig(
                "tokens must be a finite number > 0".to_string(),
            ));
        }
        // Wall-clock seconds. A clock before the epoch yields 0 —
        // fail-closed: the bucket does not refill on a broken clock.
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut conn = pool.get().await?;
        let (allowed, tokens_left): (i64, String) = Script::new(TOKEN_BUCKET_LUA)
            .key(rate_limit_key(scope, id))
            .arg(self.capacity)
            .arg(self.refill_per_sec)
            .arg(now)
            .arg(tokens)
            .arg(self.bucket_ttl().as_secs() as i64)
            .invoke_async(&mut conn)
            .await?;
        let tokens_left: f64 = tokens_left
            .parse()
            .map_err(|_| RateLimitError::UnexpectedReply(tokens_left.clone()))?;
        let reset_in = if tokens_left >= 1.0 {
            Duration::ZERO
        } else if self.refill_per_sec > 0.0 {
            Duration::from_secs_f64(((1.0 - tokens_left) / self.refill_per_sec).max(0.0))
        } else {
            // Never refills: the next admission is when the key
            // expires and a fresh full bucket is created.
            self.bucket_ttl()
        };
        Ok(RateLimit {
            allowed: allowed == 1,
            remaining: tokens_left.max(0.0).floor() as u64,
            reset_in,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_window_rejects_bad_config() {
        assert!(FixedWindow::new(0, Duration::from_secs(60)).is_err());
        assert!(FixedWindow::new(10, Duration::from_millis(500)).is_err());
        assert!(FixedWindow::new(10, Duration::from_secs(60)).is_ok());
    }

    #[test]
    fn token_bucket_rejects_bad_config() {
        assert!(TokenBucket::new(0.0, 1.0).is_err());
        assert!(TokenBucket::new(-1.0, 1.0).is_err());
        assert!(TokenBucket::new(10.0, -1.0).is_err());
        assert!(TokenBucket::new(f64::NAN, 1.0).is_err());
        assert!(TokenBucket::new(10.0, 1.0).is_ok());
    }

    #[test]
    fn bucket_ttl_covers_a_full_refill_plus_margin() {
        // 10 tokens at 2/s: 5 s to refill + 60 s margin.
        let bucket = TokenBucket::new(10.0, 2.0).expect("valid config");
        assert_eq!(bucket.bucket_ttl(), Duration::from_secs(65));
        // Fractional: 10 tokens at 3/s -> ceil(3.33) = 4 + 60.
        let bucket = TokenBucket::new(10.0, 3.0).expect("valid config");
        assert_eq!(bucket.bucket_ttl(), Duration::from_secs(64));
        // Never refills: a fixed 24 h allowance.
        let bucket = TokenBucket::new(10.0, 0.0).expect("valid config");
        assert_eq!(bucket.bucket_ttl(), Duration::from_secs(24 * 3600));
    }

    #[test]
    fn lua_sources_are_the_files_next_door() {
        // The scripts are loaded from the .lua files so they can be
        // reviewed as Lua; a drift between the const and the file
        // would silently change behaviour — assert they match.
        assert!(FIXED_WINDOW_LUA.contains("redis.call('INCR'"));
        assert!(FIXED_WINDOW_LUA.contains("redis.call('EXPIRE'"));
        assert!(TOKEN_BUCKET_LUA.contains("redis.call('HMGET'"));
        assert!(TOKEN_BUCKET_LUA.contains("redis.call('HSET'"));
        assert!(TOKEN_BUCKET_LUA.contains("tostring(tokens)"));
    }
}
