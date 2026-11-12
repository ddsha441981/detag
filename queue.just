# Queue tasks (P0-079). Run as `just queue::requeue-stuck` (or `just queue requeue-stuck`).
# NOTE: the ticket spells the command `just queue:requeue-stuck` (single colon),
# but just only accepts the double-colon form `queue::requeue-stuck` — that is
# the invocation to use.

# Re-queue jobs orphaned by dead workers (visibility-timeout sweep, P0-079, NFR-21).
# Args pass through to queue-sweep: --queue <name> (repeatable),
# --older-than-secs N (default: PF_QUEUE_VISIBILITY_TIMEOUT_SECS),
# --limit N (default: 100 per queue). No --queue sweeps all four queues.
requeue-stuck *args:
    cargo run --quiet -p detag-worker --bin queue-sweep -- {{args}}
