-- P1-053: extend a live lease; a released or expired slot cannot be resurrected.
-- KEYS[1] = pf:slots:{user}
-- ARGV[1] = job id, ARGV[2] = now epoch-ms, ARGV[3] = new lease-expiry epoch-ms, ARGV[4] = key TTL seconds.
-- Returns 1 when renewed, 0 when the slot is not held (released, expired, or never acquired).
redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, ARGV[2])
if not redis.call('ZSCORE', KEYS[1], ARGV[1]) then
  return 0
end
redis.call('ZADD', KEYS[1], ARGV[3], ARGV[1])
redis.call('EXPIRE', KEYS[1], ARGV[4])
return 1
