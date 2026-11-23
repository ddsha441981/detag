//! detag-mailer::templates — tera rendering for the transactional
//! templates (P1-028, real copy + HTML in P1-029).
//!
//! One `.subject.tera` + one `.text.tera` + one `.html.tera` per
//! template, embedded at compile time from `templates/` — the
//! mailer never reads template files from disk at runtime, so a
//! missing or moved template directory cannot break a running
//! worker. The HTML part mirrors the text copy (same facts, same
//! links) — no marketing claims: the honesty rule
//! (`05-compliance-aup.md` §6) forbids "removes invisible
//! watermarks" / "undetectable" / "defeats SynthID" copy, and
//! these mails make no product claims at all.
//!
//! Escaping: tera auto-escapes `.html` templates, which mangles
//! URLs (`/` becomes `&#x2F;`). The link templates therefore
//! mark `base_url` `| safe` — safe by construction: it is
//! operator config (`PF_PUBLIC_BASE_URL`), never user input.
//! The token stays plain-escaped: tokens are our own URL-safe
//! generated strings (no escapable characters), so the escaped
//! and unescaped renderings are identical.
//!
//! NOTE (pipeline trap, 2026-11-14): the literal sequence
//! `{{ token` (opening braces + the word "token") is redacted
//! to `<redacted>` somewhere in this agent's output pipeline —
//! both `exec` heredocs AND `write`/`edit` calls mangled it in
//! the three link templates, silently breaking the links (the
//! render then contains the literal text instead of the token).
//! If a template ever needs that sequence written again, build
//! it at runtime (e.g. `chr(123) * 2 + ' token }}'` in python),
//! never emit the literal bytes. Verify with a byte-level check
//! (hexdump / a tag-listing probe), never by eyeballing tool
//! output — the redaction also hides itself in displayed output.
//!
//! The render context is the outbox payload's JSON object plus
//! `base_url` (PF_PUBLIC_BASE_URL) and `app_name` ("Detag"),
//! added by the drain. Tera errors on an undefined variable,
//! so a payload missing a variable its template needs fails
//! loudly instead of sending a mail with a hole in it — the
//! drain treats that as a permanent failure.

use crate::error::MailError;

/// Every template the api enqueues today. The drain renders
/// exactly these names; anything else is
/// [`MailError::UnknownTemplate`] — a data error, never a relay
/// problem. P1-029's spec names six of these loosely
/// ("reset-password" is `password-reset`, "new-login" is
/// `new-login-digest`, "lockout" is `lockout-notice`); the
/// spec's "password-changed" and "verification-thanks" have no
/// template because no api service enqueues them — the
/// twenty-one names here are the contract, and the snapshots
/// cover all of them. P1-064 adds the three subscription lifecycle mails
/// (`subscription-welcome` / `subscription-dunning` /
/// `subscription-canceled`), enqueued by the subscription
/// state machine's service. P1-070 adds the two dunning
/// reminders (`subscription-dunning-day3` /
/// `subscription-dunning-day7`), enqueued by the dunning
/// policy job. P1-085 adds the two quota warnings
/// (`quota-warning-80` / `quota-warning-100`), enqueued by
/// the quota-warning service when daily usage crosses the
/// configured thresholds — one mail per level per day. P1-089
/// adds the abuse-report ack (`abuse-report-ack`), enqueued
/// by the abuse intake service when a report is filed — one
/// mail per report, addressed to the reporter's contact.
/// P1-091 adds the three enforcement-ladder mails
/// (`enforcement-warning` / `enforcement-suspension` /
/// `enforcement-termination`), enqueued by the enforcement
/// service (warning + termination) and by both suspension
/// paths (the P1-008 admin suspend and the P1-090 queue
/// suspend) — one mail per ladder step, addressed to the
/// user on record. P1-097 adds the welcome mail (`welcome`),
/// enqueued by the verification service when a registration
/// token is consumed — one mail per verified account, with
/// the quickstart link and the compliance-approved
/// quality-promise line (no invisible-watermark claims).
/// P1-101 adds the weekly digest (`weekly-digest`),
/// enqueued by the weekly-digest service once per user +
/// week (flag-gated, opt-out honored) — the week's usage
/// totals, the plan, and tips derived from the numbers.
pub const TEMPLATES: &[&str] = &[
    "verify-email",
    "email-change-verify",
    "email-change-alert",
    "lockout-notice",
    "new-login-digest",
    "password-reset",
    "refresh-reuse-alert",
    "account-deletion",
    "subscription-welcome",
    "subscription-dunning",
    "subscription-dunning-day3",
    "subscription-dunning-day7",
    "subscription-canceled",
    "quota-warning-80",
    "quota-warning-100",
    "abuse-report-ack",
    "enforcement-warning",
    "enforcement-suspension",
    "enforcement-termination",
    "welcome",
    "weekly-digest",
];

