//! Drain integration tests (P1-028): the full pass — claim,
//! resolve, render, send, record — against a scratch Postgres
//! (testkit `TestCtx`) and the in-memory transport. No relay
//! involved: `MemoryTransport` records what the drain would
//! send, and `fail_next` drives the retry/backoff paths.
//!
//! The worker binary's own loop (interval + shutdown) is
//! covered by `apps/worker/tests/mail_drain.rs`, which spawns
//! the real binary against a fake SMTP server.

use chrono::{Duration, Utc};
use detag_core::checksum::sha256_hex;
use detag_core::repos::{audit_logs, email_outbox, email_outbox::NewOutboxMail, users};
use detag_mailer::{
    backoff_delay_secs, drain_once, DrainConfig, MemoryTransport, RateLimitConfig, TemplateEngine,
};
use detag_testkit::TestCtx;
use serde_json::json;
use uuid::Uuid;

fn drain_config() -> DrainConfig {
    DrainConfig {
        max_attempts: 3,
        backoff_base_secs: 60,
        batch: 25,
        interval_secs: 30,
        base_url: "http://localhost:8080".to_string(),
        from: "noreply@detag.local".to_string(),
        rate_limits: RateLimitConfig {
            verify_cap_per_hour: 3,
            reset_cap_per_hour: 3,
            marketing_cap_per_hour: 0,
        },
    }
}

async fn make_user(ctx: &TestCtx, email: &str) -> Uuid {
    users::create(&ctx.pool, email, "argon2id$placeholder")
        .await
        .expect("create user")
        .id
}

async fn enqueue(ctx: &TestCtx, email: &str, template: &str, payload: serde_json::Value) -> Uuid {
    email_outbox::enqueue(
        &ctx.pool,
        &NewOutboxMail {
            to_hash: sha256_hex(email.as_bytes()),
            template: template.to_string(),
            payload,
            scheduled_at: None,
        },
    )
    .await
    .expect("enqueue outbox row")
}

#[tokio::test]
async fn drain_sends_a_verify_mail_and_marks_it_sent() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let user_id = make_user(&ctx, "carol@example.com").await;
    let id = enqueue(
        &ctx,
        "carol@example.com",
        "verify-email",
        json!({ "user_id": user_id, "token": "tok-verify-1" }),
    )
    .await;
    let now = Utc::now();

    let outcome = drain_once(&ctx.pool, &transport, &engine, &drain_config(), now)
        .await
        .expect("drain pass succeeds");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.sent, 1);
    assert_eq!(outcome.transient_failed, 0);
    assert_eq!(outcome.permanent_failed, 0);

    let sent = transport.sent();
    assert_eq!(sent.len(), 1, "exactly one mail sent");
    assert_eq!(sent[0].to, "carol@example.com");
    assert_eq!(sent[0].from, "noreply@detag.local");
    assert_eq!(sent[0].subject, "Verify your Detag email");
    assert!(
        sent[0].text_body.contains("tok-verify-1"),
        "the rendered body carries the token:\n{}",
        sent[0].text_body
    );

    // A sent row is gone from the unsent read-back — the next
    // pass has nothing to do.
    assert!(
        email_outbox::unsent_for_template(&ctx.pool, "verify-email")
            .await
            .expect("read back")
            .iter()
            .all(|r| r.id != id),
        "the sent row must not be unsent anymore"
    );
    let outcome = drain_once(&ctx.pool, &transport, &engine, &drain_config(), now)
        .await
        .expect("second pass succeeds");
    assert_eq!(outcome.claimed, 0, "nothing left to drain");
    assert_eq!(transport.sent_count(), 1, "no duplicate send");

    ctx.close().await;
}

