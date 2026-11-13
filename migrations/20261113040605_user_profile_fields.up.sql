-- user_profile_fields (P1-005, E1.1). Profile fields for
-- PATCH /v1/me:
-- - `display_name`: the name shown on the profile (nullable —
--   unset until the user sets one; the GET response omits it
--   until set). TEXT, no uniqueness — many users may share a
--   name; identity is the email, not the display name.
-- - `pending_email`: the email-change INTENT (P1-005 records
--   it; P1-006 verifies + swaps). CITEXT like `email`, so the
--   duplicate check at intent time is case-insensitive, the
--   same as register. Nullable — no change pending. The swap
--   (P1-006) clears it when the new address is verified.
--   There is deliberately NO unique constraint here: two
--   users could intend the same address, and the swap is what
--   enforces uniqueness (the citext unique constraint on
--   `email` rejects the second swap). The intent-time
--   duplicate check in the profile service is a courtesy
--   (fail fast with 409), not the enforcement.

ALTER TABLE users ADD COLUMN display_name TEXT;
ALTER TABLE users ADD COLUMN pending_email CITEXT;
