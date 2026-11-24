-- Revert P1-107's quota_grants table. A revert must come with
-- a code revert of the quota-grant service, which reads the
-- table for the unexpired totals in the quota snapshot.
DROP TABLE IF EXISTS quota_grants;
