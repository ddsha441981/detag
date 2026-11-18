//! The Razorpay webhook flag gate (P1-073, FR-P-03): the
//! ticket's AC is that flag-off endpoints 404. The receiver
//! itself lands in P1-074 — until then the flag-on answer is
//! the shared stub's 501 `PF-5004`, which says "not
//! implemented yet" instead of pretending the receiver
//! exists. The bodies below are a real Razorpay envelope
//! shape (`subscription.authenticated`) so the flag-on test
//! exercises the route a real delivery would hit, not an
//! empty probe.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::TestApp;
use detag_core::config::Settings;
use serde_json::json;

/// A fixed request id, echoed by the problem body.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174111";

/// The Razorpay event the flag-on route will one day receive:
/// a subscription authorization, the first payment made.
fn envelope() -> serde_json::Value {
    json!({
        "event": "subscription.authenticated",
        "payload": {
            "subscription": {
                "entity": {
                    "id": "sub_00000000000001",
                    "entity": "subscription",
                    "plan_id": "plan_00000000000001",
                    "status": "authenticated",
                    "notes": { "detag_user_id": "00000000-0000-0000-0000-000000000000" }
                }
            }
        }
    })
}

async fn post_razorpay(app: &TestApp, credential: Option<&str>) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/webhooks/razorpay")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", "203.0.113.7")
        .header("content-type", "application/json");
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    app.request(
        req.body(axum::body::Body::from(
            serde_json::to_vec(&envelope()).expect("envelope serializes"),
        ))
        .unwrap(),
    )
    .await
}

/// AC: flag off (the default settings) — the endpoint does
/// not exist: 404 `PF-2010`, with the request id echoed.
#[tokio::test]
async fn flag_off_answers_404_anonymous() {
    let app = TestApp::spawn_with(Settings::default()).await;
    let res = post_razorpay(&app, None).await;
    assert_eq!(
        res.status,
        StatusCode::NOT_FOUND,
        "flag off: {}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2010", "flag off: {json}");
    assert_eq!(json["request_id"], REQUEST_ID, "flag off: {json}");
}

/// AC, key-carrying caller: a junk Bearer key changes
/// nothing — the route takes no credential, and the flag
/// answers 404 before any credential is considered.
#[tokio::test]
async fn flag_off_answers_404_with_junk_key() {
    let app = TestApp::spawn_with(Settings::default()).await;
    let res = post_razorpay(&app, Some("pf_test_junk_key")).await;
    assert_eq!(
        res.status,
        StatusCode::NOT_FOUND,
        "flag off: {}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2010", "flag off: {json}");
    assert_eq!(json["request_id"], REQUEST_ID, "flag off: {json}");
}

/// Flag on — the route exists, but the receiver has not
/// landed yet (P1-074): 501 `PF-5004`, not a silent 200.
#[tokio::test]
async fn flag_on_answers_501_until_receiver_lands() {
    let settings = Settings {
        feature_razorpay: true,
        ..Settings::default()
    };
    let app = TestApp::spawn_with(settings).await;
    let res = post_razorpay(&app, None).await;
    assert_eq!(
        res.status,
        StatusCode::NOT_IMPLEMENTED,
        "flag on: {}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-5004", "flag on: {json}");
    assert_eq!(json["request_id"], REQUEST_ID, "flag on: {json}");
}
