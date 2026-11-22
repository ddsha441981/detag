//! P1-098: the attestation admin review queue — `GET
//! /v1/admin/attestations` (FR-C-01 / FR-W-04) + the
//! `?user_id=` filter it links to on `GET
//! /v1/admin/abuse-reports`.
//!
//! The review queue is global and the harness DB is shared
//! and persists across runs (the P1-088 precedent), so a
//! list assertion can never count the whole queue: every
//! test seeds its own rows and finds them by id. The
//! velocity test seeds its rows at `Utc::now() + 365 days`
//! — one year ahead of real time, so each run's rows are
//! strictly newer than every previous run's (the base moves
//! with the run) and the seeded rows are the newest in the
//! queue. The rows sit one second apart, so the 10 s span
//! can never overlap the next run's base (runs are minutes
//! apart). The velocity window is relative to each row's
//! own `created_at`, so no frozen clock is needed.
//!
//! - `admin_attestations_guard` — the extractor + matrix
//!   guard: anonymous 401, plain user 403, an admin user's
//!   own API key 403 ("no admin ever"), support 200, admin
//!   200.
//! - `admin_attestations_velocity_flag` — the AC: one user
//!   attests 11 uploads inside 24 h, so exactly the newest
//!   row is flagged (`velocity_count` 11, `flag_reasons`
//!   `["velocity"]`, the abuse-queue link for that user);
//!   the other ten rows read counts 10..1, unflagged; a
//!   second user's single attestation reads count 1,
//!   unflagged. `?flagged=true` returns only flagged rows,
//!   `?flagged=false` only unflagged ones.
//! - `admin_attestations_filters_and_pagination` — the
//!   cursor round-trip at limit 2 (the walk stops once all
//!   eleven seeded ids are seen — the queue never ends
//!   after them, older rows sit below), the tampered cursor
//!   422, the out-of-range `ts` 422, the unknown `flagged`
//!   value 422, and the limit clamp.
//! - `admin_attestations_link_to_abuse_queue` — the link
//!   is real: following an item's `abuse_queue_url` as an
//!   admin lists that user's abuse reports and no other
//!   user's; an unknown user is an empty page, not a 404;
//!   a malformed `user_id` is 422.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_core::checksum::sha256_hex;
use detag_core::pagination::Cursor;
use detag_core::repos::uploads;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The client IP the tests attest from (TEST-NET-3).
const TEST_IP: &str = "203.0.113.7";

