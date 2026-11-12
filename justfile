# Detag task runner. `just --list` shows all recipes.
# Lint policy: clippy runs with `-D warnings`; an `#[allow]` needs a
# justification comment next to it (see CONTRIBUTING.md, "Lint policy").

set dotenv-load := false

# Database tasks live in db.just (`just db::reset`, `just db reset`).
mod db

# Queue tasks live in queue.just (`just queue::requeue-stuck`).
mod queue

# sqlx `query!` macros compile offline from the checked-in `.sqlx/`
# data (P0-041); a live DB is only needed when regenerating it
# (`just sqlx-prepare`). Without this, every cargo build/test would
# need DATABASE_URL set.
export SQLX_OFFLINE := "true"

# Format all Rust code.
fmt:
    cargo fmt --all

# Check formatting without changing files.
fmt-check:
    cargo fmt --all -- --check

# Lint the workspace; warnings are errors.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Lint commit messages for the conventional-commit policy (P0-006, NFR-52).
# Default: the HEAD commit only; pass e.g. '--range main~3..HEAD' for more.
lint-commits args="--single HEAD":
    python3 scripts/lint_commits.py {{args}}

# Unit-test the commit-message linter (stdlib unittest).
test-lint-commits:
    python3 -m unittest discover -s scripts -p "test_*.py"

# Supply-chain check: RUSTSEC advisories + license allow-list (P0-007).
deny:
    cargo deny check

# Secret scan of the whole repo (P0-008); pre-commit scans staged changes.
secrets:
    gitleaks detect --source . --config .gitleaks.toml --verbose

# Coverage report (llvm-cov, lcov). Floors per docs/05-QUALITY/01-test-strategy.md.
coverage:
    mkdir -p target/llvm-cov
    cargo llvm-cov --workspace --lcov --output-path target/llvm-cov/lcov.info

# Release dry-run: prints the next version implied by the commits, writes
# nothing (P0-010). Generate the changelog stub with `just changelog`.
release-dry-run:
    cog bump --auto --dry-run

# Regenerate CHANGELOG.md from conventional commits (P0-010).
# cog 7 prints the changelog to stdout; the recipe writes it to the
# path in cog.toml.
changelog:
    cog changelog > CHANGELOG.md

# Run the full unit + integration test suite.
test:
    cargo test --workspace

# Verify gate: fmt + clippy + tests. (Compose smoke is `just smoke`,
# kept separate: it needs a working docker daemon, which this VM does
# not have (DOCKER-001); see ADR-0017 in docs/09-STATE/DECISIONS.md.)
verify: fmt-check lint test
    @echo "verify OK: fmt + clippy + tests green (smoke: see 'just smoke')"

# --- Phase 0 stubs: implemented by their own tickets (fail loudly until then) ---

# Start the full dev stack (compose deps, wait healthy, migrate, seed).
# Implemented in P0-012; steps whose tickets have not landed yet are
# reported as pending by scripts/dev.sh (see docs/09-STATE/ISSUES.md).
dev:
    bash scripts/dev.sh

# Run Criterion benches (scaffold lands in P0-097).
bench:
    @echo "pending: benches land in P0-097" >&2; exit 1

# Apply pending sqlx migrations from migrations/ (P0-036, embedded
# Migrator — no sqlx-cli needed). Uses PF_DATABASE_URL (dev default
# when unset). Idempotent: a second run applies nothing.
migrate:
    cargo run --quiet -p detag-core --example migrate -- up

# Create a new reversible migration pair (P0-036):
# migrations/<UTC timestamp>_<name>.up.sql + .down.sql.
migrate-new name:
    cargo run --quiet -p detag-core --example migrate -- new {{name}}

# Revert the last applied migration (P0-036). DEV ONLY — the runner
# refuses any database that is not localhost, and only reversible
# migrations (an .up.sql/.down.sql pair) can revert.
migrate-revert:
    cargo run --quiet -p detag-core --example migrate -- revert

