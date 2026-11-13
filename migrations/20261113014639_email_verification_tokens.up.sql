-- email_verification_tokens (P1-002, E1.1). Single-use hashed
-- verification tokens (24h): register / resend mint one, the
-- verify endpoint consumes it.
--
-- Choices the data-model doc leaves open, recorded here:
-- - Only the sha256 of the token is stored (`token_hash`,
--   UNIQUE): the raw token exists in the outbox payload (so the
--   mail drain can build the link) and in the email itself, and
--   nowhere else. A leaked database reveals no usable token.
-- - `used_at` is set on consume (single-use); the table is NOT
--   append-only — consuming a token is an UPDATE by design, and
--   the audit trail for verification lives in `audit_logs`, not
--   here.
-- - Minting a new token deletes the user's previous UNUSED
--   tokens (same tx): one live token per user at a time, so a
--   resend invalidates the earlier link instead of leaving
--   several live tokens around.
-- - No `expires_at` index: lookup is by `token_hash` (unique);
--   expiry is checked on the fetched row. The
--   `(user_id, created_at)` index is for listing a user's
--   recent tokens (admin/debug). It does NOT serve the resend
--   rate limit: minting deletes previous unused tokens, so the
--   token table cannot count history — the resend limit counts
--   outbox rows instead (every mint enqueues exactly one
--   `verify-email` row in the same transaction; see
--   `email_outbox::count_recent_for_user`).

CREATE TABLE email_verification_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX email_verification_tokens_user_created_idx
    ON email_verification_tokens (user_id, created_at DESC);
