//! `POST /v1/admin/users/{id}/notes` (P1-110, FR-C-02).
//!
//! An admin appends a ticket note to a user's append-only
//! list — one JSON object per note in `users.support_notes`,
//! audited as `admin.users.note_added`. Support reads the
//! notes through the P1-106 user view (a sixth section), not
//! through this endpoint. The five tests:
//!
//! - `ticket_notes_guard` — anonymous is 401 `PF-1001`, a plain
//!   user and support are 403 `PF-1002`, an admin user's API
//!   key is 403 (no admin ever), an admin is 200 — and the
//!   denials write no note and no audit row.
//! - `ticket_note_append_shape_and_audit` — the note shape (id,
//!   author, role at write time, trimmed body, created_at on
//!   the frozen clock) and exactly one `admin.users.note_added`
//!   audit row with the right actor, target and after
//!   (`{note_id, body_chars}` — no verbatim body, no reason).
//! - `ticket_notes_append_only_order_and_trigger` — the AC: two
//!   appends land oldest-first, and a direct SQL truncation
//!   UPDATE fails on the `users_support_notes_append_only`
//!   trigger, leaving both notes in place.
//! - `ticket_notes_visible_in_user_view` — support reads the
//!   notes with the P1-106 view (the sixth section): a fresh
//!   user shows `[]`, after one append the view carries it.
//! - `ticket_note_validation` — a blank body, a 2001-char
//!   body, a control character, and an unknown key are each
//!   422 and write nothing; an unknown user is 404 `PF-2010`
//!   and audits nothing.
//!
//! The app runs on the frozen clock (Thursday 2026-11-19,
//! 12:00 UTC) — the note's `created_at` is the frozen now,
//! not the wall clock.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, NaiveDate, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    clock::{Clock, ManualClock},
    repos::{audit_logs, users},
};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The frozen "now": Thursday 2026-11-19, 12:00 UTC.
fn frozen_now() -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 11, 19)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        Utc,
    )
}

