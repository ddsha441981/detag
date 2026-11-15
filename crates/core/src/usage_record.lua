-- P1-050: atomically add to a UTC day's usage hash and refresh its TTL.
-- KEYS[1] = pf:usage:{user}:{YYYY-MM-DD}
-- ARGV[1] = images delta, ARGV[2] = video-seconds delta, ARGV[3] = TTL seconds.
-- Returns the new totals: {images, video_seconds}.
redis.call('HINCRBY', KEYS[1], 'images', ARGV[1])
redis.call('HINCRBY', KEYS[1], 'video_seconds', ARGV[2])
redis.call('EXPIRE', KEYS[1], ARGV[3])
return redis.call('HMGET', KEYS[1], 'images', 'video_seconds')