/// Test settings: the register flow needs a ToS version; the
/// key bucket is generous so the suite asserts review
/// outcomes, not rate limits.
async fn review_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
        "accept_aup_version": "v1",
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/register")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Log in, return the access token.
async fn login_token(app: &TestApp, email: &str) -> String {
    let body = json!({
        "email": email,
        "password": PASSWORD,
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/login")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// Register a user, set the role in the database, log in,
/// return (id, token). There is no self-serve way to become
/// an admin or support (the role is set by the owner out of
/// band; P1-009's search is the read side, the invite CLI is
/// a DECISIONS.md item).
async fn make_role(app: &TestApp, email: &str, role: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("set role");
    let token = login_token(app, email).await;
    (id, token)
}

/// Mint a key with the given scopes; return the secret.
async fn create_key(app: &TestApp, token: &str, scopes: &[&str]) -> String {
    let body = json!({
        "name": "attestation review probe",
        "scopes": scopes,
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
}

/// Seed an upload for the user (the P1-094 helper).
async fn seed_upload(app: &TestApp, user_id: Uuid) -> Uuid {
    uploads::create(
        &app.pool,
        &uploads::NewUpload {
            user_id,
            kind: "image",
            filename: "a.png",
            bytes: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            mime: "image/png",
            storage_key: "test/attest-review-seed",
            parts: 1,
        },
    )
    .await
    .expect("seed upload")
    .id
}

/// Seed one attestation row directly (the P1-088/090
/// precedent — the list under test needs explicit
/// `created_at` values, and the capture service stamps
/// real time). The stored `ip_hash` is the sha256 of the
/// test address, never the address (NFR-42). Returns the
/// row id.
async fn seed_attestation_at(
    app: &TestApp,
    user_id: Uuid,
    upload_id: Uuid,
    created_at: chrono::DateTime<Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO attestations (user_id, upload_id, text_version, ip_hash, client, created_at) \
         VALUES ($1, $2, 'v1', $3, 'web', $4) RETURNING id",
    )
    .bind(user_id)
    .bind(upload_id)
    .bind(sha256_hex(TEST_IP.as_bytes()))
    .bind(created_at)
    .fetch_one(&app.pool)
    .await
    .expect("seed attestation")
}

/// Seed one abuse report directly for the user (the P1-090
/// helper, with a user this time). Returns the id.
async fn seed_report(app: &TestApp, user_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO abuse_reports (user_id, reporter_contact, reason, status) \
         VALUES ($1, 'reporter@example.com', 'P1-098 probe report', 'open') RETURNING id",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .expect("seed abuse report")
}

/// GET /v1/admin/attestations with the raw query string and
/// the given bearer token (or none for the anonymous probe).
async fn get_review(app: &TestApp, query: &str, token: Option<&str>) -> common::TestResponse {
    let uri = format!("/v1/admin/attestations{query}");
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

/// GET a raw URI (the abuse-queue link is followed as-is)
/// with the given bearer token.
async fn get_uri(app: &TestApp, uri: &str, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// Seed eleven attestations for the user, one second apart,
/// newest first: row `i` sits at `base - i` seconds, so its
/// 24 h velocity count is `11 - i` — exactly the newest
/// row is flagged. The 10 s span can never overlap another
/// run's base (runs are minutes apart — the P1-090
/// precedent), so the burst is the eleven newest rows.
/// Returns the ids, newest first.
async fn seed_velocity_burst(app: &TestApp, user_id: Uuid) -> Vec<Uuid> {
    let base = Utc::now() + Duration::days(365);
    let mut ids = Vec::with_capacity(11);
    for i in 0..11 {
        let upload = seed_upload(app, user_id).await;
        let id = seed_attestation_at(app, user_id, upload, base - Duration::seconds(i)).await;
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn admin_attestations_guard() {
    let app = review_app().await;
    let (admin_id, admin_token) = make_role(
        &app,
        &format!("review-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let (_support_id, support_token) = make_role(
        &app,
        &format!("review-support-{}@example.com", Uuid::now_v7()),
        "support",
    )
    .await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("review-user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant before it can mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token, &["jobs:read", "jobs:write"]).await;

    let res = get_review(&app, "", None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
    let res = get_review(&app, "", Some(&user_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    let res = get_review(&app, "", Some(&admin_key)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    let res = get_review(&app, "", Some(&support_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = get_review(&app, "", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn admin_attestations_velocity_flag() {
    let app = review_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("flag-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let burst_user = register_user(&app, &format!("burst-{}@example.com", Uuid::now_v7())).await;
    let burst_ids = seed_velocity_burst(&app, burst_user).await;
    let calm_user = register_user(&app, &format!("calm-{}@example.com", Uuid::now_v7())).await;
    let calm_upload = seed_upload(&app, calm_user).await;
    let calm_id = seed_attestation_at(
        &app,
        calm_user,
        calm_upload,
        Utc::now() + Duration::days(365) - Duration::minutes(30),
    )
    .await;

    // The burst + the calm row are among the newest rows in
    // the queue — but the sibling tests seed their own
    // bursts in parallel at the same base, so the rows are
    // found by id, never by position (the P1-090
    // precedent). Walk pages until all twelve are seen.
    let mut found: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let query = match &cursor {
            Some(cursor) => format!("?limit=100&cursor={cursor}"),
            None => "?limit=100".to_string(),
        };
        let res = get_review(&app, &query, Some(&admin_token)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        let body = res.json();
        for item in body["items"].as_array().expect("items") {
            found.insert(item["id"].as_str().unwrap().to_string(), item.clone());
        }
        let wanted: Vec<String> = burst_ids
            .iter()
            .map(|id| id.to_string())
            .chain(std::iter::once(calm_id.to_string()))
            .collect();
        if wanted.iter().all(|id| found.contains_key(id)) {
            break;
        }
        cursor = body["next_cursor"].as_str().map(|s| s.to_string());
        assert!(cursor.is_some(), "more pages while rows unseen");
    }
    for id in burst_ids.iter().chain(std::iter::once(&calm_id)) {
        assert!(found.contains_key(&id.to_string()), "seeded row {id} found");
    }

    // Exactly the newest burst row is flagged: count 11,
    // the reason, and the abuse-queue link for its user.
    let newest = &found[&burst_ids[0].to_string()];
    assert_eq!(newest["flagged"], true);
    assert_eq!(newest["velocity_count"], 11);
    assert_eq!(newest["flag_reasons"], json!(["velocity"]));
    assert_eq!(
        newest["abuse_queue_url"],
        format!("/v1/admin/abuse-reports?user_id={burst_user}")
    );
    assert_eq!(newest["text_version"], "v1");
    assert_eq!(newest["client"], "web");
    assert_eq!(newest["user_id"].as_str().unwrap(), &burst_user.to_string());
    // The rest of the burst reads counts 10..1, unflagged.
    for (i, id) in burst_ids[1..].iter().enumerate() {
        let item = &found[&id.to_string()];
        assert_eq!(item["flagged"], false, "burst row {}: {}", i + 1, item);
        assert_eq!(item["velocity_count"], 10 - i as i64);
        assert_eq!(item["flag_reasons"], json!([]));
        assert_eq!(
            item["abuse_queue_url"],
            format!("/v1/admin/abuse-reports?user_id={burst_user}")
        );
    }
    // The calm row: count 1, unflagged, its own user's link.
    let calm = &found[&calm_id.to_string()];
    assert_eq!(calm["flagged"], false);
    assert_eq!(calm["velocity_count"], 1);
    assert_eq!(calm["flag_reasons"], json!([]));
    assert_eq!(
        calm["abuse_queue_url"],
        format!("/v1/admin/abuse-reports?user_id={calm_user}")
    );
    // The burst reads newest first among themselves: sort
    // the found burst rows by `created_at` desc and the ids
    // come back in seed order.
    let mut burst_found: Vec<&serde_json::Value> =
        burst_ids.iter().map(|id| &found[&id.to_string()]).collect();
    burst_found.sort_by(|a, b| {
        b["created_at"]
            .as_str()
            .unwrap()
            .cmp(a["created_at"].as_str().unwrap())
    });
    let ordered: Vec<String> = burst_found
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect();
    let wanted: Vec<String> = burst_ids.iter().map(|id| id.to_string()).collect();
    assert_eq!(ordered, wanted, "newest first, no skips");

    // `?flagged=true` returns only flagged rows — the burst's
    // newest is on the first page (older runs' flagged rows
    // may follow, all flagged by definition of the filter).
    let res = get_review(&app, "?flagged=true&limit=100", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let items = res.json()["items"].as_array().expect("items").clone();
    assert!(!items.is_empty());
    assert!(
        items
            .iter()
            .any(|item| item["id"].as_str().unwrap() == burst_ids[0].to_string().as_str()),
        "the flagged row is listed: {}",
        res.text()
    );
    assert!(items.iter().all(|item| item["flagged"] == true));
    // `?flagged=false` returns only unflagged rows — walk
    // until the burst's other ten + the calm row are seen.
    let mut seen_unflagged: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let query = match &cursor {
            Some(cursor) => format!("?flagged=false&limit=100&cursor={cursor}"),
            None => "?flagged=false&limit=100".to_string(),
        };
        let res = get_review(&app, &query, Some(&admin_token)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        let body = res.json();
        for item in body["items"].as_array().expect("items") {
            assert_eq!(item["flagged"], false, "unflagged page: {}", item);
            seen_unflagged.push(item["id"].as_str().unwrap().to_string());
        }
        let wanted: Vec<String> = burst_ids[1..]
            .iter()
            .map(|id| id.to_string())
            .chain(std::iter::once(calm_id.to_string()))
            .collect();
        if wanted.iter().all(|id| seen_unflagged.contains(id)) {
            break;
        }
        cursor = body["next_cursor"].as_str().map(|s| s.to_string());
        assert!(cursor.is_some(), "more pages while rows unseen");
    }
}

#[tokio::test]
async fn admin_attestations_filters_and_pagination() {
    let app = review_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("page-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let burst_user =
        register_user(&app, &format!("page-burst-{}@example.com", Uuid::now_v7())).await;
    let burst_ids = seed_velocity_burst(&app, burst_user).await;

    // Cursor round-trip at limit 2: the walk stops once all
    // eleven seeded ids are seen, newest first. A page that
    // promises another page is full; the last page (no next
    // cursor) may be short — on a fresh database the queue
    // can end right after the burst (CI-FAIL-028: no older
    // rows sit below until sibling tests seed theirs, so
    // the old unconditional full-page assert failed here).
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let query = match &cursor {
            Some(cursor) => format!("?limit=2&cursor={cursor}"),
            None => "?limit=2".to_string(),
        };
        let res = get_review(&app, &query, Some(&admin_token)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        let body = res.json();
        let items = body["items"].as_array().expect("items");
        let next = body["next_cursor"].as_str().map(|s| s.to_string());
        if next.is_some() {
            assert_eq!(items.len(), 2, "a full page at limit 2: {}", res.text());
        } else {
            assert!(
                !items.is_empty(),
                "empty page while rows unseen: {}",
                res.text()
            );
        }
        for item in items {
            seen.push(item["id"].as_str().unwrap().to_string());
        }
        let wanted: Vec<String> = burst_ids.iter().map(|id| id.to_string()).collect();
        if wanted.iter().all(|id| seen.contains(id)) {
            break;
        }
        cursor = next;
        assert!(cursor.is_some(), "more pages while rows unseen");
    }
    let wanted: Vec<String> = burst_ids.iter().map(|id| id.to_string()).collect();
    assert_eq!(
        seen.iter().filter(|id| wanted.contains(id)).count(),
        11,
        "all eleven seeded ids seen once each"
    );
    assert_eq!(
        seen.iter()
            .filter(|id| wanted.contains(id))
            .collect::<Vec<_>>(),
        wanted.iter().collect::<Vec<_>>(),
        "newest first, no duplicates, no skips"
    );

    // A tampered cursor is 422, never a 500 or a silent
    // restart from the first page.
    let res = get_review(&app, "?cursor=!!!not-a-cursor", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // An out-of-range `ts` is a tampered cursor too (the
    // P1-088 adoption-review lesson).
    let bad_cursor = Cursor {
        ts: i64::MAX,
        id: Uuid::now_v7(),
    }
    .encode();
    let res = get_review(&app, &format!("?cursor={bad_cursor}"), Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // An unknown `flagged` value is 422, not an empty page.
    let res = get_review(&app, "?flagged=yes", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // The limit clamps: 0 up to 1, 500 down to 100.
    let res = get_review(&app, "?flagged=true&limit=0", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["items"].as_array().unwrap().len(), 1);
    let res = get_review(&app, "?limit=500", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(res.json()["items"].as_array().unwrap().len() <= 100);
}

#[tokio::test]
async fn admin_attestations_link_to_abuse_queue() {
    let app = review_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("link-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let burst_user =
        register_user(&app, &format!("link-burst-{}@example.com", Uuid::now_v7())).await;
    let burst_ids = seed_velocity_burst(&app, burst_user).await;
    let other_user =
        register_user(&app, &format!("link-other-{}@example.com", Uuid::now_v7())).await;
    let own_report = seed_report(&app, burst_user).await;
    let other_report = seed_report(&app, other_user).await;

    // The newest burst row links to the burst user's queue
    // (walk pages — the sibling tests seed their own bursts
    // in parallel, so the row is found by id, never by
    // position).
    let mut url: Option<String> = None;
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let query = match &cursor {
            Some(cursor) => format!("?limit=100&cursor={cursor}"),
            None => "?limit=100".to_string(),
        };
        let res = get_review(&app, &query, Some(&admin_token)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        let body = res.json();
        for item in body["items"].as_array().expect("items") {
            if item["id"].as_str().unwrap() == burst_ids[0].to_string().as_str() {
                url = Some(item["abuse_queue_url"].as_str().unwrap().to_string());
                break;
            }
        }
        if url.is_some() {
            break;
        }
        cursor = body["next_cursor"].as_str().map(|s| s.to_string());
        assert!(cursor.is_some(), "more pages while the row is unseen");
    }
    let url = url.expect("the flagged row is listed");
    assert_eq!(url, format!("/v1/admin/abuse-reports?user_id={burst_user}"));

    // Following the link lists that user's reports and no
    // other user's.
    let res = get_uri(&app, &url, &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let items = res.json()["items"].as_array().expect("items").clone();
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&own_report.to_string().as_str()),
        "own report: {}",
        res.text()
    );
    assert!(
        !ids.contains(&other_report.to_string().as_str()),
        "other user's report: {}",
        res.text()
    );

    // An unknown user is an empty page, not a 404 (a list
    // never 404s on its filter — the P1-088 precedent).
    let res = get_uri(
        &app,
        &format!("/v1/admin/abuse-reports?user_id={}", Uuid::now_v7()),
        &admin_token,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["items"].as_array().unwrap().len(), 0);
    assert!(res.json().get("next_cursor").is_none());
    // A malformed `user_id` is 422 via the Query extractor.
    let res = get_uri(&app, "/v1/admin/abuse-reports?user_id=nope", &admin_token).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
}