/// App on the frozen clock. No billing provider: the
/// endpoint never calls one.
async fn ticket_notes_app() -> (TestApp, Arc<ManualClock>) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    let clock = Arc::new(ManualClock::new(frozen_now()));
    let app = TestApp::spawn_with_fixtures_settings_and_clock(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .await;
    (app, clock)
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// Register a user, set the role in the database, log in,
/// return (id, token). There is no self-serve way to become
/// an admin or support (the role is set by the owner out of
/// band).
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

/// Register + log in; return (user id, token).
async fn authed_user(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let email = unique_email(prefix);
    let id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    (id, token)
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// Mint an API key for the user (P1-043 helper): the key is
/// returned once, at creation.
async fn mint_api_key(app: &TestApp, token: &str) -> String {
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "name": "notes probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
}

/// POST the notes endpoint as the given caller.
async fn post_note(
    app: &TestApp,
    target: Uuid,
    credential: Option<&str>,
    body: Value,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/admin/users/{target}/notes"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = credential {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(
        req.body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// GET the P1-106 user view as the given caller.
async fn get_user_view(app: &TestApp, target: Uuid, credential: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri(format!("/v1/admin/users/{target}"))
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {credential}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// The problem code in a problem+json body.
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

/// The `admin.users.note_added` audit rows for a user.
async fn note_audit_rows(app: &TestApp, user_id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audit rows")
        .into_iter()
        .filter(|row| row.action == "admin.users.note_added")
        .collect()
}

/// The user's stored notes, oldest first.
async fn stored_notes(app: &TestApp, user_id: Uuid) -> Vec<Value> {
    users::support_notes(&app.pool, user_id)
        .await
        .expect("read notes")
        .expect("user exists")
}

#[tokio::test]
async fn ticket_notes_guard() {
    let (app, _) = ticket_notes_app().await;
    let (target, _) = authed_user(&app, "notes-target").await;
    let (user, user_token) = authed_user(&app, "notes-user").await;
    let _ = user;
    let (_, support_token) = make_role(&app, &unique_email("notes-support"), "support").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("notes-admin"), "admin").await;
    // P1-055: key creation is plan-gated — the admin needs a
    // Pro grant before it can mint the probe key (keys are a
    // Pro/Business feature, FR-P-01; the admin_guard
    // precedent).
    app.grant_plan(admin_id, "pro").await;
    let admin_key = mint_api_key(&app, &admin_token).await;
    let body = json!({ "body": "guard probe" });

    // Anonymous: 401 PF-1001.
    let res = post_note(&app, target, None, body.clone()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");

    // Plain user: 403 PF-1002.
    let res = post_note(&app, target, Some(&user_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // Support: 403 PF-1002 — the note is an admin-only
    // write (the P1-042 matrix; support reads notes through
    // the view, never writes them).
    let res = post_note(&app, target, Some(&support_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // An admin user's own API key: 403 PF-1002 (no admin
    // ever).
    let res = post_note(&app, target, Some(&admin_key), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // The denials wrote nothing: no note, no audit row.
    assert_eq!(stored_notes(&app, target).await.len(), 0);
    assert_eq!(note_audit_rows(&app, target).await.len(), 0);

    // Admin: 200, and the note is the one returned.
    let res = post_note(&app, target, Some(&admin_token), body).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let notes = res.json();
    assert_eq!(notes["user_id"].as_str().unwrap(), target.to_string());
    assert_eq!(notes["notes"].as_array().unwrap().len(), 1);
    assert_eq!(notes["notes"][0]["body"], "guard probe");
}

#[tokio::test]
async fn ticket_note_append_shape_and_audit() {
    let (app, _) = ticket_notes_app().await;
    let (target, _) = authed_user(&app, "notes-shape").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("notes-admin"), "admin").await;
    let before = note_audit_rows(&app, target).await.len();

    let res = post_note(
        &app,
        target,
        Some(&admin_token),
        json!({ "body": "  Called the user, they will re-upload on Monday.  " }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["user_id"].as_str().unwrap(), target.to_string());
    let notes = body["notes"].as_array().unwrap().clone();
    assert_eq!(notes.len(), 1, "one note after the first append");
    let note = &notes[0];
    // The body is stored trimmed; the id is a uuid; the
    // author is the admin with the role recorded at write
    // time; the stamp is the frozen now, not the wall clock.
    assert_eq!(
        note["body"],
        "Called the user, they will re-upload on Monday."
    );
    assert!(note["id"].as_str().unwrap().parse::<Uuid>().is_ok());
    assert_eq!(note["author_id"].as_str().unwrap(), admin_id.to_string());
    assert_eq!(note["author_role"], "admin");
    assert_eq!(note["created_at"].as_str().unwrap(), "2026-11-19T12:00:00Z");

    // Exactly one new audit row — actor the admin, target
    // the user, after carries the note id + the body length
    // (47 chars — NOT the verbatim body), and no reason.
    let rows = note_audit_rows(&app, target).await;
    assert_eq!(rows.len(), before + 1, "one audit row per append");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(row.target_id.as_deref(), Some(target.to_string().as_str()));
    assert_eq!(
        row.after.as_ref().unwrap()["note_id"].as_str().unwrap(),
        note["id"].as_str().unwrap()
    );
    assert_eq!(row.after.as_ref().unwrap()["body_chars"], 47);
    assert!(row.reason.is_none(), "a note append carries no reason");
    let after_json = serde_json::to_string(row.after.as_ref().unwrap()).unwrap();
    assert!(
        !after_json.contains("re-upload"),
        "the audit row must not carry the note body: {after_json}"
    );
}

#[tokio::test]
async fn ticket_notes_append_only_order_and_trigger() {
    let (app, _) = ticket_notes_app().await;
    let (target, _) = authed_user(&app, "notes-order").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("notes-admin"), "admin").await;
    let _ = admin_id;

    // Two appends: the response lists both, oldest first.
    for text in ["first note", "second note"] {
        let res = post_note(&app, target, Some(&admin_token), json!({ "body": text })).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    }
    let notes = stored_notes(&app, target).await;
    assert_eq!(notes.len(), 2, "both appends landed");
    assert_eq!(notes[0]["body"], "first note");
    assert_eq!(notes[1]["body"], "second note");

    // The trigger: a direct truncation UPDATE fails, and
    // both notes are still there afterwards (the AC, proven
    // at the database, not just by the absence of an
    // endpoint).
    let err = sqlx::query("UPDATE users SET support_notes = '[]'::jsonb WHERE id = $1")
        .bind(target)
        .execute(&app.pool)
        .await
        .expect_err("truncation must fail on the append-only trigger");
    let message = match &err {
        sqlx::Error::Database(db_err) => db_err.message().to_string(),
        other => panic!("expected a database error, got {other:?}"),
    };
    assert!(
        message.contains("append-only"),
        "the trigger must name the rule: {message}"
    );
    let notes = stored_notes(&app, target).await;
    assert_eq!(notes.len(), 2, "the failed truncation changed nothing");
    assert_eq!(notes[0]["body"], "first note");
    assert_eq!(notes[1]["body"], "second note");

    // An edit fails the same way (the old element must
    // survive verbatim — rewriting it is not an append).
    let err = sqlx::query(
        "UPDATE users SET support_notes = '[{\"body\": \"rewritten\"}]'::jsonb WHERE id = $1",
    )
    .bind(target)
    .execute(&app.pool)
    .await
    .expect_err("a rewrite must fail on the append-only trigger");
    let message = match &err {
        sqlx::Error::Database(db_err) => db_err.message().to_string(),
        other => panic!("expected a database error, got {other:?}"),
    };
    assert!(message.contains("append-only"), "{message}");
    assert_eq!(stored_notes(&app, target).await.len(), 2);
}

#[tokio::test]
async fn ticket_notes_visible_in_user_view() {
    let (app, _) = ticket_notes_app().await;
    let (target, _) = authed_user(&app, "notes-view").await;
    let (_, support_token) = make_role(&app, &unique_email("notes-support"), "support").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("notes-admin"), "admin").await;
    let _ = admin_id;

    // A fresh user shows an empty sixth section.
    let res = get_user_view(&app, target, &support_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json()["support_notes"].as_array().unwrap().len(),
        0,
        "a fresh user has no notes"
    );

    // After one append, support reads it with the view —
    // the same list the POST returned, oldest first.
    let res = post_note(
        &app,
        target,
        Some(&admin_token),
        json!({ "body": "view probe" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let posted = res.json()["notes"].as_array().unwrap().clone();
    let res = get_user_view(&app, target, &support_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let view_notes = res.json()["support_notes"].as_array().unwrap().clone();
    assert_eq!(view_notes, posted, "the view carries the same notes");
    assert_eq!(view_notes[0]["body"], "view probe");
    assert_eq!(view_notes[0]["author_role"], "admin");
}

#[tokio::test]
async fn ticket_note_validation() {
    let (app, _) = ticket_notes_app().await;
    let (target, _) = authed_user(&app, "notes-valid").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("notes-admin"), "admin").await;
    let _ = admin_id;
    let unknown = Uuid::now_v7();

    // A blank body: 422, nothing written.
    let res = post_note(&app, target, Some(&admin_token), json!({ "body": "   " })).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(stored_notes(&app, target).await.len(), 0);

    // A 2001-char body: 422, nothing written.
    let res = post_note(
        &app,
        target,
        Some(&admin_token),
        json!({ "body": "a".repeat(2001) }),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(stored_notes(&app, target).await.len(), 0);

    // A control character in the body: 422, nothing written.
    let res = post_note(
        &app,
        target,
        Some(&admin_token),
        json!({ "body": "has\ta tab" }),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(stored_notes(&app, target).await.len(), 0);

    // An unknown key: 422 (`deny_unknown_fields`), nothing
    // written.
    let res = post_note(
        &app,
        target,
        Some(&admin_token),
        json!({ "body": "fine", "extra": "nope" }),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(stored_notes(&app, target).await.len(), 0);

    // An unknown user: 404 PF-2010, and no audit row.
    let before = note_audit_rows(&app, unknown).await.len();
    let res = post_note(
        &app,
        unknown,
        Some(&admin_token),
        json!({ "body": "unknown user" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    assert_eq!(
        note_audit_rows(&app, unknown).await.len(),
        before,
        "a 404 audits nothing"
    );
}
