//! Proration math (P1-069, FR-P-03): the pure arithmetic behind
//! upgrade/downgrade timing. No I/O, no provider — a wrong
//! number here is a unit test, never a live charge.
//!
//! # The model
//!
//! A plan change is classified by comparing the catalog
//! amounts (paise, `plans.amount`): a bigger new amount is an
//! **upgrade**, a smaller one a **downgrade**, an equal one is
//! already-subscribed (the service answers 422 before any math
//! runs).
//!
//! * **Upgrade** takes effect **now**: the buyer pays for the
//!   new plan from this moment, and the unused remainder of
//!   the old plan is credited back. The preview is the two
//!   legs of that exchange, each a straight time fraction of
//!   the current period:
//!   `credit = old_amount * remaining / period`,
//!   `charge = new_amount * remaining / period`,
//!   `net = charge - credit` (never negative for an upgrade,
//!   since `new > old` and both legs share the same fraction).
//!   The checkout session charges the new plan's price at the
//!   provider; the preview is what the buyer is shown, and
//!   the provider's invoice is authoritative — our numbers
//!   exist so the timing is visible and testable, not so we
//!   re-implement Stripe's invoice.
//! * **Downgrade** takes effect at the **period end**:
//!   nothing is charged or credited now (all three legs are
//!   zero), the buyer keeps the old plan until
//!   `effective_at`, and the durable `scheduled_plan_changes`
//!   row is the promise. The entitlement switch lands with
//!   the applier (`apps/api/src/services/plan_changes.rs`),
//!   not with this module.
//!
//! # The period
//!
//! The `subscriptions` row stores only `current_period_end`
//! — no start. The start is derived (ASSUMPTION-032): all
//! catalog plans are monthly, so the period started one
//! calendar month before its end
//! ([`period_start_from_end`]). The fraction is computed in
//! whole seconds; both legs truncate toward zero (amounts are
//! non-negative, so this is a floor — at most one paise of
//! rounding per leg, always in the buyer's favour on the
//! charge leg and in ours on the credit leg, and the preview
//! says so rather than hiding it).
//!
//! Edge clamping (a stale row whose period already ended, or
//! a clock that jumped): `remaining` is clamped to
//! `[0, period]` — a period with no time left previews
//! zero, never a negative. A row with no period end at all
//! has no period to prorate: [`preview_no_period`] is the
//! zero preview for it (upgrade effective now, downgrade
//! effective now — there is no boundary to wait for).

use chrono::{DateTime, Duration, Months, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which way a plan change moves, by catalog amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The new plan costs more: effective now, with a preview.
    Upgrade,
    /// The new plan costs less: scheduled at the period end.
    Downgrade,
    /// The same amount: already subscribed (the service 422s).
    Same,
}

/// Classify a change by the two catalog amounts (paise).
#[must_use]
pub fn classify(old_amount_paise: i32, new_amount_paise: i32) -> ChangeKind {
    match new_amount_paise.cmp(&old_amount_paise) {
        std::cmp::Ordering::Greater => ChangeKind::Upgrade,
        std::cmp::Ordering::Less => ChangeKind::Downgrade,
        std::cmp::Ordering::Equal => ChangeKind::Same,
    }
}

/// What a plan change costs, and when it takes effect. The
/// wire shape of the checkout response's `proration` field
/// (and of the downgrade 202 body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProrationPreview {
    /// Which way the change moves.
    pub kind: ChangeKind,
    /// Unused old-plan value returned to the buyer (paise).
    /// Zero for a downgrade — nothing moves now.
    pub credit_paise: i64,
    /// New-plan value for the remainder (paise). Zero for a
    /// downgrade — nothing moves now.
    pub charge_paise: i64,
    /// `charge - credit`: what the change costs beyond the
    /// plan price the checkout already collects. Never
    /// negative for an upgrade; zero for a downgrade.
    pub net_paise: i64,
    /// When the new plan takes effect: now for an upgrade,
    /// the period end for a downgrade.
    pub effective_at: DateTime<Utc>,
}

