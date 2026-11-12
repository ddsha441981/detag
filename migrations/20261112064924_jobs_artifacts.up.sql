-- uploads + jobs + job_events + artifacts + quality_reports
-- (P0-038, E0.4). Per docs/03-ARCHITECTURE/04-data-model.md.
--
-- Choices the doc leaves open, recorded here:
-- - ids are UUID PRIMARY KEY DEFAULT gen_random_uuid().
-- - uploads.kind is image|video (CHECK); jobs.kind is
--   image|video|batch (CHECK, as the doc spells it out).
-- - uploads.status is pending_parts|ready|aborted (CHECK).
-- - uploads.duration_s is REAL, NULL for images (the doc's "?").
-- - uploads.parts is INT NOT NULL DEFAULT 1 (single-part upload);
--   multipart uploads count their parts.
-- - jobs.upload_id is NULLABLE: a batch job has no single upload
--   (the doc's "uploads 1-1 jobs" holds for single jobs only).
--   No UNIQUE on upload_id — a batch job's children share uploads.
-- - jobs.status / stage / mode are plain TEXT with no CHECK and no
--   default: the docs name only scattered values (queued, running,
--   succeeded, quality_failed in the API spec) and no complete state
--   machine. The machine is defined with the pipeline (Phase 3); a
--   CHECK can be added then as a contract step.
-- - jobs.progress is REAL 0.0-1.0 (CHECK); priority is INT >= 0,
--   DEFAULT 0; lane is TEXT NOT NULL with no default (lane names
--   like 'priority' come from plan entitlements, FR-P-01 — no
--   invented default value).
-- - jobs.attestation_id is a plain UUID for now: attestations lands
--   in P0-040, and the FK to it is added there.
-- - job_events is append-only by convention; no UPDATE/DELETE
--   grants are revoked here — grants land with the audit work.
-- - artifacts.expires_at is NOT NULL: every artifact expires at
--   now + plan TTL (retention rules in the data-model doc).
-- - quality_reports has UNIQUE(job_id): jobs 1-1 quality_reports.
-- - gate is pass|fail (CHECK), NOT NULL: a report is written only
--   after quality evaluation.
--
-- Indexes: the doc's minimum (jobs(user_id, created_at desc),
-- jobs(status, lane, priority), uploads(user_id, status)) plus
-- FK-column indexes the doc does not name but CASCADE deletes
-- need (jobs(upload_id), artifacts(job_id)) and the event-replay
-- index job_events(job_id, at).

CREATE TABLE uploads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('image', 'video')),
    filename TEXT NOT NULL,
    bytes BIGINT NOT NULL CHECK (bytes >= 0),
    sha256 TEXT NOT NULL,
    phash TEXT,
    mime TEXT NOT NULL,
    width INT CHECK (width IS NULL OR width > 0),
    height INT CHECK (height IS NULL OR height > 0),
    duration_s REAL CHECK (duration_s IS NULL OR duration_s >= 0),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL DEFAULT 'pending_parts'
        CHECK (status IN ('pending_parts', 'ready', 'aborted')),
    storage_key TEXT NOT NULL,
    parts INT NOT NULL DEFAULT 1 CHECK (parts >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    upload_id UUID REFERENCES uploads (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('image', 'video', 'batch')),
    mode TEXT NOT NULL,
    params JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL,
    stage TEXT,
    progress REAL NOT NULL DEFAULT 0
        CHECK (progress >= 0 AND progress <= 1),
    priority INT NOT NULL DEFAULT 0 CHECK (priority >= 0),
    lane TEXT NOT NULL,
    attempts INT NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    idempotency_key TEXT UNIQUE,
    error JSONB,
    attestation_id UUID,
    webhook_url TEXT,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE job_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    at TIMESTAMPTZ NOT NULL DEFAULT now(),
    type TEXT NOT NULL
        CHECK (type IN ('stage', 'progress', 'note', 'state_change')),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE artifacts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    type TEXT NOT NULL CHECK (type IN (
        'output', 'mask', 'preview_before', 'preview_after',
        'report', 'batch_zip'
    )),
    storage_key TEXT NOT NULL,
    bytes BIGINT NOT NULL CHECK (bytes >= 0),
    sha256 TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE quality_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL UNIQUE REFERENCES jobs (id) ON DELETE CASCADE,
    engine_version TEXT NOT NULL,
    models JSONB NOT NULL DEFAULT '{}'::jsonb,
    metrics JSONB NOT NULL DEFAULT '{}'::jsonb,
    encode JSONB NOT NULL DEFAULT '{}'::jsonb,
    regions JSONB NOT NULL DEFAULT '{}'::jsonb,
    gate TEXT NOT NULL CHECK (gate IN ('pass', 'fail')),
    retried BOOLEAN NOT NULL DEFAULT false,
    seed TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Indexes from the data-model doc (minimum).
CREATE INDEX jobs_user_created_idx ON jobs (user_id, created_at DESC);
CREATE INDEX jobs_status_lane_priority_idx ON jobs (status, lane, priority);
CREATE INDEX uploads_user_status_idx ON uploads (user_id, status);
-- FK columns (CASCADE deletes) + event replay order.
CREATE INDEX jobs_upload_id_idx ON jobs (upload_id);
CREATE INDEX job_events_job_at_idx ON job_events (job_id, at);
CREATE INDEX artifacts_job_id_idx ON artifacts (job_id);
