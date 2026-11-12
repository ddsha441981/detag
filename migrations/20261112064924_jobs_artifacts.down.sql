-- uploads + jobs + job_events + artifacts + quality_reports
-- (P0-038, E0.4) — down migration. Exactly reverses the up
-- migration; drop order follows the FK graph (children first).
DROP TABLE IF EXISTS quality_reports;
DROP TABLE IF EXISTS artifacts;
DROP TABLE IF EXISTS job_events;
DROP TABLE IF EXISTS jobs;
DROP TABLE IF EXISTS uploads;
