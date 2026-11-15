-- P1-053: prune expired leases, then count the live slots.
-- KEYS[1] = pf:slots:{user}
-- ARGV[1] = now epoch-ms.
-- Returns the live slot count.
redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, ARGV[1])
return redis.call('ZCARD', KEYS[1])
