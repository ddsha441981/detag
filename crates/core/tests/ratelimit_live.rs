//! Live tests for the P0-048 rate-limit primitives.
//!
//! Against the dev Redis (native redis-server on this VM, compose
//! redis where docker runs — PF_REDIS_URL, dev default
//! `redis://localhost:6379`). Every test uses a unique scope
//! (`race-fw-{uuid}` etc.) so parallel runs never share a bucket.

use detag_core::redis::ratelimit::{FixedWindow, RateLimitError, TokenBucket};
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use std::time::Duration;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn pool() -> RedisPool {
    create_pool(&redis_url(), &RedisPoolConfig::default()).expect("pool builds")
}

/// A pool with one connection per concurrent task, so the race
/// tests really run the scripts concurrently on the server.
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

fn scope(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7())
}

#[tokio::test]
async fn fixed_window_admits_up_to_the_limit_then_denies() {
    let pool = pool();
    let scope = scope("fw-basic");
    let limiter = FixedWindow::new(3, Duration::from_secs(60)).expect("valid config");

    let first = limiter.check(&pool, &scope, "user-1").await.expect("check");
    assert!(first.allowed);
    assert_eq!(first.remaining, 2);
    let second = limiter.check(&pool, &scope, "user-1").await.expect("check");
    assert!(second.allowed);
    assert_eq!(second.remaining, 1);
    let third = limiter.check(&pool, &scope, "user-1").await.expect("check");
    assert!(third.allowed);
    assert_eq!(third.remaining, 0);
    let fourth = limiter.check(&pool, &scope, "user-1").await.expect("check");
    assert!(
        !fourth.allowed,
        "the 4th request over a limit of 3 must be denied"
    );
    assert_eq!(fourth.remaining, 0);
    assert!(
        fourth.reset_in > Duration::ZERO && fourth.reset_in <= Duration::from_secs(60),
        "reset_in must be within the window: {:?}",
        fourth.reset_in
    );

    // A different id gets its own window.
    let other = limiter.check(&pool, &scope, "user-2").await.expect("check");
    assert!(other.allowed);
    assert_eq!(other.remaining, 2);
}

#[tokio::test]
async fn token_bucket_bursts_then_refills() {
    let pool = pool();
    let scope = scope("tb-basic");
    let limiter = TokenBucket::new(2.0, 1.0).expect("valid config");

    assert!(
        limiter
            .check(&pool, &scope, "k1", 1.0)
            .await
            .expect("check")
            .allowed
    );
    assert!(
        limiter
            .check(&pool, &scope, "k1", 1.0)
            .await
            .expect("check")
            .allowed
    );
    let denied = limiter
        .check(&pool, &scope, "k1", 1.0)
        .await
        .expect("check");
    assert!(
        !denied.allowed,
        "the 3rd token over a capacity of 2 must be denied"
    );
    assert!(
        denied.reset_in > Duration::ZERO,
        "reset_in must be positive while empty"
    );

    // One second of refill buys exactly one token back.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let refilled = limiter
        .check(&pool, &scope, "k1", 1.0)
        .await
        .expect("check");
    assert!(
        refilled.allowed,
        "a refilled token must admit: {refilled:?}"
    );
}

#[tokio::test]
async fn fixed_window_race_admits_exactly_the_limit() {
    let pool = wide_pool();
    let scope = scope("race-fw");
    let limiter = FixedWindow::new(10, Duration::from_secs(60)).expect("valid config");

    let mut tasks = Vec::new();
    for _ in 0..50 {
        let pool = pool.clone();
        let scope = scope.clone();
        tasks.push(tokio::spawn(async move {
            limiter.check(&pool, &scope, "racer").await
        }));
    }
    let mut allowed = 0;
    for task in tasks {
        let result = task.await.expect("task joins");
        let check = result.expect("check succeeds");
        if check.allowed {
            allowed += 1;
        }
    }
    assert_eq!(
        allowed, 10,
        "exactly the limit of 10 must be admitted under a 50-way race, got {allowed}"
    );
}

#[tokio::test]
async fn token_bucket_race_admits_exactly_the_capacity() {
    let pool = wide_pool();
    let scope = scope("race-tb");
    // No refill: a fixed allowance of 5, so the race result is exact.
    let limiter = TokenBucket::new(5.0, 0.0).expect("valid config");

    let mut tasks = Vec::new();
    for _ in 0..50 {
        let pool = pool.clone();
        let scope = scope.clone();
        tasks.push(tokio::spawn(async move {
            limiter.check(&pool, &scope, "racer", 1.0).await
        }));
    }
    let mut allowed = 0;
    for task in tasks {
        let result = task.await.expect("task joins");
        let check = result.expect("check succeeds");
        if check.allowed {
            allowed += 1;
        }
    }
    assert_eq!(
        allowed, 5,
        "exactly the capacity of 5 must be admitted under a 50-way race, got {allowed}"
    );
}

#[tokio::test]
async fn check_rejects_bad_token_counts() {
    let pool = pool();
    let limiter = TokenBucket::new(5.0, 1.0).expect("valid config");
    let err = limiter
        .check(&pool, &scope("tb-bad"), "k", 0.0)
        .await
        .expect_err("0 tokens must be rejected");
    assert!(matches!(err, RateLimitError::InvalidConfig(_)));
}
