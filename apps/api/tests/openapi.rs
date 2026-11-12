//! P0-067: the OpenAPI scaffold end to end, against the router
//! `build_router` builds (state + middleware included).
//!
//! - In dev (the default), `/docs` redirects to `/docs/`, which
//!   serves the Swagger UI, and `/openapi.json` serves the
//!   generated spec (the same document `just openapi` commits).
//! - In prod, `/docs` does not exist (404) while `/openapi.json`
//!   stays public — the console viewer (P4-064) fetches it in prod.

use axum::http::{Method, Request, StatusCode};
use detag_api::{build_router, AppState};
use detag_core::config::Settings;
use detag_core::metrics::Metrics;
use tower::ServiceExt;

fn test_router(settings: Settings) -> axum::Router {
    let state = AppState::from_settings(&settings).expect("settings build");
    build_router(state, Metrics::new())
}

async fn get(router: axum::Router, path: &str) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router
        .oneshot(req)
        .await
        .unwrap_or_else(|err| panic!("oneshot GET {path} failed: {err:?}"));
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("read body");
    (status, body.to_vec(), headers)
}

#[tokio::test]
async fn docs_redirects_and_serves_swagger_ui_in_dev() {
    let router = test_router(Settings::default());

    let (status, _, headers) = get(router.clone(), "/docs").await;
    assert!(
        status.is_redirection(),
        "/docs must redirect to /docs/, got {status}"
    );
    assert_eq!(
        headers.get("location").and_then(|v| v.to_str().ok()),
        Some("/docs/"),
        "/docs must redirect to /docs/"
    );

    let (status, body, _) = get(router, "/docs/").await;
    assert_eq!(status, StatusCode::OK, "/docs/ must be 200");
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("Swagger UI"),
        "/docs/ must serve the Swagger UI page: {html:.200}"
    );
}

#[tokio::test]
async fn openapi_json_serves_the_generated_spec_in_dev() {
    let router = test_router(Settings::default());
    let (status, body, _) = get(router, "/openapi.json").await;
    assert_eq!(status, StatusCode::OK, "/openapi.json must be 200");
    let spec: serde_json::Value =
        serde_json::from_slice(&body).expect("/openapi.json must be JSON");
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
    let settings = Settings {
        environment: "prod".to_string(),
        ..Settings::default()
    };
    let router = test_router(settings);

    let (status, _, _) = get(router.clone(), "/docs").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "prod must not serve /docs, got {status}"
    );
    let (status, _, _) = get(router.clone(), "/docs/").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "prod must not serve /docs/, got {status}"
    );

    let (status, body, _) = get(router, "/openapi.json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "/openapi.json stays public in prod, got {status}"
    );
    let spec: serde_json::Value =
        serde_json::from_slice(&body).expect("/openapi.json must be JSON");
    assert_eq!(spec["info"]["title"], "Detag API");
}