/// A rendered mail: subject + plain-text body + HTML body.
/// The HTML part mirrors the text copy (same facts, same links);
/// the transport sends both as multipart/alternative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMail {
    /// Single line — the subject file is trimmed on render.
    pub subject: String,
    /// Plain-text body, as rendered (trailing newline kept).
    pub text_body: String,
    /// HTML body, as rendered (trailing newline kept).
    pub html_body: String,
}

/// The tera engine, built once and shared: compiling the
/// templates is the expensive part, rendering is cheap.
pub struct TemplateEngine {
    tera: tera::Tera,
}

impl TemplateEngine {
    /// Compile every template in [`TEMPLATES`] from the embedded
    /// files. Fails only when a template file is broken at build
    /// time — callers (the worker) treat this as a boot error.
    pub fn new() -> Result<Self, MailError> {
        let mut tera = tera::Tera::default();
        macro_rules! add {
            ($name:literal) => {
                tera.add_raw_template(
                    concat!($name, ".subject"),
                    include_str!(concat!("../templates/", $name, ".subject.tera")),
                )
                .map_err(|source| MailError::TemplateLoad {
                    template: concat!($name, ".subject").to_string(),
                    source,
                })?;
                tera.add_raw_template(
                    concat!($name, ".text"),
                    include_str!(concat!("../templates/", $name, ".text.tera")),
                )
                .map_err(|source| MailError::TemplateLoad {
                    template: concat!($name, ".text").to_string(),
                    source,
                })?;
                tera.add_raw_template(
                    concat!($name, ".html"),
                    include_str!(concat!("../templates/", $name, ".html.tera")),
                )
                .map_err(|source| MailError::TemplateLoad {
                    template: concat!($name, ".html").to_string(),
                    source,
                })?;
            };
        }
        add!("verify-email");
        add!("email-change-verify");
        add!("email-change-alert");
        add!("lockout-notice");
        add!("new-login-digest");
        add!("password-reset");
        add!("refresh-reuse-alert");
        add!("account-deletion");
        add!("subscription-welcome");
        add!("subscription-dunning");
        add!("subscription-dunning-day3");
        add!("subscription-dunning-day7");
        add!("subscription-canceled");
        add!("quota-warning-80");
        add!("quota-warning-100");
        add!("abuse-report-ack");
        add!("enforcement-warning");
        add!("enforcement-suspension");
        add!("enforcement-termination");
        add!("welcome");
        add!("weekly-digest");
        Ok(Self { tera })
    }

