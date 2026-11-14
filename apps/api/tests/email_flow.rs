//! P1-030: email flow tests.
//!
//! Two halves, one suite:
//!
//! 1. Outbox assertions — for every mail the api enqueues, the
//!    outbox row must carry the right template, the right
//!    recipient (sha256 of the address — the row never holds the
//!    address itself), and a well-shaped token when the mail
//!    carries one (43-char base64url; its sha256 is the stored
//!    token row, under the right purpose). The payload never
//!    carries a raw email address (NFR-42). The token the mail
//!    carries is also the token the endpoint accepts — a
//!    well-shaped but dead token would pass the shape check
//!    alone.
//! 2. MailHog e2e — register → drain → delivered mail → the
//!    link in the delivered mail verifies the account; forgot →
//!    drain → delivered mail → the link in the delivered mail
//!    resets the password. The "MailHog" here is a fake SMTP
//!    server on loopback (the same minimal protocol subset the
//!    mailer and worker suites prove the lettre transport
//!    against): no docker on this VM (DOCKER-001), no real
//!    MailHog binary, and no real SMTP anywhere in CI — the
//!    relay is always 127.0.0.1 and the mail never leaves the
//!    test process.
//!
//! Isolation: the outbox assertions share the harness scratch
//! DB (rows are filtered per user/hash, like the other suites).
//! The e2e tests drain the outbox — `drain_once` claims every
//! due row — so each one runs on its own scratch database
//! (testkit `TestCtx`, dropped at the end), or a parallel
//! test's rows would be claimed and marked sent by the wrong
//! drain.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use common::{TestApp, TestFixtures};
use detag_api::services::login::LOCKOUT_NOTICE_TEMPLATE;
use detag_api::services::password_reset::PASSWORD_RESET_TEMPLATE;
use detag_api::services::verification::{
    EMAIL_CHANGE_ALERT_TEMPLATE, EMAIL_CHANGE_VERIFY_TEMPLATE, VERIFY_EMAIL_TEMPLATE,
};
use detag_core::{
    checksum::sha256_hex,
    config::Settings,
    repos::{email_outbox, email_verification_tokens, users},
    tokens::hash_token,
};
use detag_mailer::{
    drain_once, DrainConfig, DrainOutcome, LettreTransport, SmtpConfig, TemplateEngine,
};
use detag_testkit::TestCtx;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174102";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The password a reset flow sets.
const NEW_PASSWORD: &str = "another correct horse battery";

/// A wrong password (long enough to pass the login DTO's
/// non-empty check — the failure is the password, not the
/// shape).
const WRONG_PASSWORD: &str = "not the password at all 12345";

async fn flow_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App on its own scratch database (testkit ctx): the e2e drain
/// claims every due row, so it must not see other tests' mail.
/// Returns (app, ctx) — close the ctx when the test is done
/// (it drops the scratch database).
async fn isolated_app() -> (TestApp, TestCtx) {
    let ctx = TestCtx::new().await;
    let mut settings = Settings::default();
    settings.database_url.clone_from(&ctx.database_url);
    settings.redis_url.clone_from(&ctx.redis_url);
    settings.s3_endpoint.clone_from(&ctx.s3_endpoint);
    settings.s3_bucket.clone_from(&ctx.s3_bucket);
    settings.s3_access_key.clone_from(&ctx.s3_access_key);
    settings.s3_secret_key.clone_from(&ctx.s3_secret_key);
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    let app = TestApp::spawn_with(settings).await;
    (app, ctx)
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so tests never trip each other's rate limits (the
/// P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7().as_u128() % 250 + 1)
}

fn json_request(method: Method, uri: &str, body: Value) -> Request<axum::body::Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_register(email: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/register",
        json!({"email": email, "password": PASSWORD, "accept_tos_version": "v1"}),
    )
}

fn post_verify(token: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/verify-email",
        json!({ "token": token }),
    )
}

fn post_forgot(email: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/forgot-password",
        json!({ "email": email }),
    )
}

