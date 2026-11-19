-- P1-083 Abuse spike detection (ADR-0047): abuse_reports.user_id —
-- the velocity detector's reports are system-raised, not filed by a
-- person, so they need to say WHO spiked. The public intake
-- (`POST /v1/abuse`) leaves it NULL (a reporter has no account to
-- point at); a detector report always sets it. ON DELETE CASCADE:
-- deleting the user deletes their reports with them (the audit rows
-- survive — the trail is append-only). The (user_id, created_at)
-- index serves the detector's dedup scan (one open report per user
-- per window) and the admin queue's per-user listing.
ALTER TABLE abuse_reports ADD COLUMN user_id UUID REFERENCES users (id) ON DELETE CASCADE;
CREATE INDEX abuse_reports_user_id_idx ON abuse_reports (user_id, created_at DESC);