/// Derive a period's start from its end: one calendar month
/// earlier (all catalog plans are monthly, ASSUMPTION-032).
/// A month with no matching day clamps to its last day
/// (31 March → 28/29 February); a date chrono cannot shift
/// falls back to 30 days, so the function is total.
#[must_use]
pub fn period_start_from_end(period_end: DateTime<Utc>) -> DateTime<Utc> {
    period_end
        .checked_sub_months(Months::new(1))
        .unwrap_or(period_end - Duration::days(30))
}

/// The zero preview: no period to prorate (the subscription
/// row carries no `current_period_end`). An upgrade is still
/// effective now; a downgrade has no boundary to wait for,
/// so it is effective now too.
#[must_use]
pub fn preview_no_period(kind: ChangeKind, now: DateTime<Utc>) -> ProrationPreview {
    ProrationPreview {
        kind,
        credit_paise: 0,
        charge_paise: 0,
        net_paise: 0,
        effective_at: now,
    }
}

/// The preview for a change inside a known period
/// (`period_start`..=`period_end`, both from the
/// subscription row — the start via
/// [`period_start_from_end`]). `now` is the frozen-or-real
/// clock the caller passes; the fraction is
/// `remaining / total` in whole seconds, clamped to
/// `[0, 1]`.
#[must_use]
pub fn preview(
    old_amount_paise: i32,
    new_amount_paise: i32,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    now: DateTime<Utc>,
) -> ProrationPreview {
    let kind = classify(old_amount_paise, new_amount_paise);
    match kind {
        ChangeKind::Downgrade => ProrationPreview {
            kind,
            credit_paise: 0,
            charge_paise: 0,
            net_paise: 0,
            effective_at: period_end,
        },
        ChangeKind::Same => ProrationPreview {
            kind,
            credit_paise: 0,
            charge_paise: 0,
            net_paise: 0,
            effective_at: now,
        },
        ChangeKind::Upgrade => {
            let total = (period_end - period_start).num_seconds().max(0);
            let remaining = (period_end - now).num_seconds().clamp(0, total);
            let credit = prorate(old_amount_paise, remaining, total);
            let charge = prorate(new_amount_paise, remaining, total);
            ProrationPreview {
                kind,
                credit_paise: credit,
                charge_paise: charge,
                net_paise: charge - credit,
                effective_at: now,
            }
        }
    }
}

