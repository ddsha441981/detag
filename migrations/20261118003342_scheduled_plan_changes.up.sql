-- scheduled_plan_changes (20261118003342)
-- Up migration.
-- A plan change that waits for a period boundary (P1-069,
-- FR-P-03). Per docs/03-ARCHITECTURE/04-data-model.md.
--
-- Choices the doc leaves open, recorded here:
-- - A downgrade never calls the provider at request time: the
--   row is the durable promise ("at `effective_at` this
--   subscription is on `to_plan_id`"), and the webhook handler
--   applies it when the provider's period rolls past it
--   (`apps/api/src/services/plan_changes.rs`). Nothing is
--   charged now (credit/charge are both zero in the preview),
--   so there is no invoice to reconcile at schedule time.
-- - One pending change per subscription: the partial UNIQUE
--   index (`status = 'scheduled'`) makes a second downgrade
--   request 409 — a change is replaced by canceling it, and
--   there is no cancel endpoint yet (P1-071 owns cancel
--   policy); until then a pending change can only be
--   superseded by its own application or by the subscription
--   ending, which marks it `canceled`.
-- - `status` is `scheduled` → `applied` (the applier flips
--   the subscription row + this row in one transaction) or
--   `scheduled` → `canceled` (the subscription ended before
--   the change came due, or the row was superseded). Rows are
--   never deleted — the audit trail needs them, like
--   `subscriptions` and `webhook_events`.
-- - `from_plan_id` / `to_plan_id` are `ON DELETE RESTRICT`
--   like `subscriptions.plan_id` (P1-056): a plan with a
--   pending change cannot be removed from the catalog.
-- - No expiry / GC here: a scheduled change is either
--   applied at the next period roll or mooted by the
--   subscription ending; both paths close the row.

CREATE TABLE scheduled_plan_changes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    subscription_id UUID NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    from_plan_id UUID NOT NULL REFERENCES plans(id) ON DELETE RESTRICT,
    to_plan_id UUID NOT NULL REFERENCES plans(id) ON DELETE RESTRICT,
    effective_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'applied', 'canceled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX scheduled_plan_changes_one_pending_idx
    ON scheduled_plan_changes (subscription_id) WHERE status = 'scheduled';

CREATE INDEX scheduled_plan_changes_due_idx
    ON scheduled_plan_changes (effective_at) WHERE status = 'scheduled';
