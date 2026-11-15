-- P1-053: atomically prune expired leases, then admit one slot-holder.
-- KEYS[1] = pf:slots:{user}
-- ARGV[1] = job id (member), ARGV[2] = now epoch-ms, ARGV[3] = lease-expiry epoch-ms, ARGV[4] = limit, ARGV[5] = key TTL seconds.
-- Returns 1 when the slot is held (newly acquired, or already held by this job — re-acquire is idempotent and refreshes the lease), 0 when the plan cap is full.
redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, ARGV[2])
if redis.call('ZSCORE', KEYS[1], ARGV[1]) then
  redis.call('ZADD', KEYS[1], ARGV[3], ARGV[1])
  redis.call('EXPIRE', KEYS[1], ARGV[5])
  return 1
end
if redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[4]) then
  return 0
end
redis.call('ZADD', KEYS[1], ARGV[3], ARGV[1])
redis.call('EXPIRE', KEYS[1], ARGV[5])
return 1