/// One leg: `amount * remaining / total` in paise, truncated
/// toward zero (amounts are non-negative, so a floor). A
/// zero-length period prorates nothing — never a divide by
/// zero.
fn prorate(amount_paise: i32, remaining_secs: i64, total_secs: i64) -> i64 {
    if total_secs <= 0 || remaining_secs <= 0 {
        return 0;
    }
    (i64::from(amount_paise) * remaining_secs) / total_secs
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn classify_compares_amounts() {
        assert_eq!(classify(49_900, 199_900), ChangeKind::Upgrade);
        assert_eq!(classify(199_900, 49_900), ChangeKind::Downgrade);
        assert_eq!(classify(49_900, 49_900), ChangeKind::Same);
        // Free (0) to a paid plan is an upgrade; a paid plan
        // to Free is a downgrade.
        assert_eq!(classify(0, 49_900), ChangeKind::Upgrade);
        assert_eq!(classify(49_900, 0), ChangeKind::Downgrade);
    }

    #[test]
    fn upgrade_mid_period_splits_both_legs() {
        // Pro 49900 → Business 199900, halfway through a
        // 30-day period: exactly half of each leg.
        let start = utc(2026, 11, 3, 0, 0, 0);
        let end = utc(2026, 12, 3, 0, 0, 0);
        let now = utc(2026, 11, 18, 0, 0, 0);
        let p = preview(49_900, 199_900, start, end, now);
        assert_eq!(p.kind, ChangeKind::Upgrade);
        assert_eq!(p.credit_paise, 24_950);
        assert_eq!(p.charge_paise, 99_950);
        assert_eq!(p.net_paise, 75_000);
        assert_eq!(p.effective_at, now);
    }

    #[test]
    fn upgrade_at_period_start_is_full_and_at_end_is_zero() {
        let start = utc(2026, 11, 3, 0, 0, 0);
        let end = utc(2026, 12, 3, 0, 0, 0);
        let full = preview(49_900, 199_900, start, end, start);
        assert_eq!((full.credit_paise, full.charge_paise), (49_900, 199_900));
        assert_eq!(full.net_paise, 150_000);
        let spent = preview(49_900, 199_900, start, end, end);
        assert_eq!((spent.credit_paise, spent.charge_paise), (0, 0));
        assert_eq!(spent.net_paise, 0);
        // Past the end clamps to zero too — never negative.
        let late = preview(49_900, 199_900, start, end, end + Duration::days(5));
        assert_eq!((late.credit_paise, late.charge_paise), (0, 0));
    }

    #[test]
    fn upgrade_rounds_down_each_leg() {
        // 49900 * 7 / 30 = 11643.33… → 11643; the truncation
        // is documented in the module docs.
        let start = utc(2026, 11, 3, 0, 0, 0);
        let end = utc(2026, 12, 3, 0, 0, 0);
        let now = utc(2026, 11, 26, 0, 0, 0);
        let p = preview(49_900, 199_900, start, end, now);
        assert_eq!(p.credit_paise, 11_643);
        assert_eq!(p.charge_paise, 46_643);
        assert_eq!(p.net_paise, 35_000);
    }

    #[test]
    fn downgrade_is_zero_now_and_effective_at_period_end() {
        let start = utc(2026, 11, 3, 0, 0, 0);
        let end = utc(2026, 12, 3, 0, 0, 0);
        let now = utc(2026, 11, 18, 0, 0, 0);
        let p = preview(199_900, 49_900, start, end, now);
        assert_eq!(p.kind, ChangeKind::Downgrade);
        assert_eq!((p.credit_paise, p.charge_paise, p.net_paise), (0, 0, 0));
        assert_eq!(p.effective_at, end);
        // Downgrade to Free: same shape, nothing moves now.
        let to_free = preview(49_900, 0, start, end, now);
        assert_eq!(to_free.kind, ChangeKind::Downgrade);
        assert_eq!(to_free.effective_at, end);
    }

    #[test]
    fn same_plan_previews_zero_effective_now() {
        let start = utc(2026, 11, 3, 0, 0, 0);
        let end = utc(2026, 12, 3, 0, 0, 0);
        let now = utc(2026, 11, 18, 0, 0, 0);
        let p = preview(49_900, 49_900, start, end, now);
        assert_eq!(p.kind, ChangeKind::Same);
        assert_eq!((p.credit_paise, p.charge_paise, p.net_paise), (0, 0, 0));
        assert_eq!(p.effective_at, now);
    }

    #[test]
    fn period_start_is_one_calendar_month_before_end() {
        assert_eq!(
            period_start_from_end(utc(2026, 12, 3, 0, 0, 0)),
            utc(2026, 11, 3, 0, 0, 0)
        );
        // A month with no 31st clamps to its last day: 31 March
        // → 28 February (2026 is not a leap year).
        assert_eq!(
            period_start_from_end(utc(2026, 3, 31, 0, 0, 0)),
            utc(2026, 2, 28, 0, 0, 0)
        );
    }

    #[test]
    fn no_period_previews_zero() {
        let now = utc(2026, 11, 18, 0, 0, 0);
        let up = preview_no_period(ChangeKind::Upgrade, now);
        assert_eq!((up.credit_paise, up.charge_paise), (0, 0));
        assert_eq!(up.effective_at, now);
        let down = preview_no_period(ChangeKind::Downgrade, now);
        assert_eq!(down.effective_at, now);
    }
}
