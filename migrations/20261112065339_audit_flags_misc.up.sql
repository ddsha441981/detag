-- audit_logs + idempotency_keys + email_outbox + attestations +
-- abuse_reports + webhook_endpoints/deliveries + api_keys
-- (P0-040, E0.4). Per docs/03-ARCHITECTURE/04-data-model.md.
--
-- Choices the doc leaves open, recorded here:
-- - ids are UUID PRIMARY KEY DEFAULT gen_random_uuid().
-- - feature_flags is NOT created here: migration 0001 (P0-022)
--   already created it, and this migration must not recreate it.
-- - api_keys has no ticket of its own in Phase 0 or Phase 1, so it
--   lands here under the ticket's "misc": the data-model doc
--   specifies it, and Phase 1's API-key work (FR-P-01 Pro feature)
--   needs the table. Flagged in the P0-040 status note.
-- - audit_logs.target_id is TEXT, not UUID: targets include rows
--   with UUID keys, but also flag keys and other non-UUID targets.
-- - Append-only (audit_logs, job_events, attestations) is enforced
--   with a trigger that raises on UPDATE/DELETE — not with GRANTs:
--   this repo's dev database has a single role (pf) that owns the
--   tables, and an owner bypasses ACLs, so a REVOKE would be a
--   no-op here. The trigger binds every role including the owner.
--   True role separation (a dedicated app role without UPDATE /
--   DELETE on these tables) is a deployment concern for the
--   production database, not something this migration can do.
--   job_events append-only was deferred from P0-038 to this ticket.
-- - jobs.attestation_id gets its FK here (attestations now exists;
--   P0-038 left it a plain UUID on purpose), ON DELETE SET NULL.
-- - idempotency_keys.user_id is NULLABLE: anonymous requests can
--   carry idempotency keys. response is NOT NULL DEFAULT '{}'.
-- - email_outbox.to_hash stores only a hash of the recipient
--   (NFR-42); the worker drain index is a partial index on
--   unsent rows (sent_at IS NULL).
-- - attestations.ip_hash stores only a hash of the IP (NFR-42);
--   client is web|api_key (CHECK).
-- - webhook_deliveries.event_id is TEXT: deliveries reference
--   events by their string id, not by a single table's key.
-- - api_keys.prefix gets the doc's plain (non-unique) index.

CREATE TABLE audit_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    actor_client TEXT,
    action TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id TEXT,
    before JSONB,
    after JSONB,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE idempotency_keys (
    key TEXT PRIMARY KEY,
    user_id UUID REFERENCES users (id) ON DELETE CASCADE,
    endpoint TEXT NOT NULL,
    response JSONB NOT NULL DEFAULT '{}'::jsonb,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE email_outbox (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    to_hash TEXT,
    template TEXT NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ,
    attempts INT NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE attestations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    upload_id UUID NOT NULL REFERENCES uploads (id) ON DELETE CASCADE,
    text_version TEXT NOT NULL,
    ip_hash TEXT NOT NULL,
    client TEXT NOT NULL CHECK (client IN ('web', 'api_key')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE abuse_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    reporter_contact TEXT,
    job_id UUID REFERENCES jobs (id) ON DELETE SET NULL,
    reason TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'reviewing', 'resolved')),
    resolution TEXT,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE webhook_endpoints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    events TEXT[] NOT NULL DEFAULT '{}',
    disabled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    endpoint_id UUID NOT NULL REFERENCES webhook_endpoints (id) ON DELETE CASCADE,
    event_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    attempts INT NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_status TEXT,
    last_response_code INT,
    next_retry_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    prefix TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    scopes TEXT[] NOT NULL DEFAULT '{}',
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    rate_limit_tier TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The FK P0-038 deferred: jobs.attestation_id -> attestations.
ALTER TABLE jobs
    ADD CONSTRAINT jobs_attestation_id_fkey
    FOREIGN KEY (attestation_id) REFERENCES attestations (id)
    ON DELETE SET NULL;

-- Indexes from the data-model doc (minimum).
CREATE INDEX audit_logs_actor_created_idx
    ON audit_logs (actor_user_id, created_at DESC);
CREATE INDEX audit_logs_target_idx ON audit_logs (target_type, target_id);
CREATE INDEX idempotency_keys_expires_at_idx ON idempotency_keys (expires_at);
CREATE INDEX webhook_deliveries_endpoint_created_idx
    ON webhook_deliveries (endpoint_id, created_at DESC);
CREATE INDEX api_keys_prefix_idx ON api_keys (prefix);
-- Worker drain: unsent mail due now.
CREATE INDEX email_outbox_due_idx ON email_outbox (scheduled_at)
    WHERE sent_at IS NULL;

-- Append-only enforcement (see the header note): one function,
-- one trigger per append-only table.
CREATE OR REPLACE FUNCTION forbid_append_only_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'table % is append-only', TG_TABLE_NAME;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_logs_append_only
    BEFORE UPDATE OR DELETE ON audit_logs
    FOR EACH ROW EXECUTE FUNCTION forbid_append_only_mutation();
CREATE TRIGGER job_events_append_only
    BEFORE UPDATE OR DELETE ON job_events
    FOR EACH ROW EXECUTE FUNCTION forbid_append_only_mutation();
CREATE TRIGGER attestations_immutable
    BEFORE UPDATE OR DELETE ON attestations
    FOR EACH ROW EXECUTE FUNCTION forbid_append_only_mutation();
