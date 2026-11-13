//! P1-006: the email-change flow, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: the PATCH trigger (change token
//! minted + verification mail to the new address + takeover
//! alert to the old address, in the same transaction as the
//! profile update), the confirm swap (204, email swapped,
//! pending cleared, new address verified, `email.change`
//! audited), and the failure modes (unknown / used / expired /
//! wrong-purpose token → 401 PF-1010/1011; address taken
//! meanwhile → 409 PF-2011; cleared intent → 401; resend
//! killing the live change token — the one-live-token rule).
//! Tokens are read back from the outbox payload, exactly as
//! the mail drain would read them.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::verification::{
    EMAIL_CHANGE_ALERT_TEMPLATE, EMAIL_CHANGE_VERIFY_TEMPLATE, VERIFY_EMAIL_TEMPLATE,
};
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, email_outbox, email_verification_tokens, users},
    tokens::{generate_token, hash_token},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn email_change_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email() -> String {
    format!("change-{}@example.com", Uuid::now_v7())
}

/// A per-test XFF address: each test gets its own IP bucket,
/// so the tests never trip each other's rate limits (the
/// P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7().as_u128() % 250 + 1)
}

fn json_request(method: Method, uri: &str, body: serde_json::Value) -> Request<axum::body::Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let res = app
        .request(json_request(
            Method::POST,
            "/v1/auth/register",
            json!({"email": email, "password": PASSWORD, "accept_tos_version": "v1"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Log in, return the access token.
async fn login_token(app: &TestApp, email: &str) -> String {
    let res = app
        .request(json_request(
            Method::POST,
            "/v1/auth/login",
            json!({"email": email, "password": PASSWORD}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

fn patch_me(token: &str, body: serde_json::Value) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::PATCH)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get_me(token: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

fn post_confirm(raw_token: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/confirm-email-change",
        json!({"token": raw_token}),
    )
}

/// The change token for `user_id`, read from the LATEST
/// `email-change-verify` outbox mail (the mail drain reads it
/// the same way). The outbox accumulates one mail per mint —
/// like the resend flow, an old mail stays queued with a dead
/// token when a newer mint supersedes it — so the latest mail
/// is the live one (the verification tests use `.last()` the
/// same way).
async fn change_token_for(app: &TestApp, user_id: Uuid) -> String {
    let mails = email_outbox::unsent_for_template(&app.state.pool, EMAIL_CHANGE_VERIFY_TEMPLATE)
        .await
        .expect("list outbox");
    let ours: Vec<_> = mails
        .iter()
        .filter(|m| m.payload["user_id"] == json!(user_id))
        .collect();
    assert!(!ours.is_empty(), "a change-verify mail exists");
    ours.last().unwrap().payload["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Count the `email-change-verify` mails for a user.
async fn count_change_mails(app: &TestApp, user_id: Uuid) -> usize {
    let mails = email_outbox::unsent_for_template(&app.state.pool, EMAIL_CHANGE_VERIFY_TEMPLATE)
        .await
        .expect("list outbox");
    mails
        .iter()
        .filter(|m| m.payload["user_id"] == json!(user_id))
        .count()
}

/// Count the takeover alerts addressed (by hash) to `email`.
async fn count_alerts_for(app: &TestApp, email: &str) -> usize {
    let mails = email_outbox::unsent_for_template(&app.state.pool, EMAIL_CHANGE_ALERT_TEMPLATE)
        .await
        .expect("list outbox");
    let hash = sha256_hex(email.as_bytes());
    mails
        .iter()
        .filter(|m| m.to_hash.as_deref() == Some(hash.as_str()))
        .count()
}

/// Register + log in, return (user id, token, email).
async fn registered_user(app: &TestApp) -> (Uuid, String, String) {
    let email = unique_email();
    let user_id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    (user_id, token, email)
}

#[tokio::test]
async fn patch_sets_pending_mints_change_token_and_queues_both_mails() {
    let app = email_change_app().await;
    let (user_id, token, email) = registered_user(&app).await;
    let new_email = "new-address@example.com";

    let res = app
        .request(patch_me(&token, json!({"email": new_email})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["pending_email"], new_email);

    // The token row: purpose email_change, unused, ~24h expiry.
    let raw = change_token_for(&app, user_id).await;
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .expect("token lookup")
        .expect("change token row exists");
    assert_eq!(row.purpose, "email_change");
    assert_eq!(row.user_id, user_id);
    assert!(row.used_at.is_none());
    let ttl = row.expires_at - chrono::Utc::now();
    assert!(
        ttl.num_seconds() > 23 * 3600 && ttl.num_seconds() <= 24 * 3600,
        "24h TTL, got {ttl:?}"
    );

    // The verification mail goes to the NEW address and carries
    // the token; the alert goes to the OLD address and carries
    // no token (and no new address anywhere in its payload).
    let mails = email_outbox::unsent_for_template(&app.state.pool, EMAIL_CHANGE_VERIFY_TEMPLATE)
        .await
        .expect("list outbox");
    let verify = mails
        .iter()
        .find(|m| m.payload["user_id"] == json!(user_id))
        .expect("verify mail exists");
    let new_hash = sha256_hex(new_email.as_bytes());
    assert_eq!(verify.to_hash.as_deref(), Some(new_hash.as_str()));
    assert_eq!(verify.payload["token"].as_str(), Some(raw.as_str()));

    let alerts = email_outbox::unsent_for_template(&app.state.pool, EMAIL_CHANGE_ALERT_TEMPLATE)
        .await
        .expect("list outbox");
    let old_hash = sha256_hex(email.as_bytes());
    let alert = alerts
        .iter()
        .find(|m| m.to_hash.as_deref() == Some(old_hash.as_str()))
        .expect("alert mail exists");
    assert_eq!(alert.payload["user_id"], json!(user_id));
    assert!(
        alert.payload.get("token").is_none(),
        "alert carries no token"
    );
    assert!(
        !alert.payload.to_string().contains(new_email),
        "alert never names the new address"
    );
}

#[tokio::test]
async fn confirm_happy_path_swaps_email_and_verifies() {
    let app = email_change_app().await;
    let (user_id, token, email) = registered_user(&app).await;
    // Unique per run: the harness DB persists across runs, and
    // the swap enforces email uniqueness — a fixed address
    // would collide with a previous run's swapped user.
    let new_email = unique_email();
    let res = app
        .request(patch_me(&token, json!({"email": new_email})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let raw = change_token_for(&app, user_id).await;
    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // The profile: new email, pending cleared, verified.
    let res = app.request(get_me(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert_eq!(json["email"], new_email);
    assert!(json.get("pending_email").is_none(), "{json}");
    assert_eq!(json["email_verified"], true);

    // The token is consumed; the audit carries hashes only.
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .expect("token lookup")
        .expect("token row exists");
    assert!(row.used_at.is_some());
    let rows = audit_logs::list_for_action(&app.state.pool, "email.change")
        .await
        .expect("list audits");
    let row = rows
        .iter()
        .find(|r| r.target_id.as_deref() == Some(&user_id.to_string()))
        .expect("email.change audit exists");
    assert_eq!(row.actor_user_id, Some(user_id));
    assert_eq!(
        row.before.as_ref().unwrap()["email_hash"],
        sha256_hex(email.as_bytes())
    );
    assert_eq!(
        row.after.as_ref().unwrap()["email_hash"],
        sha256_hex(new_email.as_bytes())
    );
}

#[tokio::test]
async fn confirm_unknown_token_is_401() {
    let app = email_change_app().await;
    let res = app.request(post_confirm(&generate_token())).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    // Unknown token is PF-1010 (InvalidToken), the same code
    // verify-email uses — PF-1001 is "no credential at all".
    assert_eq!(res.json()["code"], "PF-1010");
}

#[tokio::test]
async fn confirm_used_token_is_401() {
    let app = email_change_app().await;
    let (user_id, token, _) = registered_user(&app).await;
    // Unique per run (see confirm_happy_path — the harness DB
    // persists across runs and the swap enforces uniqueness).
    let pending = unique_email();
    let res = app
        .request(patch_me(&token, json!({"email": pending})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let raw = change_token_for(&app, user_id).await;

    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010");
}

#[tokio::test]
async fn confirm_expired_token_is_401_and_not_consumed() {
    let app = email_change_app().await;
    let (user_id, _token, _) = registered_user(&app).await;
    // Mint a change token that is already expired (the repo
    // takes expires_at as a parameter — no clock to freeze).
    let raw = generate_token();
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    email_verification_tokens::mint(
        &app.state.pool,
        user_id,
        &hash_token(&raw),
        email_verification_tokens::PURPOSE_EMAIL_CHANGE,
        past,
    )
    .await
    .unwrap();

    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1011");

    // Expired is not consumed: the row stays unused.
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .expect("token lookup")
        .expect("token row exists");
    assert!(row.used_at.is_none());
}

#[tokio::test]
async fn confirm_registration_token_is_401_wrong_purpose() {
    let app = email_change_app().await;
    let (user_id, _token, _) = registered_user(&app).await;
    // The registration token (purpose verification) from the
    // verify-email mail — presented at confirm, it must be
    // invalid (wrong purpose), not a verify.
    let mails = email_outbox::unsent_for_template(&app.state.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .expect("list outbox");
    let ours: Vec<_> = mails
        .iter()
        .filter(|m| m.payload["user_id"] == json!(user_id))
        .collect();
    assert_eq!(ours.len(), 1, "one registration mail: {ours:?}");
    let raw = ours[0].payload["token"].as_str().unwrap().to_string();

    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010");

    // Wrong purpose is not consumed: the registration token
    // still works for verify-email afterwards.
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .expect("token lookup")
        .expect("token row exists");
    assert!(row.used_at.is_none());
    assert_eq!(row.purpose, "verification");
}

#[tokio::test]
async fn confirm_after_clearing_pending_is_401_and_token_deleted() {
    let app = email_change_app().await;
    let (user_id, token, _) = registered_user(&app).await;
    let res = app
        .request(patch_me(&token, json!({"email": "clear-me@example.com"})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let raw = change_token_for(&app, user_id).await;

    // Clear the intent: the live change token dies with it.
    // (The queued mail stays in the outbox — like a superseded
    // resend mail, it carries a dead token and its link 401s.
    // Only the token row is the live credential.)
    let res = app.request(patch_me(&token, json!({"email": null}))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .expect("token lookup");
    assert!(row.is_none(), "clearing deletes the change token");

    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010");
}

#[tokio::test]
async fn confirm_when_address_taken_is_409_and_token_survives() {
    let app = email_change_app().await;
    let (user_a, token_a, email_a) = registered_user(&app).await;
    let taken = unique_email();
    // A intends to move to `taken`; B registers it meanwhile.
    let res = app
        .request(patch_me(&token_a, json!({"email": taken})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    register_user(&app, &taken).await;

    let raw = change_token_for(&app, user_a).await;
    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2011");

    // The swap rolled back: A's email unchanged, the intent
    // still pending, the token still unused (retryable).
    let user = users::by_id(&app.pool, user_a).await.unwrap().unwrap();
    assert_eq!(user.email, email_a);
    assert_eq!(user.pending_email.as_deref(), Some(taken.as_str()));
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .expect("token lookup")
        .expect("token row exists");
    assert!(row.used_at.is_none());
}

#[tokio::test]
async fn patch_display_name_only_queues_no_change_mail() {
    let app = email_change_app().await;
    let (user_id, token, email) = registered_user(&app).await;

    let res = app
        .request(patch_me(&token, json!({"display_name": "Deen"})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(count_change_mails(&app, user_id).await, 0);
    assert_eq!(count_alerts_for(&app, &email).await, 0);
}

#[tokio::test]
async fn patch_same_pending_twice_mints_once() {
    let app = email_change_app().await;
    let (user_id, token, email) = registered_user(&app).await;
    let new_email = "same-twice@example.com";

    for _ in 0..2 {
        let res = app
            .request(patch_me(&token, json!({"email": new_email})))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    }
    // The second PATCH is a no-op (pending unchanged) — no
    // second token, no second alert. Re-trigger by clearing +
    // re-setting instead (documented in the api-spec).
    assert_eq!(count_change_mails(&app, user_id).await, 1);
    assert_eq!(count_alerts_for(&app, &email).await, 1);
}

#[tokio::test]
async fn patch_replacing_pending_mints_fresh_token() {
    let app = email_change_app().await;
    let (user_id, token, email) = registered_user(&app).await;

    let res = app
        .request(patch_me(&token, json!({"email": "first@example.com"})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let first_raw = change_token_for(&app, user_id).await;

    let res = app
        .request(patch_me(&token, json!({"email": "second@example.com"})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // One live token: the first is deleted, the second is the
    // only live one (two mails are queued — one per mint, like
    // resends — but only the latest token's row exists); the
    // alert still goes to the current email (not to the first
    // pending address).
    let first = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&first_raw))
        .await
        .expect("token lookup");
    assert!(first.is_none(), "replacing deletes the old change token");
    assert_eq!(count_change_mails(&app, user_id).await, 2);
    let second_raw = change_token_for(&app, user_id).await;
    assert_ne!(second_raw, first_raw);
    assert_eq!(count_alerts_for(&app, &email).await, 2);
}

#[tokio::test]
async fn resend_verification_kills_live_change_token() {
    let app = email_change_app().await;
    let (user_id, token, email) = registered_user(&app).await;
    let res = app
        .request(patch_me(
            &token,
            json!({"email": "resend-kill@example.com"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let raw = change_token_for(&app, user_id).await;

    // A resend mints a verification token — one live token per
    // user — so the change token dies. The pending intent
    // survives on the user row (re-trigger by clearing +
    // re-setting).
    let res = app
        .request(json_request(
            Method::POST,
            "/v1/auth/resend-verification",
            json!({"email": email}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .expect("token lookup");
    assert!(row.is_none(), "resend deletes the change token");

    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010");

    let user = users::by_id(&app.pool, user_id).await.unwrap().unwrap();
    assert_eq!(
        user.pending_email.as_deref(),
        Some("resend-kill@example.com")
    );
}