# Regenerate checked-in sqlx offline query metadata (P0-037).
# Needs sqlx-cli (`cargo install sqlx-cli --version 0.9.0 --locked
# --no-default-features --features postgres,rustls`) and a live,
# MIGRATED DB at PF_DATABASE_URL (run `just migrate` first — the
# macros expand against its schema). `SQLX_OFFLINE=false` is
# required: the justfile exports SQLX_OFFLINE=true for normal
# builds, and prepare's internal check must run online (P0-057:
# without it prepare wipes .sqlx/ and then fails to regenerate).
# With no query! macros yet this is a no-op ("no queries found") —
# it becomes real when the repo layer lands.
sqlx-prepare:
    SQLX_OFFLINE=false DATABASE_URL="${PF_DATABASE_URL:-postgres://pf:pf@localhost:5432/pf}" cargo sqlx prepare --workspace

# Fail if the checked-in .sqlx/ metadata is stale (P0-037; CI runs
# this after `just migrate`). Passes vacuously until query! macros
# exist — then it is the freshness gate.
sqlx-prepare-check:
    DATABASE_URL="${PF_DATABASE_URL:-postgres://pf:pf@localhost:5432/pf}" cargo sqlx prepare --workspace --check

# Seed plans + feature flags (P0-043). Idempotent: run twice,
# the second run changes nothing and never clobbers an admin edit
# (ON CONFLICT DO NOTHING). Uses PF_DATABASE_URL (dev default
# when unset). Needs migrations 0001 + 003 applied — run
# `just migrate` first (or `just db::reset`, which does both).
seed:
    cargo run --quiet -p detag-core --example seed

# Compose smoke (P0-090): up the dev stack, wait for healthz/readyz,
# enqueue a ping job, assert processed, print URLs. Runs in CI
# (the `smoke` job); on this VM docker cannot run containers, so
# the full run fails loudly (DOCKER-001) and the wait/ping/URL legs
# are provable with --skip-up against the native stack (`just dev`
# in another shell). See scripts/smoke.sh.
smoke args="":
    bash scripts/smoke.sh {{args}}

# Native-stack smoke probe (P0-082): enqueue a ping job on the
# pipeline queue and assert the worker processes it. Run with the
# api + worker up on the native stack; `just smoke` (P0-090) calls
# the same bin against the compose stack.
smoke-ping:
    cargo run --quiet -p detag-worker --bin smoke-ping

# Generate deterministic golden fixtures (P0-093): 24 marked images +
# 12 clean controls, 3 videos, expected-metrics envelopes, SHA256SUMS.
# Needs Pillow + numpy + ffmpeg (see fixtures/README.md for the
# reproducibility contract and the pinned versions).
fixtures-gen:
    python3 scripts/fixtures_gen.py

# Fail if committed fixtures drift from the generator (P0-093; CI runs
# this). Regenerates to a temp dir and compares bytes (images always;
# videos only when ffmpeg matches the recorded version — otherwise
# stream properties via ffprobe), envelope sha/bytes fields, SHA256SUMS.
fixtures-check:
    python3 scripts/fixtures_gen.py --check

# Regenerate the committed OpenAPI document from code (P0-067).
# The spec is generated, never hand-edited: `just openapi-check`
# (CI) fails when this file drifts from the code.
openapi:
    mkdir -p docs/api
    cargo run --quiet -p detag-api --bin openapi > docs/api/openapi.json

# Fail if the committed OpenAPI document drifts from the code
# (P0-067; CI runs this). Regenerate with `just openapi`.
openapi-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    cargo run --quiet -p detag-api --bin openapi > "$tmp"
    diff -u docs/api/openapi.json "$tmp" || { echo "openapi-check: docs/api/openapi.json is stale — run 'just openapi' and commit it" >&2; exit 1; }

# Regenerate the committed route table from code (P0-074).
# Generated, never hand-edited: `just route-table-check` (CI)
# fails when this file drifts from the code.
route-table:
    mkdir -p docs/api
    cargo run --quiet -p detag-api --bin route_table > docs/api/route-table.md

