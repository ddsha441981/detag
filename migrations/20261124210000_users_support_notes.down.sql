-- Revert P1-110's support_notes column + append-only trigger. A
-- revert must come with a code revert of the ticket-notes
-- service, which appends to the column and reads it in the
-- P1-106 user view.
DROP TRIGGER IF EXISTS users_support_notes_append_only ON users;
DROP FUNCTION IF EXISTS users_support_notes_append_only();
ALTER TABLE users DROP COLUMN IF EXISTS support_notes;
