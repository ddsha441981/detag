//! P0-067: the OpenAPI scaffold end to end, against the router
//! `build_router` builds (state + middleware included). Uses the
//! P0-071 harness ([`common`]) — the first endpoint tests to do so.
//!
//! - In dev (the default), `/docs` redirects to `/docs/`, which
//!   serves the Swagger UI, and `/openapi.json` serves the
//!   generated spec (the same document `just openapi` commits).
//! - In prod, `/docs` does not exist (404) while `/openapi.json`
//!   stays public — the console viewer (P4-064) fetches it in prod.

mod common;

use axum::http::StatusCode;
use common::{spawn_app, TestApp, TestFixtures};

#[tokio::test]
async fn docs_redirects_and_serves_swagger_ui_in_dev() {
    let app = spawn_app().await;

    let res = app.get("/docs").await;
    assert!(
        res.status.is_redirection(),
        "/docs must redirect to /docs/, got {}",
        res.status
    );
    assert_eq!(
        res.headers.get("location").and_then(|v| v.to_str().ok()),
        Some("/docs/"),
        "/docs must redirect to /docs/"
    );

    let res = app.get("/docs/").await;
    assert_eq!(res.status, StatusCode::OK, "/docs/ must be 200");
    assert!(
        res.text().contains("Swagger UI"),
        "/docs/ must serve the Swagger UI page"
    );
}

#[tokio::test]
async fn openapi_json_serves_the_generated_spec_in_dev() {
    let app = spawn_app().await;
    let res = app.get("/openapi.json").await;
    assert_eq!(res.status, StatusCode::OK, "/openapi.json must be 200");
    let spec = res.json();
    assert_eq!(spec["info"]["title"], "Detag API");
    assert!(spec["paths"]["/healthz"].is_object(), "{spec}");
    assert!(spec["paths"]["/readyz"].is_object(), "{spec}");
    assert!(
        spec["components"]["schemas"]["Problem"].is_object(),
        "the spec must carry the Problem error schema: {spec}"
    );
}

#[tokio::test]
async fn docs_absent_in_prod_but_spec_stays_public() {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.environment = "prod".to_string();
    let app = TestApp::spawn_with(settings).await;

    let res = app.get("/docs").await;
    assert_eq!(
        res.status,
        StatusCode::NOT_FOUND,
        "prod must not serve /docs, got {}",
        res.status
    );
    let res = app.get("/docs/").await;
    assert_eq!(
        res.status,
        StatusCode::NOT_FOUND,
        "prod must not serve /docs/, got {}",
        res.status
    );

    let res = app.get("/openapi.json").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "/openapi.json stays public in prod, got {}",
        res.status
    );
    let spec = res.json();
    assert_eq!(spec["info"]["title"], "Detag API");
}