# Fail if the committed route table drifts from the code
# (P0-074; CI runs this). Regenerate with `just route-table`.
route-table-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    cargo run --quiet -p detag-api --bin route_table > "$tmp"
    diff -u docs/api/route-table.md "$tmp" || { echo "route-table-check: docs/api/route-table.md is stale — run 'just route-table' and commit it" >&2; exit 1; }

# Fail if .env.example drifts from PF_* vars referenced in code or
# documented in the runbook table (P0-011).
env-check:
    python3 scripts/env_check.py

# Fail when a table exists in the migrations but not in the
# data-model doc, or vice versa (P0-045, NFR-52). Static only —
# it parses SQL files, no database needed. Runs in CI.
schema-docs-check:
    python3 scripts/check_schema_docs.py

# Validate the Postgres dev compose file (P0-034): parses, env
# interpolates, healthcheck + named volume render. Static only — it
# does not start containers (CI's postgres service container already
# holds :5432; the live `up --wait` check waits for E0.9's smoke,
# P0-090). Requires the docker compose plugin.
compose-db-check:
    docker compose -f ops/compose.db.yml --env-file .env.example config --quiet

# Validate the Redis dev compose file (P0-046): parses, env
# interpolates, healthcheck renders. Static only — it does not start
# containers (the live `up --wait` check waits for E0.9's smoke,
# P0-090). Requires the docker compose plugin.
compose-redis-check:
    docker compose -f ops/compose.redis.yml --env-file .env.example config --quiet

# Validate the MinIO dev compose file (P0-054): the static policy
# check first (bucket stays private — runnable anywhere, no docker
# needed), then `config --quiet` (parses, env interpolates,
# healthcheck renders). Static only — it does not start containers
# (the live `up --wait` + anonymous-access probe wait for CI/E0.9's
# smoke, P0-090). Requires the docker compose plugin.
compose-minio-check:
    python3 scripts/check_minio_policy.py
    python3 scripts/check_minio_lifecycle.py
    docker compose -f ops/compose.minio.yml --env-file .env.example config --quiet

# Validate the base compose file (P0-085): renders for no profile, dev
# and prod, and the service lists match the profile contract (web is
# dev-only). Static only — it does not start containers (the live `up`
# waits for CI/E0.9's smoke, P0-090). Requires the docker compose plugin.
# NOTE: the compose checks use --env-file .env.example (P0-085), not
# .env: `--env-file` fails when the file is missing, and a fresh
# checkout / CI has no .env — the example's dev defaults are what the
# checks need. `just dev` (the real dev flow) still uses the dev .env.
compose-base-check:
    docker compose -f ops/compose.base.yml --env-file .env.example config --quiet
    docker compose -f ops/compose.base.yml --profile dev --env-file .env.example config --quiet
    docker compose -f ops/compose.base.yml --profile prod --env-file .env.example config --quiet
    docker compose -f ops/compose.base.yml --profile dev --env-file .env.example config --services | grep -q '^web$'
    ! docker compose -f ops/compose.base.yml --profile prod --env-file .env.example config --services | grep -q '^web$'

# Validate the dev compose override (P0-086): base+dev renders with the
# dev conveniences (mailhog, published data/metrics ports, web). Static
# only — it does not start containers. Requires the docker compose plugin.
compose-dev-check:
    docker compose -f ops/compose.base.yml -f ops/compose.dev.yml --profile dev --env-file .env.example config --quiet
    docker compose -f ops/compose.base.yml -f ops/compose.dev.yml --profile dev --env-file .env.example config --services | grep -q '^mailhog$'
    docker compose -f ops/compose.base.yml -f ops/compose.dev.yml --profile dev --env-file .env.example config --services | grep -q '^web$'

# Validate the prod compose override (P0-087): base+prod renders with
# caddy as the only entry point (api port unpublished, data ports
# unpublished), PF_ENVIRONMENT: prod, mem caps, restart policies and log
# rotation. Static only — it does not start containers (the live `up`
# waits for CI/E0.9's smoke, P0-090). Requires the docker compose plugin.
compose-prod-check:
    docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env.example config --quiet
    docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env.example config --services | grep -q '^caddy$'
    ! docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env.example config --services | grep -q '^web$'
    ! docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env.example config | grep -q 'published: "8080"'
    docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env.example config | grep -q 'PF_ENVIRONMENT: prod'
    docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env.example config | grep -q 'memory: "2147483648"'

