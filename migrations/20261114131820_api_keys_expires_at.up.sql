-- api_keys_expires_at (20261114131820, P1-035).
-- Rotation (POST /v1/api-keys/{id}/rotate) mints a new row and
-- keeps the old secret valid for the overlap window (24 h by
-- default, PF_API_KEY_ROTATION_OVERLAP_SECS): the old row
-- carries expires_at = rotated_at + overlap, and the P1-034
-- extractor treats a row past expires_at as dead (401 PF-1001,
-- exactly like revoked/unknown). NULL = never expires (every
-- pre-P1-035 row and every fresh mint). The column is advisory
-- until the extractor reads it — see repos/api_keys.rs.
ALTER TABLE api_keys ADD COLUMN expires_at TIMESTAMPTZ NULL;
