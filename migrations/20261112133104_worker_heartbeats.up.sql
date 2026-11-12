-- worker_heartbeats (P0-080, E0.8). Per docs/03-ARCHITECTURE/04-data-model.md.
--
-- One row per worker process, upserted every heartbeat tick
-- (PF_WORKER_HEARTBEAT_INTERVAL_SECS, default 15 s). A row whose
-- last_heartbeat_at is older than ~2 intervals is a dead worker:
-- staleness IS the signal, so nothing deletes the row on a crash
-- (a graceful shutdown deletes its own row; see the heartbeat
-- loop). Stale rows are pruned by the writer after 1 h.
--
-- Choices the doc leaves open, recorded here:
-- - worker_id is `{hostname}:{pid}` (TEXT PRIMARY KEY) — the same
--   id as the `detag_worker_info` metric label, so the row and the
--   scrape agree on which process they describe. pid alone is not
--   unique across hosts; hostname comes from $HOSTNAME.
-- - queues is TEXT[] — the queues this process serves (the four
--   worker queues in Phase 0). No CHECK: the queue set grows with
--   the product, and a CHECK would need a migration per queue.
-- - in_flight is INT >= 0 (CHECK): jobs inside a handler right
--   now (fetched, not yet acked). It is a point-in-time count, not
--   a counter — the upsert overwrites it every tick.
-- - started_at never changes after the first insert (the upsert
--   keeps the original value); last_heartbeat_at moves every tick.
-- - No FKs: a heartbeat row is process state, not domain data.

CREATE TABLE worker_heartbeats (
    worker_id TEXT PRIMARY KEY,
    pid INTEGER NOT NULL,
    queues TEXT[] NOT NULL,
    in_flight INTEGER NOT NULL DEFAULT 0 CHECK (in_flight >= 0),
    started_at TIMESTAMPTZ NOT NULL,
    last_heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX worker_heartbeats_last_heartbeat_idx
    ON worker_heartbeats (last_heartbeat_at);
