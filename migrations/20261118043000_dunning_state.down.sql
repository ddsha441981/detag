DROP INDEX IF EXISTS subscriptions_past_due_idx;
ALTER TABLE subscriptions
    DROP CONSTRAINT IF EXISTS subscriptions_dunning_step_ck,
    DROP CONSTRAINT IF EXISTS subscriptions_past_due_since_ck,
    DROP COLUMN IF EXISTS dunning_step,
    DROP COLUMN IF EXISTS past_due_since;
