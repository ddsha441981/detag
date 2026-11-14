-- jobs_key_id (20261114180600, P1-038).
-- Key-scoped usage attribution (FR-A-06): a job created
-- through an API key records the key that created it, so
-- usage can be attributed per key (P1-080's per-key
-- breakdown reads this column) and the admin jobs view can
-- show which key a job came from. NULL = created via the
-- web console / JWT (no key involved). ON DELETE SET NULL:
-- a deleted key must not take its jobs' history with it
-- (keys are never hard-deleted today, but the FK must not
-- block a future purge); the prefix shown in the admin view
-- comes from the live api_keys row, so a SET NULL row shows
-- no prefix rather than a stale one.
ALTER TABLE jobs ADD COLUMN key_id UUID NULL REFERENCES api_keys(id) ON DELETE SET NULL;
CREATE INDEX jobs_key_id_created_at_idx ON jobs(key_id, created_at DESC);
