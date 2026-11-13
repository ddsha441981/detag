-- user_profile_fields (20261113040605)
-- Down migration: exactly reverses the up migration.

ALTER TABLE users DROP COLUMN IF EXISTS pending_email;
ALTER TABLE users DROP COLUMN IF EXISTS display_name;
