//! detag-mailer::templates — tera rendering for the transactional
//! templates (P1-028).
//!
//! One `.subject.tera` + one `.text.tera` per template, embedded
//! at compile time from `templates/` — the mailer never reads
//! template files from disk at runtime, so a missing or moved
//! template directory cannot break a running worker. P1-029
//! owns the real copy (HTML + text, snapshot-tested) and will
//! add `.html.tera` parts; the v1 files here are minimal but
//! real.
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
/// problem. P1-029 adds the real copy for these same names.
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

/// A rendered mail: subject + plain-text body. HTML lands in
/// P1-029 (a `.html.tera` part per template).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMail {
    /// Single line — the subject file is trimmed on render.
    pub subject: String,
    /// Plain-text body, as rendered (trailing newline kept).
    pub text_body: String,
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
        Ok(RenderedMail {
            subject: subject.trim().to_string(),
            text_body,
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
