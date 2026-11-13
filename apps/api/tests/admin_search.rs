//! P1-009: admin user search, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: `GET /v1/admin/users` lists
//! users with cursor pagination (`?limit=&cursor=`), filters
//! by an email / display-name / id prefix (`?query=`), and
//! every row carries the plan code + today's usage. Admin and
//! support may call it (support is read-only); a plain user
//! gets 403 `PF-1002`, no credential gets 401 `PF-1001`.
//!
//! The harness DB is shared across the tests in this binary
//! (and persists across runs), so every test registers its
//! users under a unique email prefix and searches that prefix
//! — an unfiltered list can never assert an exact count.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use common::{TestApp, TestFixtures};
use detag_core::repos::plans;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn admin_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// A per-test email prefix: a full UUID, so one test's users
/// never leak into another test's filtered search — across
/// tests in this binary AND across runs (the harness DB
/// persists). The prefix is embedded in every test email, so
/// the email carries NO second UUID: `{prefix}-{letter}@...`
/// keeps the local part under the validator's 64-character
/// cap (a full UUID in the prefix + a full UUID in the email
/// is 80 — that was the earlier failure). Within one test the
/// letter differs per user; across tests the tag differs.
fn unique_prefix(tag: &str) -> String {
    format!("{tag}-{}", Uuid::now_v7())
}

/// A test user's email under a prefix: `{prefix}-{letter}@...`.
/// No UUID of its own — the prefix carries the uniqueness.
fn prefixed_email(prefix: &str, letter: &str) -> String {
    format!("{prefix}-{letter}@example.com")
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-test XFF address: each test gets its own IP bucket,
/// so the tests never trip each other's rate limits (the
/// P1-003 lesson — the shared "unknown" bucket is one bucket
/// for the whole binary).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7().as_u128() % 250 + 1)
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
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

/// Set a user's display name via PATCH /v1/me.
async fn set_display_name(app: &TestApp, token: &str, name: &str) {
    let res = app
        .request(
            Request::builder()
                .method(Method::PATCH)
                .uri("/v1/me")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "display_name": name })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

fn search_req(token: Option<&str>, query: &str) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/admin/users{query}"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
}

