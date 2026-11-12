//! Seed task (P0-043): idempotent seed data — plans per FR-P-01 and
//! the known feature flags, all OFF (static defaults).
//!
//! The SQL lives in `scripts/seed.sql` (single source; the `just
//! seed` example and the live test both run this function) and is
//! idempotent by construction: every INSERT is `ON CONFLICT DO
//! NOTHING`, so re-running never clobbers an operator/admin edit.
//! The admin invite is deliberately NOT seeded here — no invites
//! table or invite flow exists in the kit, and the runbook's admin
//! mechanism is the `pf-admin-create` CLI, which has no owning
//! ticket yet (DECISIONS.md #8, owner-input-needed).

use crate::db::DbPool;

/// What `seed` found after running: row counts, so callers can
/// report (and tests can assert) the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedReport {
    /// Rows in `plans` after seeding.
    pub plans: i64,
    /// Rows in `feature_flags` after seeding.
    pub flags: i64,
}

/// Run the seed SQL (`scripts/seed.sql`) against `pool` and return
/// the row counts. Safe to run twice: the second run changes
/// nothing. Requires migrations 0001 (feature_flags) and 003
/// (plans) to be applied — call after `migrate_up`.
pub async fn seed(pool: &DbPool) -> Result<SeedReport, sqlx::Error> {
    sqlx::raw_sql(include_str!("../../../scripts/seed.sql"))
        .execute(pool)
        .await?;
    let plans = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plans")
        .fetch_one(pool)
        .await?;
    let flags = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM feature_flags")
        .fetch_one(pool)
        .await?;
    Ok(SeedReport { plans, flags })
}
