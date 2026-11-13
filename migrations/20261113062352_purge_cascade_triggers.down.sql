-- purge_cascade_triggers (20261113062352, P1-007)
-- Down migration: exactly reverses the up migration.
--
-- Restores the audit_logs actor FK (ON DELETE SET NULL) and
-- the DELETE branch of the job_events / attestations triggers.
-- NOTE: the FK is re-added NOT VALID — a plain re-add would
-- fail validation against audit rows whose actor was purged
-- (the up migration deliberately leaves those ids in place).
-- NOT VALID constrains future inserts only; existing rows are
-- not checked. A purge run on the reversed schema fails again
-- for any user with audit rows — the up migration exists
-- because the purge needs it.

ALTER TABLE audit_logs
    ADD CONSTRAINT audit_logs_actor_user_id_fkey
    FOREIGN KEY (actor_user_id) REFERENCES users (id) ON DELETE SET NULL
    NOT VALID;

DROP TRIGGER job_events_append_only ON job_events;
CREATE TRIGGER job_events_append_only
    BEFORE UPDATE OR DELETE ON job_events
    FOR EACH ROW EXECUTE FUNCTION forbid_append_only_mutation();

DROP TRIGGER attestations_immutable ON attestations;
CREATE TRIGGER attestations_immutable
    BEFORE UPDATE OR DELETE ON attestations
    FOR EACH ROW EXECUTE FUNCTION forbid_append_only_mutation();
