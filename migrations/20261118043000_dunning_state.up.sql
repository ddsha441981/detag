-- P1-070 dunning state (ADR-0035): when a subscription entered
-- `past_due`, and which reminder step it has already been mailed.
--
-- `past_due_since` is stamped by the state machine's service on
-- the move INTO `past_due` (from the app clock) and cleared on
-- any move OUT of it, so recovery resets the sequence; a second
-- failed invoice while already past due is a same-state no-op,
-- so days count from the FIRST failure, never the latest.
-- `dunning_step` is the last reminder step mailed: 0 = the day-0
-- mail (the move itself), 3 = the day-3 reminder, 7 = the day-7
-- reminder. The dunning job sends a step only when the row's
-- step is below it, so a same-day re-run sends nothing twice.
--
-- The CHECK makes the invariant real at the DB: a row is
-- `past_due` exactly when it carries a `past_due_since`. The
-- backfill uses `updated_at`: `set_status_guarded` bumps it on
-- every status move, so for a past_due row it IS the entry time.
ALTER TABLE subscriptions
    ADD COLUMN past_due_since TIMESTAMPTZ,
    ADD COLUMN dunning_step SMALLINT NOT NULL DEFAULT 0,
    ADD CONSTRAINT subscriptions_past_due_since_ck
        CHECK ((status = 'past_due') = (past_due_since IS NOT NULL)),
    ADD CONSTRAINT subscriptions_dunning_step_ck
        CHECK (dunning_step IN (0, 3, 7));

UPDATE subscriptions
   SET past_due_since = updated_at
 WHERE status = 'past_due';

-- The dunning job's scan: past_due rows ordered by entry time.
CREATE INDEX subscriptions_past_due_idx
    ON subscriptions (past_due_since)
    WHERE status = 'past_due';