fn post_reset(token: &str, new_password: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/reset-password",
        json!({ "token": token, "new_password": new_password }),
    )
}

fn post_login(email: &str, password: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/login",
        json!({ "email": email, "password": password }),
    )
}

/// Login with an explicit XFF (the lockout test uses one
/// address for the whole episode, like lockout.rs).
fn post_login_with_xff(email: &str, password: &str, xff: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/login")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", xff)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&json!({ "email": email, "password": password })).unwrap(),
        ))
        .unwrap()
}

fn patch_me(token: &str, body: Value) -> Request<axum::body::Body> {
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

fn post_confirm(token: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/confirm-email-change",
        json!({ "token": token }),
    )
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let res = app.request(post_register(email)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Log in, return the access token.
async fn login_token(app: &TestApp, email: &str, password: &str) -> String {
    let res = app.request(post_login(email, password)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// A raw token is 32 random bytes, base64url without padding:
/// 43 URL-safe characters, no `+`, `/` or `=`.
fn assert_token_shape(token: &str) {
    assert_eq!(token.len(), 43, "token is 43 base64url chars: {token}");
    assert!(
        token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "token is URL-safe base64url: {token}"
    );
}

/// The outbox contract for a token-carrying mail: the template,
/// the recipient hash, the token shape, the stored token row
/// (right purpose, unused), and a fresh unsent row. Returns
/// the raw token.
async fn assert_token_mail(
    app: &TestApp,
    template: &str,
    email: &str,
    user_id: Uuid,
    purpose: &str,
) -> String {
    let mails = email_outbox::unsent_for_template(&app.state.pool, template)
        .await
        .expect("outbox read");
    let hash = sha256_hex(email.as_bytes());
    let ours: Vec<_> = mails
        .iter()
        .filter(|m| m.to_hash.as_deref() == Some(hash.as_str()))
        .collect();
    assert_eq!(
        ours.len(),
        1,
        "exactly one {template} mail for this address, got {}",
        ours.len()
    );
    let mail = ours[0];
    assert_eq!(mail.payload["user_id"], json!(user_id));
    assert!(
        !mail.payload.to_string().contains('@'),
        "payload carries ids only, never an address: {}",
        mail.payload
    );
    let token = mail.payload["token"]
        .as_str()
        .expect("payload carries the raw token")
        .to_string();
    assert_token_shape(&token);
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&token))
        .await
        .expect("token lookup")
        .expect("the token row exists");
    assert_eq!(row.user_id, user_id);
    assert_eq!(row.purpose, purpose);
    assert!(row.used_at.is_none(), "the token is unused");
    assert!(mail.sent_at.is_none(), "the mail is unsent");
    assert_eq!(mail.attempts, 0, "the mail is unattempted");
    assert!(mail.scheduled_at <= Utc::now(), "the mail is due now");
    token
}

/// The outbox contract for a notification mail (no token):
/// template, recipient hash, payload `{user_id}` only, fresh
/// unsent row. Returns the payload for extra checks.
async fn assert_notice_mail(app: &TestApp, template: &str, email: &str, user_id: Uuid) -> Value {
    let mails = email_outbox::unsent_for_template(&app.state.pool, template)
        .await
        .expect("outbox read");
    let hash = sha256_hex(email.as_bytes());
    let ours: Vec<_> = mails
        .iter()
        .filter(|m| m.to_hash.as_deref() == Some(hash.as_str()))
        .collect();
    assert_eq!(
        ours.len(),
        1,
        "exactly one {template} mail for this address, got {}",
        ours.len()
    );
    let mail = ours[0];
    assert_eq!(
        mail.payload,
        json!({ "user_id": user_id }),
        "a notice carries only the user id"
    );
    assert!(mail.sent_at.is_none(), "the mail is unsent");
    assert_eq!(mail.attempts, 0, "the mail is unattempted");
    mail.payload.clone()
}

/// A fake SMTP relay on loopback: the MailHog stand-in for
/// the e2e tests. Accepts connections until stopped; every
/// DATA payload is captured in arrival order.
struct FakeSmtp {
    port: u16,
    captured: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeSmtp {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake smtp binds");
        let port = listener.local_addr().expect("fake smtp port").port();
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn(run_fake_smtp(listener, Arc::clone(&captured)));
        Self {
            port,
            captured,
            task,
        }
    }

    /// Captured DATA payloads so far, in arrival order.
    fn mails(&self) -> Vec<String> {
        self.captured.lock().expect("capture lock").clone()
    }

    /// Stop accepting (abort the accept loop).
    fn stop(&self) {
        self.task.abort();
    }
}

async fn run_fake_smtp(listener: TcpListener, captured: Arc<Mutex<Vec<String>>>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(handle_smtp_connection(stream, Arc::clone(&captured)));
    }
}

/// One SMTP conversation: greeting, EHLO, MAIL FROM, RCPT TO,
/// DATA (captured), QUIT — the same minimal subset the mailer
/// and worker suites prove the lettre transport against.
async fn handle_smtp_connection(stream: TcpStream, captured: Arc<Mutex<Vec<String>>>) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    writer
        .write_all(b"220 localhost fake-smtp\r\n")
        .await
        .expect("fake smtp greeting");
    while let Some(line) = lines.next_line().await.expect("fake smtp read") {
        let upper = line.to_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            writer
                .write_all(b"250-localhost\r\n250 SMTPUTF8\r\n")
                .await
                .expect("fake smtp EHLO");
        } else if upper.starts_with("MAIL FROM") {
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp MAIL");
        } else if upper.starts_with("RCPT TO") {
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp RCPT");
        } else if upper.starts_with("DATA") {
            writer
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await
                .expect("fake smtp DATA");
            let mut data = String::new();
            while let Some(data_line) = lines.next_line().await.expect("fake smtp data") {
                if data_line == "." {
                    break;
                }
                data.push_str(&data_line);
                data.push('\n');
            }
            captured.lock().expect("capture lock").push(data);
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp queued");
        } else if upper.starts_with("QUIT") {
            writer
                .write_all(b"221 Bye\r\n")
                .await
                .expect("fake smtp QUIT");
            break;
        } else if upper.starts_with("RSET") {
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp RSET");
        } else {
            writer
                .write_all(b"500 unrecognized\r\n")
                .await
                .expect("fake smtp unknown");
        }
    }
}

