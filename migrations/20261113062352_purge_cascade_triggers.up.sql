-- purge_cascade_triggers (20261113062352, P1-007)
--
-- The account purge (DELETE FROM users, 30 days after the
-- request) could not delete any user with history: the
-- append-only triggers + the audit FK conspired to forbid
-- every path the cascade needs. The purge test proved it
-- (purge_one failed with "table audit_logs is append-only").
-- Three corrections, each keeping the original intent:
--
-- 1. audit_logs.actor_user_id loses its FK (the column stays
--    a plain UUID). The actor is a historical fact — who did
--    it — not a live reference (target_id is already a plain
--    TEXT for the same reason). The FK's ON DELETE SET NULL
--    is an UPDATE on audit_logs, which the append-only trigger
--    forbids — so no user with any audit row could ever be
--    purged. Without the FK the purge leaves the actor id
--    intact in the row: a STRONGER trail than NULL (the actor
--    is still known), and no UPDATE happens at all.
--
-- 2. job_events_append_only and attestations_immutable become
--    UPDATE-only triggers. The DELETE branch forbade the purge
--    cascade: deleting a user deletes their jobs, which must
--    delete the job's events (NOT NULL FK, ON DELETE CASCADE),
--    and their attestations (user_id NOT NULL, CASCADE).
--    "Immutable" means the record can never be ALTERED — that
--    guarantee survives (UPDATE still raises). DELETE is the
--    purge's erasure path: keeping those rows would retain the
--    user's data after erasure, and no app code deletes them
--    directly (the only DELETE path is the FK cascade from a
--    user purge). The audit_logs trigger keeps its DELETE
--    branch: the audit trail is never deleted — the purge
--    keeps audit rows by design.
--
-- The P0-038 migration test asserted the old contract (DELETE
-- forbidden on all three tables); it is updated with this
-- migration to the new contract.

-- 1. The audit actor is a historical fact, not a live
--    reference — drop the FK (the column stays).
ALTER TABLE audit_logs DROP CONSTRAINT audit_logs_actor_user_id_fkey;

-- 2. History can never be rewritten (UPDATE still raises),
--    but the purge cascade may erase it (DELETE allowed).
DROP TRIGGER job_events_append_only ON job_events;
CREATE TRIGGER job_events_append_only
    BEFORE UPDATE ON job_events
    FOR EACH ROW EXECUTE FUNCTION forbid_append_only_mutation();

DROP TRIGGER attestations_immutable ON attestations;
CREATE TRIGGER attestations_immutable
    BEFORE UPDATE ON attestations
    FOR EACH ROW EXECUTE FUNCTION forbid_append_only_mutation();
