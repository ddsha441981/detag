-- api_keys_expires_at (20261114131820, P1-035).
-- Down migration: exactly reverses the up migration.
ALTER TABLE api_keys DROP COLUMN expires_at;