#[tokio::test]
async fn drain_retries_with_backoff_then_sends() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let user_id = make_user(&ctx, "dave@example.com").await;
    let id = enqueue(
        &ctx,
        "dave@example.com",
        "password-reset",
        json!({ "user_id": user_id, "token": "tok-reset-1" }),
    )
    .await;
    let config = drain_config();
    let t0 = Utc::now();

    // The relay fails twice, then recovers.
    transport.fail_next(2);
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, t0)
        .await
        .expect("first pass");
    assert_eq!(
        outcome,
        detag_mailer::DrainOutcome {
            claimed: 1,
            sent: 0,
            transient_failed: 1,
            permanent_failed: 0,
            rate_limited: 0,
        }
    );
    assert_eq!(transport.sent_count(), 0, "nothing sent yet");
    let stored = email_outbox::unsent_for_template(&ctx.pool, "password-reset")
        .await
        .expect("read back")
        .into_iter()
        .find(|r| r.id == id)
        .expect("failed row stays unsent");
    assert_eq!(stored.attempts, 1, "the claim spent one attempt");
    let expected_next = t0 + Duration::seconds(backoff_delay_secs(1, 60) as i64);
    assert!(
        (stored.scheduled_at - expected_next).num_seconds().abs() <= 2,
        "next attempt is base * 2^0 out (60 s): scheduled {}, expected {}",
        stored.scheduled_at,
        expected_next
    );
    assert!(
        stored
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("injected test failure"),
        "the failure reason is kept: {:?}",
        stored.last_error
    );

    // Still inside the backoff window: the drain claims nothing.
    let outcome = drain_once(
        &ctx.pool,
        &transport,
        &engine,
        &config,
        t0 + Duration::seconds(30),
    )
    .await
    .expect("second pass");
    assert_eq!(outcome.claimed, 0, "backoff window not yet over");

    // Past the window, the second failure reschedules 120 s out.
    let t1 = t0 + Duration::seconds(61);
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, t1)
        .await
        .expect("third pass");
    assert_eq!(outcome.transient_failed, 1);
    let stored = email_outbox::unsent_for_template(&ctx.pool, "password-reset")
        .await
        .expect("read back")
        .into_iter()
        .find(|r| r.id == id)
        .expect("row still unsent");
    assert_eq!(stored.attempts, 2);
    let expected_next = t1 + Duration::seconds(backoff_delay_secs(2, 60) as i64);
    assert!(
        (stored.scheduled_at - expected_next).num_seconds().abs() <= 2,
        "second failure waits 120 s: scheduled {}, expected {}",
        stored.scheduled_at,
        expected_next
    );

    // Past that window the relay has recovered: the mail sends.
    let t2 = t1 + Duration::seconds(121);
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, t2)
        .await
        .expect("fourth pass");
    assert_eq!(outcome.sent, 1);
    assert_eq!(transport.sent_count(), 1);
    assert_eq!(transport.sent()[0].subject, "Reset your Detag password");

    ctx.close().await;
}

#[tokio::test]
async fn drain_parks_a_row_that_never_stops_failing() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let user_id = make_user(&ctx, "erin@example.com").await;
    let id = enqueue(
        &ctx,
        "erin@example.com",
        "verify-email",
        json!({ "user_id": user_id, "token": "tok-verify-2" }),
    )
    .await;
    let config = drain_config();
    transport.fail_next(100); // the relay never recovers
    let mut now = Utc::now();

    // Three attempts (max_attempts = 3), each past its backoff.
    for attempt in 1..=3 {
        let outcome = drain_once(&ctx.pool, &transport, &engine, &config, now)
            .await
            .expect("drain pass");
        assert_eq!(outcome.claimed, 1, "attempt {attempt} claims the row");
        assert_eq!(outcome.transient_failed, 1);
        now += Duration::seconds(backoff_delay_secs(attempt, 60) as i64 + 1);
    }
    // Attempts exhausted: the row is terminal, never claimed again.
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, now)
        .await
        .expect("final pass");
    assert_eq!(outcome.claimed, 0, "exhausted rows are never claimed again");
    assert_eq!(transport.sent_count(), 0, "nothing ever sent");
    let stored = email_outbox::unsent_for_template(&ctx.pool, "verify-email")
        .await
        .expect("read back")
        .into_iter()
        .find(|r| r.id == id)
        .expect("exhausted row is still in the table");
    assert_eq!(stored.attempts, 3, "attempts == max");
    assert!(
        stored.last_error.is_some(),
        "the last failure reason is kept for ops"
    );

    ctx.close().await;
}