/// One drain pass against the fake relay, with the real
/// lettre transport: the closest this VM gets to MailHog.
async fn drain_to_fake_smtp(app: &TestApp, smtp: &FakeSmtp) -> DrainOutcome {
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = LettreTransport::new(&SmtpConfig {
        host: "127.0.0.1".to_string(),
        port: smtp.port,
        user: String::new(),
        pass: String::new(),
        from: app.settings.mail_from.clone(),
        tls: false,
    })
    .expect("plaintext transport builds");
    let config = DrainConfig::from_settings(&app.settings);
    drain_once(&app.state.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("drain pass runs")
}

/// Decode a quoted-printable body: `=XX` hex escapes become
/// the byte (`=3D` is `=`, `=E2=80=94` the em-dash), and a
/// soft break (`=` at end of line) is removed. The captured
/// DATA is ASCII plus UTF-8 escapes, so decoding back to a
/// String is safe. Needed because lettre sends both parts
/// quoted-printable — the `=` in the link's query string
/// arrives as `=3D`, which would shift any naive token read
/// by two characters.
fn qp_decode(data: &str) -> String {
    let bytes = data.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 2; // soft break: the line continues
            } else if i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("decoded mail is UTF-8")
}

/// The token carried in a delivered mail's action link: the
/// link ends in `token=` plus 43 base64url characters. The
/// mail is quoted-printable-decoded first (see [`qp_decode`]).
fn token_from_delivered_mail(data: &str) -> String {
    let start = data
        .find("token=")
        .expect("the delivered mail carries a token link");
    let token: String = data[start + "token=".len()..].chars().take(43).collect();
    assert_token_shape(&token);
    token
}

