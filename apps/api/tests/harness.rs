//! P0-071: the harness itself, end to end.
//!
//! - `spawn_app` serves `/healthz` (the app builds from the
//!   environment fixtures, scratch DB migrated).
//! - `seed_user_unique` inserts a row the repo can read back.
//! - The testcontainers leg runs only with `PF_TEST_CONTAINERS=1`
//!   (CI does); on this VM it prints SKIP and passes — docker is
//!   absent here (DOCKER-001).

mod common;

use axum::http::StatusCode;
use common::{spawn_app, TestApp, TestFixtures};
use detag_core::repos::users;

#[tokio::test]
async fn spawn_app_serves_healthz() {
    let app = spawn_app().await;
    let res = app.get("/healthz").await;
    assert_eq!(res.status, StatusCode::OK, "/healthz must be 200");
    assert_eq!(res.text(), "ok", "/healthz answers \"ok\"");
}

#[tokio::test]
async fn seed_user_roundtrip() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    assert!(
        user.email.ends_with("@example.com"),
        "seeded email must be a harness address: {}",
        user.email
    );
    let found = users::by_id(&app.pool, user.id)
        .await
        .expect("by_id reads the seeded row")
        .expect("seeded user must exist");
    assert_eq!(found.email, user.email);
    assert_eq!(found.password_hash, common::TEST_PASSWORD_HASH);
}

#[tokio::test]
async fn testcontainers_fixtures_when_enabled() {
    if std::env::var("PF_TEST_CONTAINERS").as_deref() != Ok("1") {
        eprintln!("SKIP: testcontainers fixtures need PF_TEST_CONTAINERS=1 (docker absent here, DOCKER-001)");
        return;
    }
    let fixtures = TestFixtures::containers().await.expect("containers start");
    let app = TestApp::spawn_with_fixtures(fixtures).await;
    let res = app.get("/healthz").await;
    assert_eq!(res.status, StatusCode::OK, "/healthz must be 200");
}
