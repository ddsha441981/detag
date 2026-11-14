//! Detag mailer: SMTP + template rendering + outbox drain (P1-028).
//!
//! The api never sends mail directly: it enqueues rows in
//! `email_outbox` (see `detag_core::repos::email_outbox`) and
//! this crate's drain — run by the worker — claims due rows,
//! renders the named template and sends the mail over SMTP
//! (lettre). Retries use exponential backoff; a row that
//! exhausts its attempts, or that can never be sent (unknown
//! template, unresolvable recipient), is parked, never deleted.
//!
//! Templates live in `templates/` (one `.subject.tera` + one
//! `.text.tera` per template, embedded at compile time). P1-029
//! owns the real copy (HTML + text, snapshot-tested); the v1
//! files here are minimal but real — every template the outbox
//! names must render, or the drain would park every queued mail.

pub mod drain;
pub mod error;
pub mod rate_limit;
pub mod smtp;
pub mod templates;

pub use drain::{backoff_delay_secs, drain_once, DrainConfig, DrainOutcome};
pub use error::MailError;
pub use rate_limit::{
    is_over_cap, template_class, templates_for_class, MailClass, RateLimitConfig,
    RATE_LIMIT_WINDOW_SECS,
};
pub use smtp::{LettreTransport, MailTransport, MemoryTransport, OutgoingMail, SmtpConfig};
pub use templates::{RenderedMail, TemplateEngine, TEMPLATES};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
