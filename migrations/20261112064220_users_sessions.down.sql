-- users + sessions (P0-037, E0.4) — down migration.
-- Exactly reverses the up migration. The citext extension is left
-- in place on purpose: it may pre-date this migration (CREATE
-- EXTENSION IF NOT EXISTS) and other objects may use it.
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS users;
