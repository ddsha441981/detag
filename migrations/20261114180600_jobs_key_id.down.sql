-- jobs_key_id (20261114180600, P1-038).
-- Down migration: exactly reverses the up migration.
DROP INDEX jobs_key_id_created_at_idx;
ALTER TABLE jobs DROP COLUMN key_id;
