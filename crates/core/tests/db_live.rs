//! Live database tests (P0-035, E0.4).
//!
//! These need a real Postgres on `PF_DATABASE_URL` (default: the dev
//! database, `postgres://pf:pf@localhost:5432/pf`). Start it with
//! `just dev` (compose postgres when docker can run containers) or
//! the native PostgreSQL on this VM — see DOCKER-001 / ASSUMPTION-007.
//! CI provides it as a service container. If the database is down the
//! tests fail loudly with instructions instead of skipping silently:
//! a pool that cannot reach its database is a real failure, not a
//! reason to skip the suite.

use detag_core::db::{connect, db_health, PoolConfig};

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

#[tokio::test]
async fn db_health_reports_up_against_real_postgres() {
    let url = database_url();
    let pool = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live db test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    let health = db_health(&pool).await;
    assert!(health.ok, "db must report up: {}", health.detail);
    assert_eq!(health.name, "db");
    assert_eq!(health.detail, "ok");
    // The pool really talks to the server: SELECT 1 came back as 1.
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("SELECT 1 over the pool");
    assert_eq!(one, 1);
    pool.close().await;
}