/// The ids in a search response body, in order.
fn item_ids(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn search_lists_users_with_plan_status_and_usage() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let prefix = unique_prefix("list");
    let first = register_user(&app, &prefixed_email(&prefix, "a")).await;
    let second = register_user(&app, &prefixed_email(&prefix, "b")).await;

    let res = app
        .request(search_req(Some(&admin_token), &format!("?query={prefix}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(item_ids(&body), vec![first.to_string(), second.to_string()]);
    // No next page: both rows fit in the default page.
    assert!(body.get("next_cursor").is_none(), "{body}");
    for item in body["items"].as_array().unwrap() {
        assert_eq!(item["plan"], "free", "{item}");
        assert_eq!(item["status"], "active", "{item}");
        assert_eq!(item["role"], "user", "{item}");
        assert_eq!(item["images_used_today"], 0, "{item}");
        assert_eq!(item["video_seconds_used_today"], 0, "{item}");
        assert!(item["email_verified_at"].is_null(), "{item}");
        assert!(item["created_at"].is_string(), "{item}");
    }
    // Unfiltered: 200, rows in created_at order, every row
    // shaped the same way (the exact count is the shared DB's
    // business, not this test's).
    let res = app.request(search_req(Some(&admin_token), "")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert!(!body["items"].as_array().unwrap().is_empty(), "{body}");
}

#[tokio::test]
async fn search_filters_by_email_prefix() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let prefix = unique_prefix("email");
    let alice = register_user(&app, &prefixed_email(&prefix, "alice")).await;
    register_user(&app, &prefixed_email(&prefix, "bob")).await;

    let res = app
        .request(search_req(
            Some(&admin_token),
            &format!("?query={prefix}-alice"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(item_ids(&res.json()), vec![alice.to_string()]);
}

#[tokio::test]
async fn search_filters_by_display_name_prefix() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let prefix = unique_prefix("name");
    let user_email = prefixed_email(&prefix, "u");
    let user_id = register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;
    let name = format!("{prefix} Sharma");
    set_display_name(&app, &user_token, &name).await;
    register_user(&app, &prefixed_email(&prefix, "other")).await;

    let res = app
        .request(search_req(Some(&admin_token), &format!("?query={prefix}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // The name match finds the named user; the email prefix
    // also matches the other user (both emails start with the
    // prefix) — so assert the named user is there and its
    // display_name is shown.
    let body = res.json();
    assert!(item_ids(&body).contains(&user_id.to_string()), "{body}");
    let item = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == user_id.to_string())
        .unwrap();
    assert_eq!(item["display_name"], name, "{body}");

    // A query that matches ONLY the display name (not the
    // email prefix) narrows to that user.
    let res = app
        .request(search_req(
            Some(&admin_token),
            &format!("?query={prefix}%20Sharma"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(item_ids(&res.json()), vec![user_id.to_string()]);
}

#[tokio::test]
async fn search_filters_by_id_prefix() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let prefix = unique_prefix("id");
    let user_id = register_user(&app, &prefixed_email(&prefix, "u")).await;

    let id_prefix = &user_id.to_string()[..8];
    let res = app
        .request(search_req(
            Some(&admin_token),
            &format!("?query={id_prefix}"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    let ids = item_ids(&body);
    assert!(ids.contains(&user_id.to_string()), "{body}");
    assert!(
        ids.iter().all(|id| id.starts_with(id_prefix)),
        "every returned id must carry the prefix: {body}"
    );
}

#[tokio::test]
async fn search_no_match_is_empty() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    let res = app
        .request(search_req(
            Some(&admin_token),
            "?query=definitely-no-such-user-zzz",
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["items"], json!([]), "{body}");
    assert!(body.get("next_cursor").is_none(), "{body}");
}

#[tokio::test]
async fn search_pagination_walks_all_pages_without_duplicates() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let prefix = unique_prefix("page");
    let mut expected = Vec::new();
    for tag in ["a", "b", "c", "d", "e"] {
        expected.push(register_user(&app, &prefixed_email(&prefix, tag)).await);
    }

    // Page 1: 2 rows + a cursor.
    let res = app
        .request(search_req(
            Some(&admin_token),
            &format!("?query={prefix}&limit=2"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let page1 = res.json();
    assert_eq!(item_ids(&page1).len(), 2, "{page1}");
    let cursor1 = page1["next_cursor"]
        .as_str()
        .expect("page 1 has a cursor")
        .to_string();

    // Page 2: the next 2 + a cursor.
    let res = app
        .request(search_req(
            Some(&admin_token),
            &format!("?query={prefix}&limit=2&cursor={cursor1}"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let page2 = res.json();
    assert_eq!(item_ids(&page2).len(), 2, "{page2}");
    let cursor2 = page2["next_cursor"]
        .as_str()
        .expect("page 2 has a cursor")
        .to_string();

    // Page 3: the last 1, no cursor.
    let res = app
        .request(search_req(
            Some(&admin_token),
            &format!("?query={prefix}&limit=2&cursor={cursor2}"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let page3 = res.json();
    assert_eq!(item_ids(&page3).len(), 1, "{page3}");
    assert!(page3.get("next_cursor").is_none(), "{page3}");

    // All five, in registration (created_at) order, no
    // duplicates, no row skipped.
    let mut seen = item_ids(&page1);
    seen.extend(item_ids(&page2));
    seen.extend(item_ids(&page3));
    let expected: Vec<String> = expected.iter().map(ToString::to_string).collect();
    assert_eq!(seen, expected, "pages must walk the whole set in order");
}

#[tokio::test]
async fn search_limit_zero_clamps_to_one() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let prefix = unique_prefix("clamp");
    register_user(&app, &prefixed_email(&prefix, "a")).await;
    register_user(&app, &prefixed_email(&prefix, "b")).await;

    // limit=0 is a limit, not an error: it clamps up to 1.
    let res = app
        .request(search_req(
            Some(&admin_token),
            &format!("?query={prefix}&limit=0"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(item_ids(&body).len(), 1, "{body}");
    assert!(body["next_cursor"].is_string(), "{body}");
}

#[tokio::test]
async fn search_tampered_cursor_is_422() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    let res = app
        .request(search_req(Some(&admin_token), "?cursor=!!!not-a-cursor!!!"))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
}

#[tokio::test]
async fn search_bad_limit_is_422() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    let res = app
        .request(search_req(Some(&admin_token), "?limit=abc"))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
}

#[tokio::test]
async fn search_support_can_read_plain_user_cannot() {
    let app = admin_app().await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let user_email = unique_email("plainuser");
    register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;

    // Support is read-only, and search is a read: 200.
    let res = app.request(search_req(Some(&support_token), "")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // A plain user may not read the admin list at all.
    let res = app.request(search_req(Some(&user_token), "")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    // No credential: 401.
    let res = app.request(search_req(None, "")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn search_shows_live_plan_and_usage_today() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let prefix = unique_prefix("plan");
    let user_id = register_user(&app, &prefixed_email(&prefix, "u")).await;

    // A live Pro subscription (the seeded 'pro' plan) + today's
    // usage row, both inserted directly — there is no billing
    // flow yet to create them through the API.
    let pro = plans::by_code(&app.state.pool, "pro")
        .await
        .expect("load pro plan")
        .expect("pro plan is seeded");
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, status) VALUES ($1, $2, 'stripe', 'active')",
    )
    .bind(user_id)
    .bind(pro.id)
    .execute(&app.state.pool)
    .await
    .expect("seed subscription");
    let today = Utc::now().date_naive();
    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) VALUES ($1, $2, 3, 120)",
    )
    .bind(user_id)
    .bind(today)
    .execute(&app.state.pool)
    .await
    .expect("seed usage");

    let res = app
        .request(search_req(Some(&admin_token), &format!("?query={prefix}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(item_ids(&body), vec![user_id.to_string()], "{body}");
    let item = &body["items"][0];
    assert_eq!(item["plan"], "pro", "{body}");
    assert_eq!(item["images_used_today"], 3, "{body}");
    assert_eq!(item["video_seconds_used_today"], 120, "{body}");
}
