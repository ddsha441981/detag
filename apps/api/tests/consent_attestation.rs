//! P1-099: the consent/attestation contract, end to end.
//!
//! One suite that re-proves the three spec properties as
//! user journeys through the real router (`TestApp` oneshot)
//! against the migrated scratch database — in the P1-092
//! matrix-suite pattern, so a future change must keep the
//! whole contract green in one place instead of relying on
//! the implementing tickets' own files (P1-094's gate,
//! P1-095's consent versioning, P1-096's onboarding
//! reopening, migration 004's trigger).
//!
//! - Job without attestation impossible: no attestation →
//!   403 `PF-4001` and no job row (the P1-094 AC, restated
//!   here as the suite anchor); an attestation for a
//!   *different* upload does not open the gate; at the
//!   service level `require_for_upload` is scoped to the
//!   caller (another user's attestation is `AttestationMissing`)
//!   and over HTTP a foreign upload is 404 `PF-2010` before
//!   the gate runs (the P1-045 IDOR rule).
//! - Version bump forces re-accept: the forcing mechanism
//!   is P1-095's consent versioning — register at v1, then
//!   on the v2 app the seeded v1 row reads `up_to_date`
//!   false, a stale re-accept is 422, a current re-accept
//!   is 200 with exactly one `user.consent_accepted` audit
//!   row, and the bump reopens the onboarding `accept_tos`
//!   step until the re-accept lands (the P1-096 behavior).
//!   The attestation text version is pinned as recorded
//!   evidence, NOT a gate input (the P1-094 `client`
//!   precedent): a row captured at a superseded version
//!   still satisfies the gate — the gate proves a row
//!   exists, it does not police its version, and no job
//!   block follows a consent bump (ASSUMPTION-056); forcing
//!   a re-attestation on a text bump is a future owner
//!   decision (ADR-0062), recorded honestly, not silently
//!   assumed.
//! - Immutability enforced: at the SERVICE level (the P1-087
//!   `append_only_denied_on_service_written_row` precedent —
//!   the migration-004 test only proved the trigger on
//!   raw-SQL rows), UPDATE on a `capture`-written attestation
//!   row fails on the `attestations_immutable` trigger and
//!   the row survives verbatim — while DELETE succeeds, by
//!   design: the P1-007 purge migration made the trigger
//!   UPDATE-only, because DELETE is the GDPR purge's erasure
//!   path (the FK cascade from a user purge) and keeping the
//!   rows would retain the user's data after erasure;
//!   "immutable" means the record can never be ALTERED, not
//!   that it can never be erased. The audit trail is
//!   different: UPDATE + DELETE on the re-accept's
//!   `user.consent_accepted` audit row both fail on the audit
//!   append-only trigger (which kept its DELETE branch) and
//!   the row survives.
//!
//! The token comes from a real login (register + login
//! through the router), never minted by hand.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    attestation::{capture, require_for_upload, AttestationError, Client, CURRENT_TEXT_VERSION},
    password::{hash_password, Argon2Params},
    repos::{attestations, audit_logs, uploads, users},
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174105";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The client IP the tests attest from (TEST-NET-3).
const TEST_IP: &str = "203.0.113.7";

/// App with both current versions at v1 (the default).
async fn suite_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    settings.aup_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App with both current versions bumped to v2 — the bumped
/// config a re-accept is tested against. A user seeded with
/// v1/v1 through the repo is exactly what a production bump
/// leaves behind: config changed, rows didn't.
async fn suite_app_v2() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v2".to_string();
    settings.aup_current_version = "v2".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-test client identity (TEST-NET-3; the middleware and
/// the lockout treat the XFF first entry as an opaque key, so
/// a UUID suffix is a valid distinct address here — FLAKY-006:
/// the old `% 250` pool let one test's lockout 423 another
/// test's login, so every test draws its own bucket).
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

/// Register + log in; return (user id, token).
async fn authed_user(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let email = unique_email(prefix);
    let id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    (id, token)
}

/// Seed a user with v1/v1 consent through the repo (a real
/// Argon2id hash, so the test can log in through the router).
/// Used on the v2 app: the row a config bump leaves behind.
async fn seed_v1_user(app: &TestApp, email: &str) -> Uuid {
    let hash = hash_password(PASSWORD, &Argon2Params::defaults()).unwrap();
    users::create_with_consent(&app.pool, email, &hash, Some("v1"), Some("v1"))
        .await
        .expect("seed v1 user")
        .id
}

/// Seed an upload for the user (the quota_middleware helper —
/// `duration_s` is set with an UPDATE: `NewUpload` carries no
/// duration, it is filled in on complete in Phase 3).
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
            storage_key: "test/consent-attest-seed",
            parts: 1,
        },
    )
    .await
    .expect("seed upload")
    .id
}