#[tokio::test]
async fn drain_parks_unknown_templates_and_bad_recipients() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let user_id = make_user(&ctx, "fran@example.com").await;
    let config = drain_config();

    // Unknown template: parked on the first pass, never retried.
    let unknown = enqueue(
        &ctx,
        "fran@example.com",
        "marketing-drip",
        json!({ "user_id": user_id }),
    )
    .await;
    // to_hash mismatch: the row names a different address than
    // the user's — parked, never sent anywhere.
    let mismatch = email_outbox::enqueue(
        &ctx.pool,
        &NewOutboxMail {
            to_hash: sha256_hex("someone-else@example.com".as_bytes()),
            template: "verify-email".to_string(),
            payload: json!({ "user_id": user_id, "token": "tok-x" }),
            scheduled_at: None,
        },
    )
    .await
    .expect("enqueue mismatch row");
    // Deleted user: the payload names a user that does not exist.
    let ghost = enqueue(
        &ctx,
        "fran@example.com",
        "verify-email",
        json!({ "user_id": Uuid::now_v7(), "token": "tok-y" }),
    )
    .await;

    // The clock for the pass is sampled after the rows exist:
    // a row's scheduled_at defaults to its insert time, so a
    // `now` captured before the enqueues would predate it and
    // the claim would (correctly) find nothing due.
    let now = Utc::now();
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, now)
        .await
        .expect("drain pass");
    assert_eq!(outcome.claimed, 3);
    assert_eq!(outcome.sent, 0);
    assert_eq!(outcome.permanent_failed, 3);
    assert_eq!(
        transport.sent_count(),
        0,
        "nothing is ever sent for bad rows"
    );
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, now)
        .await
        .expect("second pass");
    assert_eq!(outcome.claimed, 0, "parked rows are never claimed again");
    for (id, why) in [
        (unknown, "unknown template"),
        (mismatch, "to_hash mismatch"),
        (ghost, "missing user"),
    ] {
        let stored = email_outbox::unsent_for_template(&ctx.pool, "verify-email")
            .await
            .expect("read back")
            .into_iter()
            .find(|r| r.id == id);
        // The unknown-template row lives under its own template
        // name, so read it back through the generic query path.
        let stored = match stored {
            Some(row) => row,
            None => {
                let rows = email_outbox::unsent_for_template(&ctx.pool, "marketing-drip")
                    .await
                    .expect("read back");
                rows.into_iter()
                    .find(|r| r.id == id)
                    .unwrap_or_else(|| panic!("parked row {id} ({why}) must stay in the table"))
            }
        };
        assert_eq!(stored.attempts, 3, "{why}: parked at max attempts");
        assert!(stored.last_error.is_some(), "{why}: reason kept");
    }

    ctx.close().await;
}

#[tokio::test]
async fn drain_sends_email_change_verify_to_the_pending_address() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let user_id = make_user(&ctx, "gail@example.com").await;
    // The pending email is the new address; the old address is
    // the one on record. The mail must go to the new one, and
    // its to_hash must be the new address's hash.
    users::set_pending_email(&ctx.pool, user_id, "gail-new@example.com")
        .await
        .expect("set pending email");
    let id = email_outbox::enqueue(
        &ctx.pool,
        &NewOutboxMail {
            to_hash: sha256_hex("gail-new@example.com".as_bytes()),
            template: "email-change-verify".to_string(),
            payload: json!({ "user_id": user_id, "token": "tok-change-1" }),
            scheduled_at: None,
        },
    )
    .await
    .expect("enqueue change-verify row");
    let config = drain_config();
    let now = Utc::now();

    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, now)
        .await
        .expect("drain pass");
    assert_eq!(outcome.sent, 1);
    let sent = transport.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].to, "gail-new@example.com",
        "goes to the NEW address"
    );
    assert_eq!(sent[0].subject, "Verify your new Detag email");
    assert!(
        sent[0].text_body.contains("tok-change-1"),
        "the token is in the body"
    );
    let _ = id;

    ctx.close().await;
}

#[tokio::test]
async fn drain_parks_email_change_verify_with_no_pending_email() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let user_id = make_user(&ctx, "hank@example.com").await;
    // No pending email on the user: the drain cannot know the
    // new address — park, do not fall back to the old one.
    let _id = email_outbox::enqueue(
        &ctx.pool,
        &NewOutboxMail {
            to_hash: sha256_hex("hank-new@example.com".as_bytes()),
            template: "email-change-verify".to_string(),
            payload: json!({ "user_id": user_id, "token": "tok-change-2" }),
            scheduled_at: None,
        },
    )
    .await
    .expect("enqueue change-verify row");
    let config = drain_config();

    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("drain pass");
    assert_eq!(outcome.permanent_failed, 1);
    assert_eq!(transport.sent_count(), 0, "no fallback to the old address");

    ctx.close().await;
}

// --- P1-031: per-recipient hourly send caps ---

/// Enqueue `n` rows of one template for one recipient and drain
/// once; the caller asserts the outcome. The token varies per
/// row so the rows are distinct mails, not duplicates.
async fn enqueue_n(
    ctx: &TestCtx,
    email: &str,
    template: &str,
    user_id: Uuid,
    n: usize,
) -> Vec<Uuid> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(
            enqueue(
                ctx,
                email,
                template,
                json!({ "user_id": user_id, "token": format!("tok-{template}-{i}") }),
            )
            .await,
        );
    }
    ids
}

