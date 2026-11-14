//! Per-recipient hourly send caps per template class (P1-031).
//!
//! The drain is the choke point: every mail the api enqueues —
//! register, resend, forgot, email-change, lockout, any future
//! enqueue site — passes through `drain_once`, so one check
//! here covers all of them. The cap is on SENDS per recipient
//! address per hour: the drain counts sent outbox rows for the
//! recipient's hash + the class's templates in the last hour
//! (`email_outbox::count_sent_recent_for_recipient`)
//! and parks the row when the count reaches the cap. A parked
//! row is dropped, not retried into the next hour — its token
//! would be stale by then (verify tokens live 24 h but only
//! the latest mint works; reset tokens 30 min), and the caller
//! can request a fresh mail after the hour. The trip also
//! writes an abuse audit row (`email.rate_limit_tripped`).
//!
//! Classes (spec: verify 3, reset 3, marketing 0):
//!
//! * Verify — `verify-email` + `email-change-verify`: both are
//!   verification mails to a recipient address. The change mail
//!   goes to the NEW address, and its `to_hash` is that
//!   address's hash, so the two share one per-address budget
//!   exactly as the spec's "verify 3" intends.
//! * Reset — `password-reset`.
//! * Marketing — `marketing-newsletter`, reserved: no marketing
//!   template ships in v1 (transactional-only stance, P1-032),
//!   so the class is empty in practice and the 0 cap means any
//!   marketing-class row is always parked. If a marketing
//!   template is ever added to the engine it stays blocked
//!   until the operator deliberately raises the cap.
//! * Transactional — everything else (`email-change-alert`,
//!   `lockout-notice`, `new-login-digest`,
//!   `refresh-reuse-alert`, `account-deletion`): security and
//!   account alerts are never capped — a lockout notice must
//!   reach the user precisely when someone is hammering the
//!   account.
//!
//! The caps are operator config (`PF_MAILER_*_CAP_PER_HOUR`,
//! defaults 3 / 3 / 0). This is a backstop, not the only limit:
//! the endpoints already rate-limit their own flows
//! (resend-verification 3/hour per user+template, P1-002;
//! forgot-password 10 burst / 1 per min per account, P1-025).
//! The drain cap catches what those miss — e.g. register
//! re-attempts + resends + an email change combining into more
//! than 3 verification mails to one address in an hour.

use detag_core::config::Settings;

/// One hour, in seconds — the cap window for every class.
pub const RATE_LIMIT_WINDOW_SECS: i64 = 3600;

/// A template's send-cap class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailClass {
    /// Verification mails (`verify-email`, `email-change-verify`).
    Verify,
    /// Password-reset mails (`password-reset`).
    Reset,
    /// Marketing mails (reserved — none ships in v1).
    Marketing,
    /// Security / account alerts — never capped.
    Transactional,
}

impl MailClass {
    /// Short name for audit rows and log fields.
    pub fn as_str(self) -> &'static str {
        match self {
            MailClass::Verify => "verify",
            MailClass::Reset => "reset",
            MailClass::Marketing => "marketing",
            MailClass::Transactional => "transactional",
        }
    }
}

/// The class of one template name. Unknown names are
/// transactional: an unknown template never reaches the send
/// (the drain parks it as `UnknownTemplate`), so capping it
/// would only hide the real error.
pub fn template_class(template: &str) -> MailClass {
    match template {
        "verify-email" | "email-change-verify" => MailClass::Verify,
        "password-reset" => MailClass::Reset,
        "marketing-newsletter" => MailClass::Marketing,
        _ => MailClass::Transactional,
    }
}

/// Every template in a class — the set the sent-count query
/// matches. Transactional has none: it is uncapped, so there
/// is nothing to count.
pub fn templates_for_class(class: MailClass) -> &'static [&'static str] {
    match class {
        MailClass::Verify => &["verify-email", "email-change-verify"],
        MailClass::Reset => &["password-reset"],
        MailClass::Marketing => &["marketing-newsletter"],
        MailClass::Transactional => &[],
    }
}

