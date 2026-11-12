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

# Run the full unit + integration test suite.
test:
    cargo test --workspace

# Verify gate: fmt + clippy + tests. (Compose smoke is `just smoke`, kept
# separate until P0-090 lands it; see ADR-0017 in docs/09-STATE/DECISIONS.md.)
verify: fmt-check lint test
    @echo "verify OK: fmt + clippy + tests green (smoke: see 'just smoke')"

# --- Phase 0 stubs: implemented by their own tickets (fail loudly until then) ---

# Start the full dev stack (compose deps, wait healthy, migrate, seed).
# Implemented in P0-012.
dev:
    @echo "pending: 'just dev' lands in P0-012 (compose files land in E0.9)" >&2; exit 1

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

# Fail if .env.example drifts from PF_* vars referenced in code (lands in P0-011).
env-check:
    @echo "pending: env drift check lands in P0-011" >&2; exit 1

# Full gate incl. smoke, for use once P0-090 lands.
verify-full: verify smoke
