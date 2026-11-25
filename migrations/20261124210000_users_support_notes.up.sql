-- Ticket notes (P1-110): admin-only append-only notes on a user,
-- stored as a jsonb list on the user row (ADR-0069 /
-- ASSUMPTION-071). Each note is one JSON object: {id (uuidv7),
-- author_id, author_role, body, created_at} — the author role is
-- recorded at write time, so the history says who the author was
-- then. The list is append-only by construction (one repo fn
-- appends, no endpoint edits or deletes) AND by enforcement: the
-- trigger below raises on any UPDATE that drops or rewrites an
-- existing element (jsonb containment — every old element must
-- survive in the new list). Reordering passes the trigger
-- (containment is order-insensitive) but the app never reorders
-- (append + oldest-first), so the trigger's job is no-loss /
-- no-edit, not ordering. A wrong note is corrected by appending
-- a new one — the history stays honest. No new table: the notes
-- are per-user working text, not a queryable entity; the purge
-- deletes the user row and the notes go with it.
ALTER TABLE users ADD COLUMN support_notes jsonb NOT NULL DEFAULT '[]'::jsonb;

CREATE OR REPLACE FUNCTION users_support_notes_append_only()
RETURNS trigger AS $$
BEGIN
  IF OLD.support_notes IS DISTINCT FROM NEW.support_notes
     AND NOT (OLD.support_notes <@ NEW.support_notes) THEN
    RAISE EXCEPTION 'support_notes is append-only';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER users_support_notes_append_only
  BEFORE UPDATE ON users
  FOR EACH ROW EXECUTE FUNCTION users_support_notes_append_only();