/// Per-class hourly send caps, from `Settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// `PF_MAILER_VERIFY_CAP_PER_HOUR` — default 3.
    pub verify_cap_per_hour: u32,
    /// `PF_MAILER_RESET_CAP_PER_HOUR` — default 3.
    pub reset_cap_per_hour: u32,
    /// `PF_MAILER_MARKETING_CAP_PER_HOUR` — default 0: no
    /// marketing mail is ever sent in v1.
    pub marketing_cap_per_hour: u32,
}

impl RateLimitConfig {
    /// From loaded settings (P1-031).
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            verify_cap_per_hour: settings.mailer_verify_cap_per_hour,
            reset_cap_per_hour: settings.mailer_reset_cap_per_hour,
            marketing_cap_per_hour: settings.mailer_marketing_cap_per_hour,
        }
    }

    /// The cap for a class, or `None` when the class is
    /// uncapped (transactional alerts).
    pub fn cap_for_class(self, class: MailClass) -> Option<u32> {
        match class {
            MailClass::Verify => Some(self.verify_cap_per_hour),
            MailClass::Reset => Some(self.reset_cap_per_hour),
            MailClass::Marketing => Some(self.marketing_cap_per_hour),
            MailClass::Transactional => None,
        }
    }
}

/// Is `sent_in_window` already at (or past) the cap? A cap of
/// 0 blocks everything — that is the marketing stance, not a
/// bug: `is_over_cap(0, 0)` is true.
pub fn is_over_cap(sent_in_window: i64, cap: u32) -> bool {
    sent_in_window >= i64::from(cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::TEMPLATES;

    #[test]
    fn every_shipped_template_maps_to_a_class() {
        // The mapping must stay total over TEMPLATES: a shipped
        // template silently falling into Transactional would
        // dodge its cap. Transactional is uncapped, so it has
        // no template list to count — the check below skips it
        // (the `alerts_are_transactional_and_uncapped` test
        // pins the transactional set instead).
        for template in TEMPLATES {
            let class = template_class(template);
            if class == MailClass::Transactional {
                continue;
            }
            assert!(
                templates_for_class(class).contains(template),
                "{template} maps to {class:?} but is not in that class's template list"
            );
        }
    }

    #[test]
    fn class_lists_round_trip() {
        for class in [MailClass::Verify, MailClass::Reset, MailClass::Marketing] {
            for template in templates_for_class(class) {
                assert_eq!(template_class(template), class);
            }
        }
    }

    #[test]
    fn verify_class_is_both_verification_templates() {
        assert_eq!(template_class("verify-email"), MailClass::Verify);
        assert_eq!(template_class("email-change-verify"), MailClass::Verify);
        assert_eq!(template_class("password-reset"), MailClass::Reset);
        assert_eq!(template_class("marketing-newsletter"), MailClass::Marketing);
    }

    #[test]
    fn alerts_are_transactional_and_uncapped() {
        for template in [
            "email-change-alert",
            "lockout-notice",
            "new-login-digest",
            "refresh-reuse-alert",
            "account-deletion",
        ] {
            assert_eq!(template_class(template), MailClass::Transactional);
        }
        assert_eq!(
            RateLimitConfig {
                verify_cap_per_hour: 3,
                reset_cap_per_hour: 3,
                marketing_cap_per_hour: 0,
            }
            .cap_for_class(MailClass::Transactional),
            None,
            "alerts never get a cap"
        );
    }

    #[test]
    fn cap_zero_blocks_everything() {
        assert!(is_over_cap(0, 0), "cap 0: even the first mail trips");
        assert!(is_over_cap(5, 0));
    }

    #[test]
    fn cap_boundary_is_at_the_cap_not_past_it() {
        assert!(!is_over_cap(2, 3), "2 of 3 sent: the 3rd still goes");
        assert!(is_over_cap(3, 3), "3 of 3 sent: the 4th trips");
        assert!(is_over_cap(4, 3));
    }

    #[test]
    fn defaults_are_verify_3_reset_3_marketing_0() {
        let settings = Settings::default();
        let config = RateLimitConfig::from_settings(&settings);
        assert_eq!(config.verify_cap_per_hour, 3);
        assert_eq!(config.reset_cap_per_hour, 3);
        assert_eq!(config.marketing_cap_per_hour, 0);
    }
}
