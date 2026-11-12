-- Token-bucket rate limit (P0-048). Atomic: read + refill +
-- deduct + store run in one script, so concurrent callers cannot
-- over-admit.
--
-- KEYS[1] = bucket key (pf:ratelimit:{scope}:{id})
-- ARGV[1] = capacity (tokens)
-- ARGV[2] = refill rate (tokens per second, may be fractional)
-- ARGV[3] = now (seconds since epoch, may be fractional)
-- ARGV[4] = tokens requested
-- ARGV[5] = key TTL in seconds (idle buckets expire)
--
-- Returns: {allowed (1/0), tokens_left (string, may be
-- fractional — returned as a string because Redis cannot return
-- a fractional number as an integer reply)}
local data = redis.call('HMGET', KEYS[1], 'tokens', 'updated')
local capacity = tonumber(ARGV[1])
local refill = tonumber(ARGV[2])
local now = tonumber(ARGV[3])
local requested = tonumber(ARGV[4])
local tokens = tonumber(data[1])
local updated = tonumber(data[2])
if tokens == nil then
  -- First use: the bucket starts full.
  tokens = capacity
  updated = now
else
  local elapsed = math.max(0, now - updated)
  tokens = math.min(capacity, tokens + elapsed * refill)
end
local allowed = 0
if tokens >= requested then
  tokens = tokens - requested
  allowed = 1
end
redis.call('HSET', KEYS[1], 'tokens', tokens, 'updated', now)
redis.call('EXPIRE', KEYS[1], ARGV[5])
return {allowed, tostring(tokens)}
