//! P1-089: the abuse intake — `POST /v1/abuse`
//! (public, rate-limited, honeypot field, FR-C-06).
//!
//! The harness DB is shared and persists across runs, so
//! every test uses a UUID-suffixed reporter contact and
//! asserts on rows it can name exactly (its own report id
//! or contact) — never on a global count.
//!
//! - `abuse_intake_files_report` — a valid filing is 201
//!   with the ticket shape; the row lands (`user_id` NULL,
//!   the contact + reason as filed), exactly one
//!   `abuse.report_filed` audit row (its `after` carries the
//!   contact's sha256, never the address), and exactly one
//!   `abuse-report-ack` outbox row whose payload is
//!   `{report_id}` — and that payload renders (the ack the
//!   reporter actually receives names the report id).
//! - `abuse_intake_honeypot_drops_bots` — the AC: the same
//!   valid body with the honeypot field filled is still 201
//!   with the ticket shape, but no row, no audit, and no
//!   mail exist for it — and the returned id names no row
//!   at all.
//! - `abuse_intake_validation` — a bad contact email, an
//!   empty reason, and a 501-char reason are each 422
//!   `PF-2002` with `field_errors`; a filing with no job id
//!   is a general-abuse report (201); a filing naming a job
//!   id that does not exist is 404 `PF-2010` (the FK would
//!   reject the insert — a 500 for a wrong id would be a lie).
//! - `abuse_intake_rate_limited` — with the bucket at burst
//!   2 / refill 0, the third filing from one IP is 429
//!   `PF-3007` with `Retry-After` and the bucket's
//!   `X-RateLimit-*` trio (Limit "2").

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::checksum::sha256_hex;
use detag_core::clock::{Clock, ManualClock};
use detag_core::repos::{
    abuse_reports, audit_logs, email_outbox,
    jobs::{self, NewJob},
};
use detag_mailer::TemplateEngine;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The frozen "now": Thursday 2026-11-19, 12:00 UTC.
fn frozen_now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(2026, 11, 19, 12, 0, 0)
        .unwrap()
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// A reporter contact that no other test (or run) uses.
fn unique_contact(tag: &str) -> String {
    format!("{tag}-{}@example.com", Uuid::now_v7())
}

/// App on the frozen clock. No billing provider: the
/// endpoint never calls one.
async fn abuse_intake_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let settings = fixtures.settings();
    let clock = Arc::new(ManualClock::new(frozen_now()));
    TestApp::spawn_with_fixtures_settings_and_clock(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .await
}

/// App with the intake bucket at burst 2 / refill 0 (the
/// rate-limit test's small bucket).
async fn rate_limit_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.rate_limit_abuse_intake_burst = 2;
    settings.rate_limit_abuse_intake_refill_per_min = 0;
    let clock = Arc::new(ManualClock::new(frozen_now()));
    TestApp::spawn_with_fixtures_settings_and_clock(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .await
}

/// POST /v1/abuse with the given JSON body and XFF address.
async fn post_abuse(app: &TestApp, body: serde_json::Value, xff: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/abuse")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", xff)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// A valid filing body for the given contact. No job id —
/// the rate-limit test files general reports; the
/// job-naming tests seed a real job instead (a random id
/// would 404 — the FK rejects unknown jobs).
fn filing_body(contact: &str) -> serde_json::Value {
    json!({
        "reporter_contact": contact,
        "reason": "Stolen artwork, please review.",
    })
}

/// Seed one queued image job for a fresh user; return its id.
/// A job's `upload_id` is nullable, so no upload is needed.
async fn seed_job(app: &TestApp) -> Uuid {
    let user = app.seed_user_unique().await;
    jobs::create(
        &app.pool,
        &NewJob {
            user_id: user.id,
            upload_id: None,
            kind: "image",
            mode: "inpaint",
            status: "queued",
            lane: "standard",
            params: json!({}),
            idempotency_key: Some(&format!("p089-{}", Uuid::now_v7())),
            key_id: None,
        },
    )
    .await
    .expect("seed job")
    .id
}

