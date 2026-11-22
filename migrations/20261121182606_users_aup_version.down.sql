-- Revert P1-095's AUP version column: consent state loses the
-- accepted AUP version (the audit rows survive the revert —
-- append-only — so past re-accepts stay provable from the trail).
ALTER TABLE users DROP COLUMN IF EXISTS aup_version;
