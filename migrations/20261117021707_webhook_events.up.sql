-- webhook_events — the provider-event ledger (P1-063, FR-P-04).
-- Per docs/03-ARCHITECTURE/04-data-model.md.
--
-- Choices the doc leaves open, recorded here:
-- - One row per provider event, keyed by (provider, event_id):
--   Stripe event ids (`evt_...`) are unique per provider, but the
--   composite key keeps a future Razorpay event from colliding
--   with a Stripe one (Razorpay ids share no namespace with
--   Stripe's). The UNIQUE constraint is the idempotency: a
--   redelivered event inserts nothing (ON CONFLICT DO NOTHING)
--   and the receiver answers 200 without dispatching twice.
-- - payload is the FULL envelope JSON (id, type, data, ...), not
--   just data.object: the row is the durable queue P1-065's
--   handlers read from, and the envelope carries the id/type the
--   handlers key off. It holds no secrets (a Stripe event body
--   carries ids + status, never keys).
-- - dispatched_at is NULL until the receiver has mapped the event
--   and handed it to the dispatcher (P1-063's dispatch seam);
--   a replay never reaches dispatch, so its dispatched_at stays
--   the first delivery's. P1-065's handlers record their own
--   application separately — this stamp means "dispatched", not
--   "applied".
-- - No expiry / GC here: events are the billing audit trail
--   (like audit_logs, 365 d retention per the data-model doc);
--   a sweep is P1-082's if one is ever wanted.
-- - Rows are never updated except dispatched_at (no trigger:
--   the repo only ever INSERTs and stamps dispatched_at).

CREATE TABLE webhook_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider TEXT NOT NULL CHECK (provider IN ('stripe', 'razorpay')),
    event_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    dispatched_at TIMESTAMPTZ,
    UNIQUE (provider, event_id)
);

CREATE INDEX webhook_events_received_at_idx ON webhook_events (received_at DESC);