#[tokio::test]
async fn abuse_intake_files_report() {
    let app = abuse_intake_app().await;
    let contact = unique_contact("filed");
    let job_id = seed_job(&app).await;
    let body = json!({
        "job_id": job_id,
        "reporter_contact": contact,
        "reason": "Stolen artwork, please review.",
    });
    let res = post_abuse(&app, body, &unique_xff()).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let ticket = res.json();
    let report_id: Uuid = ticket["id"]
        .as_str()
        .expect("ticket carries an id")
        .parse()
        .expect("ticket id is a UUID");
    assert_eq!(ticket["status"], "open");
    assert!(ticket["created_at"].is_string(), "ticket: {ticket}");

    // The row: the job, the contact, and the reason as filed;
    // `user_id` NULL — the reporter has no account.
    let row = abuse_reports::by_id(&app.pool, report_id)
        .await
        .expect("read back")
        .expect("the filed report exists");
    assert_eq!(row.job_id, Some(job_id));
    assert_eq!(row.reporter_contact.as_deref(), Some(contact.as_str()));
    assert_eq!(row.reason, "Stolen artwork, please review.");
    assert_eq!(row.user_id, None);
    assert_eq!(row.status, "open");

    // Exactly one audit row for this report: the action, the
    // target, and the contact's hash — never the address.
    let audits = audit_logs::list_for_action(&app.pool, "abuse.report_filed")
        .await
        .expect("read audit rows");
    let ours: Vec<_> = audits
        .iter()
        .filter(|row| row.target_id.as_deref() == Some(report_id.to_string().as_str()))
        .collect();
    assert_eq!(ours.len(), 1, "exactly one audit row for the report");
    assert_eq!(ours[0].target_type, "abuse_report");
    assert_eq!(ours[0].actor_client.as_deref(), Some("abuse-intake"));
    let after = ours[0].after.as_ref().expect("audit after exists");
    assert_eq!(
        after["email_hash"],
        sha256_hex(contact.as_bytes()),
        "audit carries the hash, not the address"
    );
    assert!(
        !after.to_string().contains(&contact),
        "no raw contact in the audit row: {after}"
    );

    // Exactly one ack mail, queued for this report: the
    // payload is `{report_id}` only, `to_hash` the contact's
    // sha256.
    let mails = email_outbox::unsent_for_template(&app.pool, "abuse-report-ack")
        .await
        .expect("read outbox rows");
    let ours: Vec<_> = mails
        .iter()
        .filter(|mail| mail.payload["report_id"].as_str() == Some(report_id.to_string().as_str()))
        .collect();
    assert_eq!(ours.len(), 1, "exactly one ack mail for the report");
    assert_eq!(
        ours[0].to_hash.as_deref(),
        Some(sha256_hex(contact.as_bytes()).as_str())
    );
    assert_eq!(
        ours[0].payload,
        json!({ "report_id": report_id }),
        "the ack payload is ids only"
    );

    // And that payload renders: the ack the reporter actually
    // receives names the report id (the P1-085 render-test
    // precedent — a payload missing a variable its template
    // needs must fail loudly here, not in production).
    let engine = TemplateEngine::new().expect("templates compile");
    let rendered = engine
        .render(
            "abuse-report-ack",
            &json!({
                "report_id": report_id,
                "base_url": "http://localhost:8080",
                "app_name": "Detag",
            }),
        )
        .expect("the enqueued ack renders");
    assert!(
        rendered.text_body.contains(&report_id.to_string()),
        "the ack names the report:\\n{}",
        rendered.text_body
    );
    assert!(
        rendered.text_body.contains("72 hours"),
        "the ack states the review target:\\n{}",
        rendered.text_body
    );
}