# Validate healthchecks everywhere (P0-088): every service except the
# one-shot bootstrap has a healthcheck (api/worker via their
# Dockerfiles), each inside the 120 s smoke budget, and the
# depends_on graph waits on healthy/completed services. Static only —
# it renders both profiles, it does not start containers (the live
# `up --wait` check waits for CI/E0.9's smoke, P0-090). Requires the
# docker compose plugin.
compose-health-check:
    python3 scripts/check_compose_health.py

# Validate compose env wiring (P0-089): every ${VAR} in every
# ops/*.yml is a .env.example key with a matching default, every
# secret-named env value is a ${...} interpolation (no literal
# secrets in yml), and $${VAR} shell escapes name a var the same
# service sets. Static only — it parses the files, it never starts
# a container.
compose-env-check:
    python3 scripts/check_compose_env.py

# Image size gate (P0-083, NFR-63; P0-084 worker): build the api and
# worker images and fail if either is over its ceiling (the check reads
# the uncompressed size, a conservative proxy for the compressed limit —
# see scripts/check_image_size.py). Api 150 MB is NFR-63; worker 200 MB
# is a Phase-0 ops guardrail (NFR-63 caps API images only — the worker
# ceiling comes from the parts estimate, see Dockerfile.worker).
# Needs a working docker daemon: fails loudly here (DOCKER-001) and
# runs green in CI.
image-size-check:
    docker build -f Dockerfile.api -t detag-api:check .
    python3 scripts/check_image_size.py --image detag-api:check --max-mb 150
    docker build -f Dockerfile.worker -t detag-worker:check .
    python3 scripts/check_image_size.py --image detag-worker:check --max-mb 200

# Retention GC (P0-060, FR-J-09): dry-run by default — lists the
# bucket, reports expired artifacts + old orphans, deletes nothing.
# `cargo run -p detag-gc -- --execute` is the deleting run. Needs
# PF_DATABASE_URL and a reachable S3/MinIO; fails loudly without
# them (a GC that cannot read the DB must not run at all).
gc:
    cargo run -p detag-gc -- --dry-run

# Validate Grafana dashboards + provisioning (P0-030, NFR-62):
# JSON parses, uids unique, panels well-formed, every PromQL expr
# queries only series the code registers.
dashboards-check:
    python3 scripts/validate_dashboards.py

# Validate Prometheus config + alerts (P0-031, NFR-62): static
# validator (YAML, scrape jobs, rule completeness, PromQL
# allow-list) plus promtool check config/rules when promtool is
# available (download a Prometheus release tarball and put promtool
# on PATH, or use .scratch/tools/promtool). Absent promtool only
# warns — the static validator is the enforced gate.
prometheus-check:
    #!/usr/bin/env bash
    set -euo pipefail
    python3 scripts/validate_prometheus.py
    if command -v promtool >/dev/null 2>&1; then PROMTOOL=promtool;
    elif [ -x .scratch/tools/promtool ]; then PROMTOOL=.scratch/tools/promtool;
    else echo "prometheus-check: promtool not found (static checks only)"; exit 0; fi
    "$PROMTOOL" check config ops/prometheus/prometheus.yml
    "$PROMTOOL" check rules ops/prometheus/alerts.yml

# Regenerate the PF_* settings reference table (P0-024). The table is
# generated from .env.example (name + dev default) and the runbook env
# table (used-by); never edit docs/06-DELIVERY/04-config-reference.md
# by hand.
gen-config-table:
    python3 scripts/gen_config_table.py

# Fail if the committed settings table is stale (P0-024); runs in CI.
config-table-check:
    python3 scripts/gen_config_table.py --check

# Full gate incl. smoke (P0-090 landed it; docker-capable machines only —
# on this VM docker cannot run containers, DOCKER-001).
verify-full: verify smoke
