-- P1-090 Abuse queue + actions (ADR-0054): the queue's SLA
-- timestamps. `resolved_at` is stamped exactly when a report
-- leaves the queue (a resolve action), so the SLA check can
-- tell a report resolved late (resolved_at past its 72 h due
-- time) from one resolved on time — `updated_at` cannot say
-- that, because moving open → reviewing bumps it too.
-- `resolved_by` is the resolving admin (NULL until resolved).
-- ON DELETE SET NULL: deleting the admin keeps the report and
-- its resolution time (the audit row is the actor record).
ALTER TABLE abuse_reports ADD COLUMN resolved_at TIMESTAMPTZ;
ALTER TABLE abuse_reports ADD COLUMN resolved_by UUID REFERENCES users (id) ON DELETE SET NULL;
