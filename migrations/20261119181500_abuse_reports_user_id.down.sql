DROP INDEX IF EXISTS abuse_reports_user_id_idx;
ALTER TABLE abuse_reports DROP COLUMN IF EXISTS user_id;