/// The delivered mail's envelope: recipient, sender, subject,
/// multipart text + HTML — the same headers MailHog would
/// show for the message. `token` is `Some` for the mails that
/// carry an action link (verify / reset); notification mails
/// carry none.
fn assert_delivered_mail(data: &str, to: &str, subject: &str, token: Option<&str>) {
    assert!(
        data.contains(&format!("To: {to}")),
        "DATA carries the recipient:\n{data}"
    );
    assert!(
        data.contains("From: noreply@detag.local"),
        "DATA carries the sender:\n{data}"
    );
    assert!(
        data.contains(&format!("Subject: {subject}")),
        "DATA carries the subject:\n{data}"
    );
    assert!(
        data.contains("multipart/alternative"),
        "DATA is multipart (text + html):\n{data}"
    );
    if let Some(token) = token {
        assert!(data.contains(token), "DATA carries the token:\n{data}");
    }
}

#[tokio::test]
async fn register_enqueues_verify_mail_with_the_outbox_contract() {
    let app = flow_app().await;
    let email = unique_email("flow-verify");
    let user_id = register_user(&app, &email).await;
    let token = assert_token_mail(
        &app,
        VERIFY_EMAIL_TEMPLATE,
        &email,
        user_id,
        email_verification_tokens::PURPOSE_VERIFICATION,
    )
    .await;
    // The token the mail carries is the one the endpoint
    // accepts — a well-shaped but dead token would pass the
    // shape check alone.
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
}

