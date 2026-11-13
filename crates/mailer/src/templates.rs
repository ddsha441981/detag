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
/// template because no api service enqueues them — the eight
/// names here are the contract, and the snapshots cover all of
/// them.
pub const TEMPLATES: &[&str] = &[
    "verify-email",
    "email-change-verify",
    "email-change-alert",
    "lockout-notice",
    "new-login-digest",
    "password-reset",
    "refresh-reuse-alert",
    "account-deletion",
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
