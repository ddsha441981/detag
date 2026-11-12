//! P0-092: the shared test context itself.
//!
//! - `ctx_creates_migrated_database`: a fresh ctx has all
//!   migrations applied (the `users` table exists and is empty)
//!   and `close` drops the database (reconnecting to its URL
//!   fails afterwards).
//! - `two_contexts_are_isolated`: a row written through one ctx
//!   is invisible to the other (per-test databases).
//! - `parallel_contexts_are_isolated`: four ctxs built + used
//!   concurrently, each seeing only its own row (the parallel
//!   suite's shape).
//! - `redis_namespaces_do_not_collide`: the key prefixes differ
//!   per ctx, and `redis_key` stays inside the namespace.
//! - `containers_leg_when_enabled`: the testcontainers leg (CI
//!   only, `PF_TEST_CONTAINERS=1`) — the bucket IS created there.
//!
//! The bucket leg is NOT asserted in `new()`: on this VM there is
//! no MinIO, so `new()` builds with `s3_ready == false` by design
//! (the ctx still builds — the test asserts exactly that, so a
//! missing MinIO can never fail the suite).

use detag_core::repos::users;
use detag_testkit::TestCtx;
use sqlx::Connection as _;

#[tokio::test]
async fn ctx_creates_migrated_database() {
    let ctx = TestCtx::new().await;
    assert!(
        !ctx.s3_ready(),
        "no MinIO on this VM: the ctx builds with s3_ready false"
    );

    // Migrations applied: the users table exists and starts empty.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&ctx.pool)
        .await
        .expect("users table must exist after migrate_up");
    assert_eq!(count, 0, "fresh scratch DB must start empty");

    let url = ctx.database_url.clone();
    ctx.close().await;

    // close() dropped the database: reconnecting fails.
    let reconnected = sqlx::PgConnection::connect(&url).await;
    assert!(
        reconnected.is_err(),
        "scratch DB must be gone after close()"
    );
}

#[tokio::test]
async fn two_contexts_are_isolated() {
    let a = TestCtx::new().await;
    let b = TestCtx::new().await;
    assert_ne!(a.database_url, b.database_url, "each ctx gets its own DB");
    assert_ne!(a.s3_bucket, b.s3_bucket, "each ctx gets its own bucket");
    assert_ne!(
        a.redis_prefix, b.redis_prefix,
        "each ctx gets its own redis namespace"
    );

    let user = users::create(&a.pool, "iso@example.com", "h")
        .await
        .expect("create in ctx A");
    let found_in_a = users::by_id(&a.pool, user.id)
        .await
        .expect("by_id in A")
        .expect("row must exist in A");
    assert_eq!(found_in_a.email, "iso@example.com");
    let found_in_b = users::by_id(&b.pool, user.id).await.expect("by_id in B");
    assert!(
        found_in_b.is_none(),
        "ctx B must not see ctx A's row (per-test databases)"
    );
    let by_email_b = users::by_email(&b.pool, "iso@example.com")
        .await
        .expect("by_email in B");
    assert!(by_email_b.is_none(), "ctx B must not see ctx A's email");

    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn parallel_contexts_are_isolated() {
    // Four ctxs built + used concurrently — the parallel suite's
    // shape. Each writes one row and reads back only its own.
    let (a, b, c, d) = tokio::join!(
        TestCtx::new(),
        TestCtx::new(),
        TestCtx::new(),
        TestCtx::new()
    );
    let ctxs = [a, b, c, d];
    let mut ids = Vec::new();
    for (i, ctx) in ctxs.iter().enumerate() {
        let email = format!("parallel-{i}@example.com");
        let user = users::create(&ctx.pool, &email, "h")
            .await
            .expect("create in parallel ctx");
        ids.push((user.id, email));
    }
    for (ctx, (id, email)) in ctxs.iter().zip(&ids) {
        let found = users::by_id(&ctx.pool, *id)
            .await
            .expect("by_id")
            .expect("own row must exist");
        assert_eq!(found.email, *email);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&ctx.pool)
            .await
            .expect("count");
        assert_eq!(count, 1, "each ctx must see exactly its own row");
    }
    for ctx in ctxs {
        ctx.close().await;
    }
}

#[tokio::test]
async fn redis_namespaces_do_not_collide() {
    let a = TestCtx::new().await;
    let b = TestCtx::new().await;
    assert_ne!(a.redis_prefix, b.redis_prefix);
    assert!(a.redis_prefix.starts_with("testkit:"));
    assert!(a.redis_prefix.ends_with(':'));
    let key_a = a.redis_key("jobs:123");
    let key_b = b.redis_key("jobs:123");
    assert_ne!(key_a, key_b, "same logical key, different namespaces");
    assert!(key_a.starts_with(&a.redis_prefix));
    assert!(key_b.starts_with(&b.redis_prefix));
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn containers_leg_when_enabled() {
    if std::env::var("PF_TEST_CONTAINERS").as_deref() != Ok("1") {
        eprintln!(
            "SKIP: testcontainers leg needs PF_TEST_CONTAINERS=1 (docker absent here, DOCKER-001)"
        );
        return;
    }
    let ctx = TestCtx::containers().await.expect("containers start");
    assert!(ctx.s3_ready(), "the bucket IS created in containers mode");
    // The bucket exists: head_bucket answers.
    ctx.s3_client()
        .head_bucket()
        .bucket(&ctx.s3_bucket)
        .send()
        .await
        .expect("head_bucket on the ctx bucket");
    // Migrations applied on the container postgres too.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&ctx.pool)
        .await
        .expect("users table on container pg");
    assert_eq!(count, 0);
    ctx.close().await;
}