/// Read back one outbox row's stored error, for asserting the
/// park reason on a rate-limited row.
async fn stored_error(ctx: &TestCtx, template: &str, id: Uuid) -> Option<String> {
    email_outbox::unsent_for_template(&ctx.pool, template)
        .await
        .expect("read back")
        .into_iter()
        .find(|r| r.id == id)
        .and_then(|r| r.last_error)
}

/// The abuse audit rows for one trip, oldest first.
async fn trip_audits(ctx: &TestCtx) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_action(&ctx.pool, "email.rate_limit_tripped")
        .await
        .expect("read audits")
}

#[tokio::test]
async fn drain_parks_fourth_verify_mail_in_an_hour_and_audits_the_trip() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let email = "ivan-verify@example.com";
    let user_id = make_user(&ctx, email).await;
    let config = drain_config();

    // Three verification mails go out inside the hour.
    enqueue_n(&ctx, email, "verify-email", user_id, 3).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("first pass");
    assert_eq!(outcome.sent, 3);
    assert_eq!(outcome.rate_limited, 0);

    // The fourth trips the cap: parked, never sent, audited.
    let fourth = enqueue(
        &ctx,
        email,
        "verify-email",
        json!({ "user_id": user_id, "token": "tok-verify-4" }),
    )
    .await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("second pass");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.sent, 0);
    assert_eq!(outcome.permanent_failed, 1);
    assert_eq!(outcome.rate_limited, 1);
    assert_eq!(transport.sent_count(), 3, "the 4th mail never left");
    assert!(
        stored_error(&ctx, "verify-email", fourth)
            .await
            .unwrap_or_default()
            .contains("rate limit"),
        "the park reason names the cap"
    );

    // The trip is in the abuse audit log: hash, not address.
    let audits = trip_audits(&ctx).await;
    assert_eq!(audits.len(), 1, "exactly one trip logged");
    let audit = &audits[0];
    assert_eq!(
        audit.target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
    let after = audit.after.clone().expect("trip carries facts");
    assert_eq!(after["to_hash"], sha256_hex(email.as_bytes()));
    assert_eq!(after["template"], "verify-email");
    assert_eq!(after["class"], "verify");
    assert_eq!(after["cap_per_hour"], 3);
    assert_eq!(after["sent_in_window"], 3);
    assert!(
        !after.to_string().contains(email),
        "no raw address in the audit row: {after}"
    );

    // Parked is terminal: the next pass has nothing to do.
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("third pass");
    assert_eq!(outcome.claimed, 0);
    assert_eq!(trip_audits(&ctx).await.len(), 1, "no second audit");

    ctx.close().await;
}

#[tokio::test]
async fn drain_parks_fourth_reset_mail_in_an_hour() {
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let email = "judy-reset@example.com";
    let user_id = make_user(&ctx, email).await;
    let config = drain_config();

    enqueue_n(&ctx, email, "password-reset", user_id, 3).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("first pass");
    assert_eq!(outcome.sent, 3);

    enqueue_n(&ctx, email, "password-reset", user_id, 1).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("second pass");
    assert_eq!(outcome.sent, 0);
    assert_eq!(outcome.rate_limited, 1);
    assert_eq!(transport.sent_count(), 3);
    let audits = trip_audits(&ctx).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].after.clone().expect("facts")["class"], "reset");

    ctx.close().await;
}

#[tokio::test]
async fn drain_parks_marketing_mail_on_sight() {
    // Marketing cap is 0: no marketing mail is ever sent in
    // v1 — even the first one trips, before any render.
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let email = "mallory-mkt@example.com";
    let user_id = make_user(&ctx, email).await;
    let config = drain_config();

    enqueue(
        &ctx,
        email,
        "marketing-newsletter",
        json!({ "user_id": user_id }),
    )
    .await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("drain pass");
    assert_eq!(outcome.sent, 0);
    assert_eq!(outcome.rate_limited, 1);
    assert_eq!(transport.sent_count(), 0);
    let audits = trip_audits(&ctx).await;
    assert_eq!(audits.len(), 1);
    let after = audits[0].after.clone().expect("facts");
    assert_eq!(after["class"], "marketing");
    assert_eq!(after["cap_per_hour"], 0);
    assert_eq!(after["sent_in_window"], 0);

    ctx.close().await;
}

