//! Live test for the P0-047 connection manager (`create_pool`).
//!
//! Checks out connections from a pool against the dev Redis
//! (native redis-server on this VM, compose redis where docker
//! runs — PF_REDIS_URL, dev default `redis://localhost:6379`)
//! and round-trips a key. Test keys are unique per run
//! (`pf:cache:test:{uuid}`) and deleted afterwards — never
//! FLUSHDB, never a shared key.

use detag_core::redis::keys::cache_key;
use detag_core::redis::{create_pool, RedisPoolConfig};
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

#[tokio::test]
async fn pool_ping_and_key_roundtrip() {
    let pool = create_pool(&redis_url(), &RedisPoolConfig::default())
        .expect("pool builds from the dev URL");
    let mut conn = pool.get().await.expect("checkout from the pool");
    let pong: String = deadpool_redis::redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("PING over a pooled connection");
    assert_eq!(pong, "PONG");

    let test_key = cache_key("test", &Uuid::now_v7().to_string());
    let _: () = deadpool_redis::redis::cmd("SET")
        .arg(&test_key)
        .arg("roundtrip")
        .query_async(&mut conn)
        .await
        .expect("SET a test key");
    let got: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg(&test_key)
        .query_async(&mut conn)
        .await
        .expect("GET the test key");
    assert_eq!(got.as_deref(), Some("roundtrip"));
    let deleted: i32 = deadpool_redis::redis::cmd("DEL")
        .arg(&test_key)
        .query_async(&mut conn)
        .await
        .expect("DEL the test key");
    assert_eq!(deleted, 1);
}

#[tokio::test]
async fn malformed_url_fails_pool_creation() {
    let err = create_pool("::not a url::", &RedisPoolConfig::default())
        .expect_err("a malformed URL must fail pool creation");
    let _ = err; // The Display may contain the URL — never log it unredacted.
}

#[tokio::test]
async fn unreachable_server_fails_checkout_not_creation() {
    // deadpool connects lazily: creating the pool succeeds, and the
    // failure surfaces at checkout (bounded by wait_timeout).
    let pool = create_pool(
        "redis://127.0.0.1:59998",
        &RedisPoolConfig {
            wait_timeout: std::time::Duration::from_millis(200),
            ..RedisPoolConfig::default()
        },
    )
    .expect("pool creation is lazy and must succeed");
    // `Connection` is not `Debug`, so `expect_err` cannot be used —
    // assert on the Result directly.
    assert!(
        pool.get().await.is_err(),
        "checkout against a closed port must fail"
    );
}
