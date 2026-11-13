-- login_attempt_counters (P1-004, E1.1). Durable failed-login
-- counters + lockout state, one row per (scope, key):
-- scope 'account' keyed by the sha256 of the email, scope 'ip'
-- keyed by the sha256 of the client IP.
--
-- Why a table, not Redis: P1-003 promised the lockout as the
-- durable backstop that survives a Redis outage — the
-- per-account rate-limit bucket (Redis) is the cheap first
-- line; this table is the lockout of record. A Redis flush
-- must never silently unlock an account under attack.
--
-- Design (see `detag_core::lockout` for the curve, and the
-- repo `login_attempts.rs` for the atomic record):
-- - `attempts` counts failures since the last success (a
--   success deletes the row) or since the window reset (a
--   failure more than LOCKOUT_WINDOW_SECS after the previous
--   one starts a new window at 1).
-- - At LOCKOUT_THRESHOLD failures the row locks:
--   `locked_until = now() + lock_seconds(attempts)` — the
--   lock doubles per further failure, capped at
--   LOCKOUT_MAX_SECS. The lock is set with GREATEST against
--   the existing value, so concurrent failures can only ever
--   extend it, never shorten it.
-- - While locked, login attempts are denied at the lock check
--   (before the password verify) and are NOT recorded — a
--   locked account cannot be kept locked forever by hammering
--   it. (The race window is real: attempts that passed the
--   check before the lock was set still record, and extend it
--   — that is correct, they were genuine attempts.)
-- - `notified_at` records when the lockout notification mail
--   was queued (once per lock episode — the first lock, when
--   attempts first reaches the threshold; extensions do not
--   re-notify).
-- - Rows are never deleted by the login path except on success
--   (reset). Stale rows (expired lock, old window) are inert —
--   the next failure resets them — and bounded by the number
--   of distinct attacked keys; a GC sweep is a later ticket,
--   not this one.

CREATE TABLE login_attempt_counters (
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    attempts INT NOT NULL DEFAULT 0,
    first_failed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_failed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_until TIMESTAMPTZ,
    notified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, key),
    CONSTRAINT login_attempt_counters_scope_check
        CHECK (scope IN ('account', 'ip'))
);
