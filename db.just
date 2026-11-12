# Database tasks (P0-043). Run as `just db::reset` (or `just db reset`).
# NOTE: the ticket spells the command `just db:reset` (single colon),
# but just only accepts the double-colon form `db::reset` — that is
# the invocation to use.

# Drop + recreate the local dev database, then migrate + seed
# (P0-043). DEV ONLY: the runner refuses any database that is not
# localhost, and there is no undo — it drops the whole database.
reset:
    cargo run --quiet -p detag-core --example migrate -- reset
