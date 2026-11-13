//! detag-mailer::error — typed mailer errors (P1-028).
//!
//! The drain distinguishes two failure kinds: transient (the
//! network / the relay said no — retry with backoff) and
//! permanent (the data is wrong — park the row). The
//! [`MailError::is_permanent`] split is that decision; the
//! drain never retries a permanent error.

use thiserror::Error;

/// A mailer failure.
#[derive(Debug, Error)]
pub enum MailError {
    /// No template with this name exists. The outbox names a
    /// fixed set (see [`crate::templates::TEMPLATES`]); anything
    /// else is a data error, never a relay problem.
    #[error("unknown email template '{0}'")]
    UnknownTemplate(String),

    /// Tera failed to render a template — a missing context
    /// variable (e.g. a `verify-email` payload without `token`)
    /// or a template syntax error. The payload is fixed, so a
    /// retry cannot help: permanent.
    #[error("template '{template}' failed to render: {source}")]
    Render {
        template: String,
        #[source]
        source: tera::Error,
    },

    /// A template file failed to load at engine build time
    /// (embedded at compile time, so this is a build bug, not
    /// a runtime condition).
    #[error("template '{template}' failed to load: {source}")]
    TemplateLoad {
        template: String,
        #[source]
        source: tera::Error,
    },

    /// An address (sender or recipient) does not parse as a
    /// mailbox. Deterministic — retrying cannot fix it.
    #[error("invalid email address '{0}'")]
    InvalidAddress(String),

    /// Building the lettre message or the transport failed
    /// (bad relay config, bad from address at build time).
    #[error("mailer configuration error: {0}")]
    Config(String),

    /// The SMTP relay refused the send (connection, auth, or a
    /// 4xx/5xx answer). Transient by default — the relay may be
    /// down or throttling; the drain retries with backoff.
    #[error("SMTP send failed: {0}")]
    Smtp(String),

    /// The outbox database call failed (claim, mark, record).
    /// Transient — the next drain pass retries.
    #[error("outbox database error: {0}")]
    Db(#[from] detag_core::repos::RepoError),
}

impl MailError {
    /// Permanent failures are data errors: retrying the same
    /// row cannot help, so the drain parks the row instead of
    /// scheduling another attempt. Everything else is transient
    /// (relay down, database hiccup) and gets the backoff.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            MailError::UnknownTemplate(_)
                | MailError::Render { .. }
                | MailError::TemplateLoad { .. }
                | MailError::InvalidAddress(_)
                | MailError::Config(_)
        )
    }
}
