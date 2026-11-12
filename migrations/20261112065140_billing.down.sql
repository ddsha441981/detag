-- plans + subscriptions + usage_counters (P0-039, E0.4) — down
-- migration. Exactly reverses the up migration (seed rows go away
-- with the plans table); drop order follows the FK graph.
DROP TABLE IF EXISTS subscriptions;
DROP TABLE IF EXISTS usage_counters;
DROP TABLE IF EXISTS plans;
