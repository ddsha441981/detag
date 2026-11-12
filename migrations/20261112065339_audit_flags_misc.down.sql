-- audit_logs + idempotency_keys + email_outbox + attestations +
-- abuse_reports + webhook_endpoints/deliveries + api_keys
-- (P0-040, E0.4) — down migration. Exactly reverses the up
-- migration: triggers + function first, then the jobs FK, then the
-- tables in reverse dependency order. feature_flags (0001) is
-- untouched — it is not part of this migration.
--
-- The UPDATE first: dropping the attestations table would leave
-- dangling jobs.attestation_id values, and a later re-apply of the
-- up migration validates the FK against existing rows and fails.
DROP TRIGGER IF EXISTS attestations_immutable ON attestations;
DROP TRIGGER IF EXISTS job_events_append_only ON job_events;
DROP TRIGGER IF EXISTS audit_logs_append_only ON audit_logs;
DROP FUNCTION IF EXISTS forbid_append_only_mutation();
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_attestation_id_fkey;
UPDATE jobs SET attestation_id = NULL WHERE attestation_id IS NOT NULL;
DROP TABLE IF EXISTS api_keys;
DROP TABLE IF EXISTS webhook_deliveries;
DROP TABLE IF EXISTS webhook_endpoints;
DROP TABLE IF EXISTS abuse_reports;
DROP TABLE IF EXISTS attestations;
DROP TABLE IF EXISTS email_outbox;
DROP TABLE IF EXISTS idempotency_keys;
DROP TABLE IF EXISTS audit_logs;