#[tokio::test]
async fn forgot_enqueues_reset_mail_with_the_outbox_contract() {
    let app = flow_app().await;
    let email = unique_email("flow-reset");
    let user_id = register_user(&app, &email).await;
    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = assert_token_mail(
        &app,
        PASSWORD_RESET_TEMPLATE,
        &email,
        user_id,
        email_verification_tokens::PURPOSE_PASSWORD_RESET,
    )
    .await;
    // The token the mail carries resets the password — and the
    // new password logs in.
    let res = app.request(post_reset(&token, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let res = app.request(post_login(&email, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn email_change_enqueues_verify_to_the_new_address_and_alert_to_the_old() {
    let app = flow_app().await;
    let email = unique_email("flow-change");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email, PASSWORD).await;
    let new_email = unique_email("flow-change-new");
    let res = app
        .request(patch_me(&token, json!({ "email": new_email })))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // The verification mail goes to the NEW address and carries
    // a live change token...
    let raw = assert_token_mail(
        &app,
        EMAIL_CHANGE_VERIFY_TEMPLATE,
        &new_email,
        user_id,
        email_verification_tokens::PURPOSE_EMAIL_CHANGE,
    )
    .await;
    // ...and the takeover alert goes to the OLD address,
    // carrying no token and never naming the new address.
    let alert = assert_notice_mail(&app, EMAIL_CHANGE_ALERT_TEMPLATE, &email, user_id).await;
    assert!(
        !alert.to_string().contains(&new_email),
        "the alert never names the new address: {alert}"
    );
    // The token the mail carries confirms the change.
    let res = app.request(post_confirm(&raw)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
}

#[tokio::test]
async fn lockout_enqueues_one_notice_with_no_token() {
    let app = flow_app().await;
    let email = unique_email("flow-lockout");
    let user_id = register_user(&app, &email).await;
    let xff = unique_xff();
    for _ in 0..5 {
        let res = app
            .request(post_login_with_xff(&email, WRONG_PASSWORD, &xff))
            .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    }
    // One notice, addressed to the account, carrying no token.
    assert_notice_mail(&app, LOCKOUT_NOTICE_TEMPLATE, &email, user_id).await;
    // A further attempt while locked (423) queues no second mail.
    let res = app
        .request(post_login_with_xff(&email, PASSWORD, &xff))
        .await;
    assert_eq!(res.status, StatusCode::LOCKED);
    let mails = email_outbox::unsent_for_template(&app.state.pool, LOCKOUT_NOTICE_TEMPLATE)
        .await
        .expect("outbox read");
    let hash = sha256_hex(email.as_bytes());
    assert_eq!(
        mails
            .iter()
            .filter(|m| m.to_hash.as_deref() == Some(hash.as_str()))
            .count(),
        1,
        "still one notice after a locked attempt"
    );
}

#[tokio::test]
async fn verify_flow_end_to_end_over_fake_smtp() {
    let (app, ctx) = isolated_app().await;
    let email = unique_email("flow-e2e-verify");
    let user_id = register_user(&app, &email).await;

    let smtp = FakeSmtp::start().await;
    let outcome = drain_to_fake_smtp(&app, &smtp).await;
    assert_eq!(outcome.claimed, 1, "the drain claims the verify mail");
    assert_eq!(outcome.sent, 1, "the drain sends it");
    assert_eq!(outcome.transient_failed, 0);
    assert_eq!(outcome.permanent_failed, 0);
    assert_eq!(smtp.mails().len(), 1, "one mail reached the relay");
    let flat = qp_decode(&smtp.mails()[0]);
    let token = token_from_delivered_mail(&flat);
    assert_delivered_mail(&flat, &email, "Verify your Detag email", Some(&token));
    // The token in the DELIVERED mail — not the outbox — is
    // what verifies the account.
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .unwrap()
        .expect("user row exists");
    assert!(user.email_verified_at.is_some(), "verified_at is set");
    // The row is marked sent: a second pass sends nothing.
    let outcome = drain_to_fake_smtp(&app, &smtp).await;
    assert_eq!(outcome.claimed, 0, "nothing left to claim");
    assert_eq!(outcome.sent, 0);
    assert_eq!(smtp.mails().len(), 1, "no duplicate delivery");
    smtp.stop();
    ctx.close().await;
}

#[tokio::test]
async fn reset_flow_end_to_end_over_fake_smtp() {
    let (app, ctx) = isolated_app().await;
    let email = unique_email("flow-e2e-reset");
    register_user(&app, &email).await;
    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    let smtp = FakeSmtp::start().await;
    let outcome = drain_to_fake_smtp(&app, &smtp).await;
    assert_eq!(
        outcome.claimed, 2,
        "the drain claims both the verify and the reset mail"
    );
    assert_eq!(outcome.sent, 2, "the drain sends both");
    assert_eq!(smtp.mails().len(), 2, "both mails reached the relay");
    let reset = smtp
        .mails()
        .into_iter()
        .find(|m| m.contains("Subject: Reset your Detag password"))
        .expect("the reset mail was delivered");
    let flat = qp_decode(&reset);
    let token = token_from_delivered_mail(&flat);
    assert_delivered_mail(&flat, &email, "Reset your Detag password", Some(&token));
    // The token in the DELIVERED mail — not the outbox — is
    // what resets the password.
    let res = app.request(post_reset(&token, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let res = app.request(post_login(&email, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // Both rows are marked sent — but the reset queued a
    // password-changed digest, so the second pass delivers
    // that one (P1-025), and only the third pass is empty.
    let outcome = drain_to_fake_smtp(&app, &smtp).await;
    assert_eq!(outcome.claimed, 1, "the digest mail is due");
    assert_eq!(outcome.sent, 1);
    assert_eq!(smtp.mails().len(), 3, "the digest was delivered too");
    let digest = smtp
        .mails()
        .into_iter()
        .find(|m| m.contains("Subject: Your Detag password was changed"))
        .expect("the digest mail was delivered");
    let digest = qp_decode(&digest);
    assert_delivered_mail(&digest, &email, "Your Detag password was changed", None);
    let outcome = drain_to_fake_smtp(&app, &smtp).await;
    assert_eq!(outcome.claimed, 0, "nothing left to claim");
    assert_eq!(outcome.sent, 0);
    assert_eq!(smtp.mails().len(), 3, "no duplicate delivery");
    smtp.stop();
    ctx.close().await;
}
