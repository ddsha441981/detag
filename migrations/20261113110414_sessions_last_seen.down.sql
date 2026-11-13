-- sessions_last_seen (20261113110414, P1-019).
-- Down migration: exactly reverses the up migration.
ALTER TABLE sessions DROP COLUMN last_seen;
