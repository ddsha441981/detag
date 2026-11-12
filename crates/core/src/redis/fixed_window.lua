-- Fixed-window rate limit (P0-048). Atomic: INCR + EXPIRE run
-- in one script, so two concurrent callers cannot both see
-- count == 1 and lose the expiry.
--
-- KEYS[1] = bucket key (pf:ratelimit:{scope}:{id})
-- ARGV[1] = window in seconds
--
-- Returns: {count, ttl_seconds} — the caller allows the request
-- when count <= limit and reports remaining = limit - count.
local count = redis.call('INCR', KEYS[1])
if count == 1 then
  redis.call('EXPIRE', KEYS[1], ARGV[1])
end
local ttl = redis.call('TTL', KEYS[1])
return {count, ttl}