#[tokio::test]
async fn drain_sends_third_verify_mail_under_the_cap() {
    // The cap is 3/hour: the 3rd mail still goes — the trip
    // is at the 4th, not the 3rd.
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let email = "karl-under@example.com";
    let user_id = make_user(&ctx, email).await;
    let config = drain_config();

    enqueue_n(&ctx, email, "verify-email", user_id, 2).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("first pass");
    assert_eq!(outcome.sent, 2);

    enqueue_n(&ctx, email, "verify-email", user_id, 1).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("second pass");
    assert_eq!(outcome.sent, 1, "3rd of 3 sends");
    assert_eq!(outcome.rate_limited, 0);
    assert_eq!(transport.sent_count(), 3);
    assert_eq!(trip_audits(&ctx).await.len(), 0, "no trip logged");

    ctx.close().await;
}

#[tokio::test]
async fn drain_cap_window_expires_after_an_hour() {
    // Three sends, then the hour passes (sent_at backdated
    // past the window): the next mail sends again.
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let email = "lara-window@example.com";
    let user_id = make_user(&ctx, email).await;
    let config = drain_config();

    let ids = enqueue_n(&ctx, email, "verify-email", user_id, 3).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("first pass");
    assert_eq!(outcome.sent, 3);

    let old = Utc::now() - Duration::minutes(61);
    for id in ids {
        sqlx::query("UPDATE email_outbox SET sent_at = $1 WHERE id = $2")
            .bind(old)
            .bind(id)
            .execute(&ctx.pool)
            .await
            .expect("backdate sent_at");
    }

    enqueue_n(&ctx, email, "verify-email", user_id, 1).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("second pass");
    assert_eq!(outcome.sent, 1, "window expired: the mail sends");
    assert_eq!(outcome.rate_limited, 0);

    ctx.close().await;
}

#[tokio::test]
async fn drain_cap_is_per_recipient() {
    // Three sends to one address do not spend another
    // address's budget.
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let config = drain_config();

    let user_a = make_user(&ctx, "mia-a@example.com").await;
    let user_b = make_user(&ctx, "nina-b@example.com").await;
    enqueue_n(&ctx, "mia-a@example.com", "verify-email", user_a, 3).await;
    enqueue_n(&ctx, "nina-b@example.com", "verify-email", user_b, 1).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("drain pass");
    assert_eq!(outcome.sent, 4, "both recipients get their mail");
    assert_eq!(outcome.rate_limited, 0);

    ctx.close().await;
}

#[tokio::test]
async fn drain_change_verify_shares_the_verify_cap() {
    // `verify-email` and `email-change-verify` are one class:
    // two of the first + one of the second fill the budget, and
    // the next change-verify trips it.
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let email = "omar-shared@example.com";
    let user_id = make_user(&ctx, email).await;
    // Pending address == address on record, so both templates
    // resolve to the same recipient hash.
    users::set_pending_email(&ctx.pool, user_id, email)
        .await
        .expect("set pending email");
    let config = drain_config();

    enqueue_n(&ctx, email, "verify-email", user_id, 2).await;
    enqueue_n(&ctx, email, "email-change-verify", user_id, 1).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("first pass");
    assert_eq!(outcome.sent, 3, "2 verify + 1 change-verify fill the cap");

    enqueue_n(&ctx, email, "email-change-verify", user_id, 1).await;
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("second pass");
    assert_eq!(outcome.sent, 0);
    assert_eq!(outcome.rate_limited, 1);
    let audits = trip_audits(&ctx).await;
    assert_eq!(audits.len(), 1);
    let after = audits[0].after.clone().expect("facts");
    assert_eq!(after["template"], "email-change-verify");
    assert_eq!(after["class"], "verify");
    assert_eq!(after["sent_in_window"], 3);

    ctx.close().await;
}

#[tokio::test]
async fn drain_never_caps_transactional_alerts() {
    // Security alerts are uncapped: four lockout notices to
    // one address in an hour all go out — the user must hear
    // about the hammering precisely when it happens.
    let ctx = TestCtx::new().await;
    let engine = TemplateEngine::new().expect("templates compile");
    let transport = MemoryTransport::new();
    let email = "pam-alerts@example.com";
    let user_id = make_user(&ctx, email).await;
    let config = drain_config();

    for _ in 0..4 {
        enqueue(&ctx, email, "lockout-notice", json!({ "user_id": user_id })).await;
    }
    let outcome = drain_once(&ctx.pool, &transport, &engine, &config, Utc::now())
        .await
        .expect("drain pass");
    assert_eq!(outcome.sent, 4, "alerts are never capped");
    assert_eq!(outcome.rate_limited, 0);
    assert_eq!(trip_audits(&ctx).await.len(), 0, "no trip logged");

    ctx.close().await;
}