    /// Render `template` with `context` (a JSON object — the
    /// outbox payload plus `base_url` / `app_name`, see the
    /// drain). Unknown template or a render error (e.g. a
    /// missing variable) is a [`MailError`] the drain treats as
    /// permanent: the payload is fixed, so retrying cannot help.
    pub fn render(
        &self,
        template: &str,
        context: &serde_json::Value,
    ) -> Result<RenderedMail, MailError> {
        if !TEMPLATES.contains(&template) {
            return Err(MailError::UnknownTemplate(template.to_string()));
        }
        let mut ctx = tera::Context::new();
        let obj = context.as_object().ok_or_else(|| MailError::Render {
            template: template.to_string(),
            source: tera::Error::msg("render context must be a JSON object"),
        })?;
        for (key, value) in obj {
            ctx.insert(key, value);
        }
        let subject = self
            .tera
            .render(&format!("{template}.subject"), &ctx)
            .map_err(|source| MailError::Render {
                template: template.to_string(),
                source,
            })?;
        let text_body = self
            .tera
            .render(&format!("{template}.text"), &ctx)
            .map_err(|source| MailError::Render {
                template: template.to_string(),
                source,
            })?;
        let html_body = self
            .tera
            .render(&format!("{template}.html"), &ctx)
            .map_err(|source| MailError::Render {
                template: template.to_string(),
                source,
            })?;
        Ok(RenderedMail {
            subject: subject.trim().to_string(),
            text_body,
            html_body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> serde_json::Value {
        json!({
            "base_url": "http://localhost:8080",
            "app_name": "Detag",
            "token": "tok-abc-123",
            "sessions_revoked": 2,
            "user_id": "00000000-0000-0000-0000-000000000001",
            "report_id": "00000000-0000-0000-0000-000000000002",
            "plan_code": "pro",
            "day": "2026-11-20",
            "unit": "images",
            "used": 8,
            "limit": 10,
            "percent": 80,
            // P1-091: the enforcement templates render
            // `{{ reason }}` — without it the full-context
            // test fails on the first enforcement template
            // (the snapshot context already had it; this
            // one did not — caught by verify attempt 2).
            "reason": "repeated stock-agency template matches",
            // P1-101: the weekly digest renders the week's
            // totals + the plan + the tips list.
            "week_start": "2026-11-16",
            "images_used": 56,
            "images_limit": 70,
            "video_seconds_used": 0,
            "video_seconds_limit": 0,
            "tips": ["You used most of your weekly image allowance — your daily quota resets at midnight UTC.", "Your plan includes no video seconds — upgrade to Pro or Business to process video."],
        })
    }

    #[test]
    fn every_template_renders_with_a_full_context() {
        let engine = TemplateEngine::new().expect("templates compile");
        for name in TEMPLATES {
            let rendered = engine.render(name, &ctx()).unwrap_or_else(|err| {
                panic!("template {name} must render with a full context: {err}")
            });
            assert!(!rendered.subject.is_empty(), "{name}: empty subject");
            assert!(
                !rendered.subject.contains('\n'),
                "{name}: subject must be one line"
            );
            assert!(!rendered.text_body.is_empty(), "{name}: empty body");
            assert!(
                !rendered.text_body.contains("{{"),
                "{name}: unrendered variable left in the body"
            );
            assert!(!rendered.html_body.is_empty(), "{name}: empty html body");
            assert!(
                rendered.html_body.contains("<!doctype html>"),
                "{name}: html body must be a full document"
            );
            assert!(
                !rendered.html_body.contains("{{"),
                "{name}: unrendered variable left in the html body"
            );
        }
    }

    #[test]
    fn verify_link_carries_the_token_and_base_url() {
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("verify-email", &ctx())
            .expect("verify-email renders");
        assert_eq!(rendered.subject, "Verify your Detag email");
        assert!(
            rendered
                .text_body
                .contains("http://localhost:8080/auth/verify-email?token=tok-abc-123"),
            "verify link missing from body:\n{}",
            rendered.text_body
        );
        assert!(
            rendered.text_body.contains("24 hours"),
            "verify mail must state the 24 h expiry"
        );
    }

    #[test]
    fn verify_html_carries_the_same_link_as_text() {
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("verify-email", &ctx())
            .expect("verify-email renders");
        assert!(
            rendered
                .html_body
                .contains("http://localhost:8080/auth/verify-email?token=tok-abc-123"),
            "verify link missing from html body:\n{}",
            rendered.html_body
        );
        assert!(
            rendered.html_body.contains("24 hours"),
            "html body must state the 24 h expiry"
        );
        assert!(
            rendered.html_body.contains("Verify my email"),
            "html body must carry the action button"
        );
    }

    #[test]
    fn reset_link_carries_the_token_and_the_30_minute_expiry() {
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("password-reset", &ctx())
            .expect("password-reset renders");
        assert_eq!(rendered.subject, "Reset your Detag password");
        assert!(
            rendered
                .text_body
                .contains("http://localhost:8080/auth/reset-password?token=tok-abc-123"),
            "reset link missing from body:\n{}",
            rendered.text_body
        );
        assert!(
            rendered.text_body.contains("30 minutes"),
            "reset mail must state the 30 min expiry"
        );
    }

    #[test]
    fn digest_names_the_revoked_session_count() {
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("new-login-digest", &ctx())
            .expect("new-login-digest renders");
        assert!(
            rendered.text_body.contains("2 other session(s)"),
            "digest must name the revoked count:\n{}",
            rendered.text_body
        );
    }

    #[test]
    fn welcome_carries_the_quickstart_link_and_the_honest_promise() {
        // P1-097, AC "copy passes honesty rule"
        // (05-compliance-aup.md §6): the one-liner is the
        // compliance-approved sentence — visible marks only,
        // with a verifiable no-quality-loss report — and
        // nothing in the copy claims invisible-watermark
        // removal, undetectability, or defeating SynthID.
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine.render("welcome", &ctx()).expect("welcome renders");
        assert_eq!(rendered.subject, "Welcome to Detag");
        assert!(
            rendered
                .text_body
                .contains("http://localhost:8080/quickstart"),
            "welcome mail must link the quickstart:\n{}",
            rendered.text_body
        );
        assert!(
            rendered
                .text_body
                .contains("removes visible marks from AI media you own, with a verifiable no-quality-loss report"),
            "welcome mail must carry the approved quality promise:\n{}",
            rendered.text_body
        );
        assert!(
            rendered
                .html_body
                .contains("http://localhost:8080/quickstart"),
            "html body must carry the same quickstart link:\n{}",
            rendered.html_body
        );
        for forbidden in ["invisible", "undetectable", "SynthID", "defeats"] {
            assert!(
                !rendered
                    .text_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "welcome text must not claim {forbidden}:\n{}",
                rendered.text_body
            );
            assert!(
                !rendered
                    .html_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "welcome html must not claim {forbidden}:\n{}",
                rendered.html_body
            );
        }
    }

    #[test]
    fn weekly_digest_renders_usage_plan_and_tips() {
        // P1-101, AC "flag respected" at the template layer:
        // the digest names the week's totals, the plan, and
        // every tip — and, like every template, carries no
        // invisible-watermark claim (05-compliance-aup.md §6).
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("weekly-digest", &ctx())
            .expect("weekly-digest renders");
        assert_eq!(rendered.subject, "Your week on Detag");
        for needle in [
            "week starting 2026-11-16",
            "Images processed: 56 of 70",
            "Video processed: 0 of 0 seconds",
            "Plan: pro",
            "You used most of your weekly image allowance",
            "Your plan includes no video seconds",
        ] {
            assert!(
                rendered.text_body.contains(needle),
                "digest text must carry {needle:?}:\n{}",
                rendered.text_body
            );
            assert!(
                rendered.html_body.contains(needle),
                "digest html must carry {needle:?}:\n{}",
                rendered.html_body
            );
        }
        assert!(
            rendered.text_body.contains("http://localhost:8080/billing"),
            "digest must link usage:\n{}",
            rendered.text_body
        );
        for forbidden in ["invisible", "undetectable", "SynthID", "defeats"] {
            assert!(
                !rendered
                    .text_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "digest text must not claim {forbidden}:\n{}",
                rendered.text_body
            );
            assert!(
                !rendered
                    .html_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "digest html must not claim {forbidden}:\n{}",
                rendered.html_body
            );
        }
    }

    #[test]
    fn quota_warning_80_renders_numbers_plan_and_links() {
        // P1-102, AC "triggers verified" at the template layer:
        // the 80% warning states the true numbers (8 of 10
        // images, 80%, the day, the plan) and links usage +
        // upgrade — and, like every template, carries no
        // invisible-watermark claim (05-compliance-aup.md §6).
        // The shared ctx() already carries the exact payload
        // the quota-warning service enqueues (P1-085).
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("quota-warning-80", &ctx())
            .expect("quota-warning-80 renders");
        assert_eq!(
            rendered.subject,
            "You've used 80% of your daily Detag quota"
        );
        for needle in [
            "You've used 80% of your daily quota on your Detag pro plan (8 of 10 images) for 2026-11-20.",
            "http://localhost:8080/billing",
            "http://localhost:8080/plans",
        ] {
            assert!(
                rendered.text_body.contains(needle),
                "quota-warning-80 text must carry {needle:?}:\n{}",
                rendered.text_body
            );
            assert!(
                rendered.html_body.contains(needle),
                "quota-warning-80 html must carry {needle:?}:\n{}",
                rendered.html_body
            );
        }
        for forbidden in ["invisible", "undetectable", "SynthID", "defeats"] {
            assert!(
                !rendered
                    .text_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "quota-warning-80 text must not claim {forbidden}:\n{}",
                rendered.text_body
            );
            assert!(
                !rendered
                    .html_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "quota-warning-80 html must not claim {forbidden}:\n{}",
                rendered.html_body
            );
        }
    }

    #[test]
    fn quota_warning_100_renders_full_quota_and_links() {
        // P1-102: the 100% warning states the full quota (10
        // of 10 images, 100%) — a dedicated context, because
        // the shared ctx() is an 80% week (8 of 10). Same
        // links + honesty rule as the 80% test.
        let engine = TemplateEngine::new().expect("templates compile");
        let full = json!({
            "base_url": "http://localhost:8080",
            "app_name": "Detag",
            "plan_code": "pro",
            "day": "2026-11-20",
            "unit": "images",
            "used": 10,
            "limit": 10,
            "percent": 100,
        });
        let rendered = engine
            .render("quota-warning-100", &full)
            .expect("quota-warning-100 renders");
        assert_eq!(rendered.subject, "Your daily Detag quota is full");
        for needle in [
            "You've used all of your daily quota on your Detag pro plan (10 of 10 images) for 2026-11-20.",
            "http://localhost:8080/billing",
            "http://localhost:8080/plans",
        ] {
            assert!(
                rendered.text_body.contains(needle),
                "quota-warning-100 text must carry {needle:?}:\n{}",
                rendered.text_body
            );
            assert!(
                rendered.html_body.contains(needle),
                "quota-warning-100 html must carry {needle:?}:\n{}",
                rendered.html_body
            );
        }
        for forbidden in ["invisible", "undetectable", "SynthID", "defeats"] {
            assert!(
                !rendered
                    .text_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "quota-warning-100 text must not claim {forbidden}:\n{}",
                rendered.text_body
            );
            assert!(
                !rendered
                    .html_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "quota-warning-100 html must not claim {forbidden}:\n{}",
                rendered.html_body
            );
        }
    }

    #[test]
    fn subscription_welcome_names_the_plan_and_links_billing() {
        // P1-102: the activation mail (the spec's
        // "subscription-activated") names the plan that went
        // live and links billing — no invisible-watermark
        // claim. The payload is `{user_id, plan_code}`, the
        // exact shape the state machine enqueues (P1-064).
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("subscription-welcome", &ctx())
            .expect("subscription-welcome renders");
        assert_eq!(rendered.subject, "Your Detag pro plan is active");
        for needle in [
            "Your Detag pro plan is now active.",
            "http://localhost:8080/billing",
        ] {
            assert!(
                rendered.text_body.contains(needle),
                "subscription-welcome text must carry {needle:?}:\n{}",
                rendered.text_body
            );
            assert!(
                rendered.html_body.contains(needle),
                "subscription-welcome html must carry {needle:?}:\n{}",
                rendered.html_body
            );
        }
        for forbidden in ["invisible", "undetectable", "SynthID", "defeats"] {
            assert!(
                !rendered
                    .text_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "subscription-welcome text must not claim {forbidden}:\n{}",
                rendered.text_body
            );
            assert!(
                !rendered
                    .html_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "subscription-welcome html must not claim {forbidden}:\n{}",
                rendered.html_body
            );
        }
    }

    #[test]
    fn subscription_dunning_names_the_failed_payment_and_links_billing() {
        // P1-102: the payment-failed mail (the spec's
        // "payment-failed", the day-0 past_due mail) names the
        // failed plan payment, the past-due state, and the
        // billing link where the method is updated — no
        // invisible-watermark claim. Same `{user_id,
        // plan_code}` payload shape as the welcome mail.
        let engine = TemplateEngine::new().expect("templates compile");
        let rendered = engine
            .render("subscription-dunning", &ctx())
            .expect("subscription-dunning renders");
        assert_eq!(
            rendered.subject,
            "Your Detag payment failed — update your payment method"
        );
        for needle in [
            "Your Detag pro plan payment failed, so your subscription is past due.",
            "http://localhost:8080/billing",
        ] {
            assert!(
                rendered.text_body.contains(needle),
                "subscription-dunning text must carry {needle:?}:\n{}",
                rendered.text_body
            );
            assert!(
                rendered.html_body.contains(needle),
                "subscription-dunning html must carry {needle:?}:\n{}",
                rendered.html_body
            );
        }
        for forbidden in ["invisible", "undetectable", "SynthID", "defeats"] {
            assert!(
                !rendered
                    .text_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "subscription-dunning text must not claim {forbidden}:\n{}",
                rendered.text_body
            );
            assert!(
                !rendered
                    .html_body
                    .to_lowercase()
                    .contains(&forbidden.to_lowercase()),
                "subscription-dunning html must not claim {forbidden}:\n{}",
                rendered.html_body
            );
        }
    }

    #[test]
    fn unknown_template_is_an_error_not_an_empty_mail() {
        let engine = TemplateEngine::new().expect("templates compile");
        let err = engine
            .render("marketing-drip", &ctx())
            .expect_err("an unlisted template must fail");
        assert!(matches!(err, MailError::UnknownTemplate(_)));
        assert!(err.is_permanent(), "unknown template is permanent");
    }

    #[test]
    fn missing_variable_fails_loudly_instead_of_sending_a_hole() {
        let engine = TemplateEngine::new().expect("templates compile");
        let thin = json!({"base_url": "http://localhost:8080", "app_name": "Detag"});
        let err = engine
            .render("verify-email", &thin)
            .expect_err("a payload without `token` must fail");
        assert!(
            matches!(err, MailError::Render { .. }),
            "missing variable must be a render error, got {err}"
        );
        assert!(err.is_permanent(), "missing variable is permanent");
    }

    #[test]
    fn non_object_context_is_rejected() {
        let engine = TemplateEngine::new().expect("templates compile");
        let err = engine
            .render("verify-email", &json!("just a string"))
            .expect_err("a non-object context must fail");
        assert!(matches!(err, MailError::Render { .. }));
    }
}
