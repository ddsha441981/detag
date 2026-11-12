-- users + sessions (P0-037, E0.4).
-- Per docs/03-ARCHITECTURE/04-data-model.md: users (email citext
-- unique, password_hash argon2id, email_verified_at, role, status,
-- tos_version, deleted_at grace, settings jsonb) and sessions
-- (user_id, refresh_hash sha256, family_id rotation lineage, device
-- ua + ip-hashed, expires_at, revoked_at, replaced_by).
--
-- Choices the doc leaves open, recorded here:
-- - ids are UUID PRIMARY KEY DEFAULT gen_random_uuid().
-- - role/status are TEXT with CHECK constraints (not Postgres enums:
--   enums are painful to extend in later migrations).
-- - tos_version is TEXT (version strings like 'v1'); NULL means the
--   user has not accepted any ToS version yet.
-- - device is one JSONB column {"ua": "...", "ip_hash": "..."}: the
--   doc's "device (ua, ip-hashed)" is a single structured value, and
--   the IP is stored ONLY as a hash (NFR-42: no raw IPs in the DB).
-- - family_id is UUID NOT NULL: the app sets it at login (a rotation
--   family starts with the first refresh token); it has no default so
--   a missing family is a bug, not silent data.
-- - replaced_by self-references sessions(id): a revoked session
--   points at its replacement; ON DELETE SET NULL.
-- - sessions are deleted with the user (ON DELETE CASCADE); a
--   revoked session is kept (revoked_at) for audit, not deleted.
-- - citext needs its extension; IF NOT EXISTS so the migration is
--   idempotent and re-runnable on a DB that already has it.

CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email CITEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    email_verified_at TIMESTAMPTZ,
    role TEXT NOT NULL DEFAULT 'user'
        CHECK (role IN ('user', 'support', 'admin')),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'suspended', 'deletion_pending')),
    tos_version TEXT,
    deleted_at TIMESTAMPTZ,
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    refresh_hash TEXT NOT NULL,
    family_id UUID NOT NULL,
    device JSONB NOT NULL DEFAULT '{}'::jsonb,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    replaced_by UUID REFERENCES sessions (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Indexes (data-model doc, minimum): sessions(refresh_hash) unique.
-- Plus sessions(user_id): revoke-all-sessions (password reset,
-- logout everywhere) and "list my sessions" both filter by user.
CREATE UNIQUE INDEX sessions_refresh_hash_idx ON sessions (refresh_hash);
CREATE INDEX sessions_user_id_idx ON sessions (user_id);