/// Capture an attestation for the user's upload through the
/// service (upload-create is still the 501 stub, so the
/// tests capture directly — Phase 2's intake calls this
/// same function with the attestation from the create body).
async fn seed_attestation(app: &TestApp, user_id: Uuid, upload_id: Uuid) {
    capture(
        &app.pool,
        user_id,
        upload_id,
        CURRENT_TEXT_VERSION,
        TEST_IP,
        Client::Web,
    )
    .await
    .expect("capture attestation");
}

/// POST /v1/jobs with a JSON body as the user.
async fn post_job(app: &TestApp, token: &str, body: Value) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// The quota headers on a response, as (images, video-seconds).
fn quota_headers(res: &common::TestResponse) -> (String, String) {
    let images = res
        .headers
        .get("x-quota-images-remaining")
        .expect("images header present")
        .to_str()
        .unwrap()
        .to_string();
    let video = res
        .headers
        .get("x-quota-video-seconds-remaining")
        .expect("video-seconds header present")
        .to_str()
        .unwrap()
        .to_string();
    (images, video)
}

/// Assert a problem+json body: media type, code, title, and
/// status.
fn assert_problem(res: &common::TestResponse, status: StatusCode, code: &str, title: &str) {
    assert_eq!(res.status, status, "{}", res.text());
    let content_type = res
        .headers
        .get("content-type")
        .expect("content-type present")
        .to_str()
        .unwrap();
    assert_eq!(content_type, "application/problem+json");
    let body = res.json();
    assert_eq!(body["code"], code, "{}", res.text());
    assert_eq!(body["title"], title, "{}", res.text());
    assert_eq!(body["status"], status.as_u16(), "{}", res.text());
}

/// How many job rows the user has (a rejection must create
/// none).
async fn jobs_count(app: &TestApp, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("count jobs")
}

fn get_me(token: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn post_consent(token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/me/consent")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get_onboarding(token: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me/onboarding")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

/// One step of the onboarding checklist, by id.
fn step<'a>(checklist: &'a Value, id: &str) -> &'a Value {
    checklist["steps"]
        .as_array()
        .expect("steps is an array")
        .iter()
        .find(|step| step["id"] == id)
        .unwrap_or_else(|| panic!("step {id} missing: {checklist}"))
}

/// A completed_at value parsed back to a timestamp (the
/// response carries RFC 3339; the DB carries microseconds —
/// the round-trip is exact).
fn completed_at(checklist: &Value, id: &str) -> Option<DateTime<Utc>> {
    step(checklist, id)["completed_at"]
        .as_str()
        .map(|s| s.parse().expect("completed_at parses"))
}

/// The `user.consent_accepted` audit rows for one user.
async fn consent_audit_rows(app: &TestApp, user_id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_action(&app.pool, "user.consent_accepted")
        .await
        .expect("list audits")
        .into_iter()
        .filter(|row| row.target_id.as_deref() == Some(&user_id.to_string()))
        .collect()
}

