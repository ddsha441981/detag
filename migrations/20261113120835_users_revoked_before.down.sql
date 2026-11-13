-- users_revoked_before (20261113120835)
-- Down migration: exactly reverses the up migration.
ALTER TABLE users DROP COLUMN revoked_before;
