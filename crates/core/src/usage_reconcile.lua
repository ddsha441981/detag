-- P1-050: set a UTC day's usage hash to the DB values, but only if the
-- hash still holds the values the caller read (compare-and-set). A
-- concurrent record changes the hash, the CAS fails (returns 0), and
-- the caller's retry re-reads — so a record can never be wiped by a
-- reconcile that ran on stale data.
-- KEYS[1] = pf:usage:{user}:{YYYY-MM-DD}
-- ARGV[1..2] = expected images / video_seconds ('' when the field is absent)
-- ARGV[3..4] = DB images / video_seconds to set
-- ARGV[5] = TTL seconds.
-- Returns 1 when set, 0 when the hash moved.
local cur = redis.call('HMGET', KEYS[1], 'images', 'video_seconds')
local function eq(a, b)
  if b == '' then return a == false end
  return tostring(a) == b
end
if eq(cur[1], ARGV[1]) and eq(cur[2], ARGV[2]) then
  redis.call('HSET', KEYS[1], 'images', ARGV[3], 'video_seconds', ARGV[4])
  redis.call('EXPIRE', KEYS[1], ARGV[5])
  return 1
end
return 0