#[tokio::test]
async fn suite_job_without_attestation_is_impossible() {
    let app = suite_app().await;
    let (user, token) = authed_user(&app, "suite-none").await;
    let upload = seed_upload(&app, user).await;

    // The suite anchor (the P1-094 AC, restated here so the
    // contract lives in this file too): no attestation row,
    // no job — 403 `PF-4001` with the quota headers, and no
    // job row is created.
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::FORBIDDEN,
        "PF-4001",
        "attestation_missing",
    );
    // P1-052: every job-creating response carries the quota
    // headers — Free, no usage: 10 images left, 0 video-seconds.
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));
    assert_eq!(
        jobs_count(&app, user).await,
        0,
        "a rejected create creates no job row"
    );
}

#[tokio::test]
async fn attestation_for_another_upload_does_not_open_the_gate() {
    let app = suite_app().await;
    let (user, token) = authed_user(&app, "suite-other-upload").await;
    let upload_a = seed_upload(&app, user).await;
    let upload_b = seed_upload(&app, user).await;
    seed_attestation(&app, user, upload_b).await;

    // The attestation is for upload B, not A: creating a job
    // from A is still impossible — the gate is per-upload,
    // not per-user.
    let res = post_job(&app, &token, json!({ "upload_id": upload_a })).await;
    assert_problem(
        &res,
        StatusCode::FORBIDDEN,
        "PF-4001",
        "attestation_missing",
    );
    assert_eq!(jobs_count(&app, user).await, 0);

    // And the attested upload itself still opens: the gate
    // is exact, not broken by the rejection above.
    let res = post_job(&app, &token, json!({ "upload_id": upload_b })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
}

#[tokio::test]
async fn foreign_upload_job_create_is_404() {
    let app = suite_app().await;
    let (owner, _) = authed_user(&app, "suite-owner").await;
    let (other, other_token) = authed_user(&app, "suite-other").await;
    let upload = seed_upload(&app, owner).await;
    seed_attestation(&app, owner, upload).await;

    // The other user names the owner's upload: the upload is
    // invisible to them (the P1-045 IDOR rule) — 404, never a
    // leak that it exists, and never a ride on the owner's
    // attestation. No job row for either user.
    let res = post_job(&app, &other_token, json!({ "upload_id": upload })).await;
    assert_problem(&res, StatusCode::NOT_FOUND, "PF-2010", "not_found");
    assert_eq!(jobs_count(&app, owner).await, 0);
    assert_eq!(jobs_count(&app, other).await, 0);
}

#[tokio::test]
async fn require_for_upload_is_scoped_to_the_caller() {
    let app = suite_app().await;
    let (owner, _) = authed_user(&app, "suite-scope-owner").await;
    let (other, _) = authed_user(&app, "suite-scope-other").await;
    let upload = seed_upload(&app, owner).await;
    seed_attestation(&app, owner, upload).await;

    // The owner's own gate opens…
    let row = require_for_upload(&app.pool, owner, upload)
        .await
        .expect("owner's gate opens");
    assert_eq!(row.user_id, owner);
    assert_eq!(row.upload_id, upload);

    // …but another user's attestation check on the same
    // upload is missing: the gate is scoped to the caller's
    // rows, so no other user's attestation can ever satisfy
    // it — even if the HTTP layer ever stopped 404ing the
    // foreign upload first.
    let err = require_for_upload(&app.pool, other, upload).await;
    assert!(
        matches!(err, Err(AttestationError::AttestationMissing)),
        "another user's attestation must not satisfy the gate: {err:?}"
    );
}

#[tokio::test]
async fn consent_bump_forces_reaccept() {
    // At v1 the register is current: the banner is clear.
    let app = suite_app().await;
    let email = unique_email("suite-bump");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let res = app.request(get_me(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["consent"]["up_to_date"], true);

    // The bump: a v2 app with the same user seeded at v1/v1
    // — exactly what a production config bump leaves behind.
    let app2 = suite_app_v2().await;
    let email2 = unique_email("suite-bump-v2");
    let user2 = seed_v1_user(&app2, &email2).await;
    let token2 = login_token(&app2, &email2).await;
    let res = app2.request(get_me(&token2)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let consent = &res.json()["consent"];
    assert_eq!(consent["tos_version"], "v1", "{consent}");
    assert_eq!(consent["aup_version"], "v1", "{consent}");
    assert_eq!(consent["tos_current_version"], "v2", "{consent}");
    assert_eq!(consent["aup_current_version"], "v2", "{consent}");
    assert_eq!(consent["up_to_date"], false, "{consent}");

    // Re-accepting the stale versions is refused — the bump
    // forces a re-accept of the CURRENT text, not a
    // re-statement of the old one.
    let res = app2
        .request(post_consent(
            &token2,
            json!({"accept_tos_version": "v1", "accept_aup_version": "v1"}),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    let fields = json["field_errors"].as_array().expect("field errors");
    assert_eq!(fields.len(), 2, "{json}");
    assert_eq!(fields[0]["field"], "accept_tos_version");
    assert_eq!(fields[0]["code"], "tos_version_mismatch");
    assert_eq!(fields[1]["field"], "accept_aup_version");
    assert_eq!(fields[1]["code"], "aup_version_mismatch");

    // Re-accepting the current versions clears the banner,
    // updates the row, and writes exactly one audit row.
    let res = app2
        .request(post_consent(
            &token2,
            json!({"accept_tos_version": "v2", "accept_aup_version": "v2"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let consent = &res.json()["consent"];
    assert_eq!(consent["tos_version"], "v2", "{consent}");
    assert_eq!(consent["aup_version"], "v2", "{consent}");
    assert_eq!(consent["up_to_date"], true, "{consent}");
    let user = users::by_id(&app2.pool, user2)
        .await
        .unwrap()
        .expect("user row exists");
    assert_eq!(user.tos_version.as_deref(), Some("v2"));
    assert_eq!(user.aup_version.as_deref(), Some("v2"));
    let rows = consent_audit_rows(&app2, user2).await;
    assert_eq!(rows.len(), 1, "exactly one re-accept audit row");
    assert_eq!(rows[0].actor_user_id, Some(user2));
    assert_eq!(rows[0].target_type, "user");
    let after = rows[0].after.as_ref().expect("audit after exists");
    assert_eq!(after["tos_version"], "v2");
    assert_eq!(after["aup_version"], "v2");

    let _ = user_id;
}

#[tokio::test]
async fn bump_reopens_onboarding_tos_step_until_reaccept() {
    let app = suite_app_v2().await;
    let email = unique_email("suite-bump-onboarding");
    let user_id = seed_v1_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    // The bump reopened the step: incomplete, no time — even
    // though the row still says v1/v1.
    let checklist = app.request(get_onboarding(&token)).await.json();
    assert_eq!(step(&checklist, "accept_tos")["completed"], false);
    assert!(step(&checklist, "accept_tos")["completed_at"].is_null());
    assert_eq!(checklist["all_done"], false, "{checklist}");

    // Re-accept the current versions over HTTP.
    let res = app
        .request(post_consent(
            &token,
            json!({"accept_tos_version": "v2", "accept_aup_version": "v2"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // The step closes again, and its time is the re-accept's
    // audit row, not the account's creation (the row predates
    // the bump).
    let checklist = app.request(get_onboarding(&token)).await.json();
    assert_eq!(step(&checklist, "accept_tos")["completed"], true);
    let rows = consent_audit_rows(&app, user_id).await;
    assert_eq!(rows.len(), 1, "exactly one re-accept audit row");
    assert_eq!(
        completed_at(&checklist, "accept_tos"),
        Some(rows[0].created_at)
    );
}

#[tokio::test]
async fn superseded_text_version_still_satisfies_the_gate() {
    let app = suite_app().await;
    let (user, token) = authed_user(&app, "suite-stale-version").await;
    let upload = seed_upload(&app, user).await;

    // A row captured at a superseded attestation text version
    // ("v0" — the current text is CURRENT_TEXT_VERSION) still
    // satisfies the gate: the version is recorded evidence,
    // not a gate input (the P1-094 `client` precedent). The
    // re-accept a version bump forces is the CONSENT
    // re-accept (P1-095), not a re-attestation — forcing one
    // is a future owner decision (ADR-0062).
    capture(&app.pool, user, upload, "v0", TEST_IP, Client::Web)
        .await
        .expect("capture at a superseded version");

    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
}

#[tokio::test]
async fn attestation_row_is_immutable() {
    let app = suite_app().await;
    let (user, _) = authed_user(&app, "suite-immutable").await;
    let upload = seed_upload(&app, user).await;

    // The row is written by the service — the production path
    // — not by raw SQL (the migration-004 test proves the
    // trigger on raw rows; this proves it on service rows).
    let row = capture(
        &app.pool,
        user,
        upload,
        CURRENT_TEXT_VERSION,
        TEST_IP,
        Client::Web,
    )
    .await
    .expect("capture attestation");

    // UPDATE fails on the `attestations_immutable` trigger —
    // the history a job relied on cannot be rewritten after
    // the fact. DELETE succeeds, by design: the P1-007 purge
    // migration made the trigger UPDATE-only, because DELETE
    // is the GDPR purge's erasure path (the FK cascade from a
    // user purge) — "immutable" means the record can never be
    // ALTERED, not that it can never be erased.
    let err = sqlx::query("UPDATE attestations SET text_version = 'v2' WHERE id = $1")
        .bind(row.id)
        .execute(&app.pool)
        .await;
    assert!(err.is_err(), "attestations UPDATE must fail");

    // The failed UPDATE left the row untouched — every
    // column, including the hashed IP and the timestamp.
    let after = attestations::for_upload(&app.pool, user, upload)
        .await
        .expect("re-read attestation")
        .expect("row survives the failed UPDATE");
    assert_eq!(after.id, row.id);
    assert_eq!(after.user_id, row.user_id);
    assert_eq!(after.upload_id, row.upload_id);
    assert_eq!(after.text_version, row.text_version);
    assert_eq!(after.ip_hash, row.ip_hash);
    assert_eq!(after.client, row.client);
    assert_eq!(after.created_at, row.created_at);

    // And the purge path works: a direct DELETE erases the
    // row (the migration_purge_cascade_triggers contract —
    // no app code deletes attestations directly, the only
    // production path is the FK cascade from a user purge).
    sqlx::query("DELETE FROM attestations WHERE id = $1")
        .bind(row.id)
        .execute(&app.pool)
        .await
        .expect("attestations DELETE must succeed (purge path)");
    let gone = attestations::for_upload(&app.pool, user, upload)
        .await
        .expect("re-read attestation");
    assert!(gone.is_none(), "the deleted row is gone");
}

#[tokio::test]
async fn consent_accept_audit_row_is_immutable() {
    let app = suite_app_v2().await;
    let email = unique_email("suite-audit-immutable");
    let user_id = seed_v1_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    // The re-accept writes the audit row that proves the
    // re-accept happened.
    let res = app
        .request(post_consent(
            &token,
            json!({"accept_tos_version": "v2", "accept_aup_version": "v2"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = consent_audit_rows(&app, user_id).await;
    assert_eq!(rows.len(), 1, "exactly one re-accept audit row");
    let row = &rows[0];

    // UPDATE and DELETE both fail on the audit append-only
    // trigger — the evidence of the re-accept cannot be
    // rewritten or erased after the fact.
    let err = sqlx::query("UPDATE audit_logs SET reason = 'tampered' WHERE id = $1")
        .bind(row.id)
        .execute(&app.pool)
        .await;
    assert!(err.is_err(), "audit_logs UPDATE must fail");
    let err = sqlx::query("DELETE FROM audit_logs WHERE id = $1")
        .bind(row.id)
        .execute(&app.pool)
        .await;
    assert!(err.is_err(), "audit_logs DELETE must fail");

    // The row survives verbatim.
    let after = consent_audit_rows(&app, user_id).await;
    assert_eq!(after.len(), 1, "the audit row survives");
    assert_eq!(after[0].id, row.id);
    assert_eq!(after[0].action, row.action);
    assert_eq!(after[0].target_type, row.target_type);
    assert_eq!(after[0].target_id, row.target_id);
    assert_eq!(after[0].after, row.after);
    assert_eq!(after[0].created_at, row.created_at);
}