#[tokio::test]
async fn abuse_intake_honeypot_drops_bots() {
    let app = abuse_intake_app().await;
    let contact = unique_contact("honeypot");
    let body = json!({
        "job_id": Uuid::now_v7(),
        "reporter_contact": contact,
        "reason": "Stolen artwork, please review.",
        "website": "https://spam.example",
    });
    let res = post_abuse(&app, body, &unique_xff()).await;
    // Indistinguishable from a filing: the same 201 shape.
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let ticket = res.json();
    assert_eq!(ticket["status"], "open");
    let returned_id: Uuid = ticket["id"]
        .as_str()
        .expect("ticket carries an id")
        .parse()
        .expect("ticket id is a UUID");

    // But nothing was written: the returned id names no row,
    // no row carries this contact, no audit row, no mail.
    assert!(
        abuse_reports::by_id(&app.pool, returned_id)
            .await
            .expect("read back")
            .is_none(),
        "the honeypot id must name no row"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM abuse_reports WHERE reporter_contact = $1")
            .bind(&contact)
            .fetch_one(&app.pool)
            .await
            .expect("count rows for the contact");
    assert_eq!(count, 0, "the honeypot filing wrote no row");
    let audits = audit_logs::list_for_action(&app.pool, "abuse.report_filed")
        .await
        .expect("read audit rows");
    assert!(
        audits
            .iter()
            .all(|row| row.target_id.as_deref() != Some(returned_id.to_string().as_str())),
        "the honeypot filing wrote no audit row"
    );
    let mails = email_outbox::unsent_for_template(&app.pool, "abuse-report-ack")
        .await
        .expect("read outbox rows");
    assert!(
        mails.iter().all(
            |mail| mail.payload["report_id"].as_str() != Some(returned_id.to_string().as_str())
        ),
        "the honeypot filing queued no mail"
    );
}

#[tokio::test]
async fn abuse_intake_validation() {
    let app = abuse_intake_app().await;

    // A bad contact email is 422 with field_errors.
    let res = post_abuse(
        &app,
        json!({
            "reporter_contact": "not-an-email",
            "reason": "Spam.",
        }),
        &unique_xff(),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let body = res.json();
    assert_eq!(body["code"], "PF-2002");
    assert_eq!(body["field_errors"][0]["field"], "reporter_contact");

    // An empty reason is 422.
    let res = post_abuse(
        &app,
        json!({
            "reporter_contact": "reporter@example.com",
            "reason": "   ",
        }),
        &unique_xff(),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["field_errors"][0]["field"], "reason");

    // A 501-char reason is 422.
    let res = post_abuse(
        &app,
        json!({
            "reporter_contact": "reporter@example.com",
            "reason": "a".repeat(501),
        }),
        &unique_xff(),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["field_errors"][0]["field"], "reason");

    // No job id: a general-abuse report, still filed.
    let contact = unique_contact("no-job");
    let res = post_abuse(
        &app,
        json!({
            "reporter_contact": contact,
            "reason": "Abusive behaviour on the platform.",
        }),
        &unique_xff(),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let report_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let row = abuse_reports::by_id(&app.pool, report_id)
        .await
        .expect("read back")
        .expect("the general report exists");
    assert_eq!(row.job_id, None);

    // An unknown job id: 404 PF-2010 — the FK would reject
    // the insert, and a 500 for a wrong id would be a lie.
    let res = post_abuse(
        &app,
        json!({
            "job_id": Uuid::now_v7(),
            "reporter_contact": unique_contact("unknown-job"),
            "reason": "This job does not exist.",
        }),
        &unique_xff(),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010");
}

#[tokio::test]
async fn abuse_intake_rate_limited() {
    let app = rate_limit_app().await;
    // One fixed IP for all three filings: the bucket is
    // per-IP, so the three must share it — and the address
    // is unique to this run, so no other test shares it.
    let xff = unique_xff();
    for i in 0..2 {
        let res = post_abuse(&app, filing_body(&unique_contact("limited")), &xff).await;
        assert_eq!(
            res.status,
            StatusCode::CREATED,
            "filing {i} must succeed: {}",
            res.text()
        );
    }
    let res = post_abuse(&app, filing_body(&unique_contact("limited")), &xff).await;
    assert_eq!(
        res.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the third filing trips the burst-2 bucket: {}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-3007");
    let headers = &res.headers;
    assert!(
        headers.contains_key("retry-after"),
        "a 429 carries Retry-After"
    );
    assert_eq!(
        headers
            .get("x-ratelimit-limit")
            .and_then(|v| v.to_str().ok()),
        Some("2"),
        "the Limit header names the denying bucket's burst"
    );
    assert_eq!(
        headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok()),
        Some("0")
    );
}
