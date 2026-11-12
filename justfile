# Detag task runner. `just --list` shows all recipes.
# Lint policy: clippy runs with `-D warnings`; an `#[allow]` needs a
# justification comment next to it (see CONTRIBUTING.md, "Lint policy").

set dotenv-load := false

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

# Verify gate: fmt + clippy + tests. (Compose smoke is `just smoke`, kept
# separate until P0-090 lands it; see ADR-0017 in docs/09-STATE/DECISIONS.md.)
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

# Apply sqlx migrations (tooling lands in P0-036).
migrate:
    @echo "pending: migration tooling lands in P0-036" >&2; exit 1

# Seed plans, feature flags, admin invite (lands in P0-043).
seed:
    @echo "pending: seed task lands in P0-043" >&2; exit 1

# Compose smoke: up, health checks, enqueue ping job, assert processed.
# Lands in P0-090; see ADR-0017 for why `verify` does not call it yet.
smoke:
    @echo "pending: smoke script lands in P0-090 (blocked: docker image pulls, see docs/09-STATE/ISSUES.md)" >&2; exit 1

# Generate deterministic golden fixtures (lands in P0-093).
fixtures-gen:
    @echo "pending: fixture generator lands in P0-093" >&2; exit 1

# Fail if .env.example drifts from PF_* vars referenced in code or
# documented in the runbook table (P0-011).
env-check:
    python3 scripts/env_check.py

# Validate the Postgres dev compose file (P0-034): parses, env
# interpolates, healthcheck + named volume render. Static only — it
# does not start containers (CI's postgres service container already
# holds :5432; the live `up --wait` check waits for E0.9's smoke,
# P0-090). Requires the docker compose plugin.
compose-db-check:
    docker compose -f ops/compose.db.yml --env-file .env config --quiet

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

# Full gate incl. smoke, for use once P0-090 lands.
verify-full: verify smoke
