#!/usr/bin/env bash
# Dev bootstrap (P0-012): one command to bring the dev stack up.
#
#   just dev
#
# Steps (each gated on the artifact its ticket provides — steps whose
# ticket has not landed yet are reported as pending, not faked):
#   1. prerequisites: cargo, docker + compose plugin, .env (copied from
#      .env.example when missing — placeholders only).
#   2. postgres (ops/compose.db.yml, P0-034) — `up -d --wait`, then a
#      pg_isready wait loop; native postgres on :5432 is the fallback
#      when docker cannot run containers on this VM (DOCKER-001).
#      compose deps (ops/compose.dev.yml) — pending P0-085..P0-090.
#   3. cargo build of the workspace, then start api + worker in the
#      background (logs in .scratch/dev/).
#   4. wait for /healthz (api, exists since P0-002) and /readyz (db
#      component since P0-035; redis/storage/models land in E0.7,
#      P0-063) — readyz is pending until then.
#   5. migrations (migrations/ dir) — pending P0-036.
#   6. seed (ops/seed.sql) — pending P0-043. Assumption (logged in
#      docs/09-STATE/ISSUES.md): P0-043 provides ops/seed.sql; if it uses
#      a different artifact, this gate gets updated in that ticket.
#   7. print URLs + a PARTIAL STACK summary when steps were skipped.
#
# The command stays in the foreground: Ctrl-C stops api + worker.
# The Phase 0 exit gate re-verifies `just dev` against the full stack
# once compose/migrations/seed exist — this script grows with them.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

LOG_DIR="$REPO_ROOT/.scratch/dev"
mkdir -p "$LOG_DIR"
API_LOG="$LOG_DIR/api.log"
WORKER_LOG="$LOG_DIR/worker.log"
API_URL="http://127.0.0.1:8080"
PENDING=()

note_pending() { # <ticket> <what>
    PENDING+=("$2 (pending $1)")
    echo "dev: skipping: $2 — pending $1"
}

# --- 1. prerequisites -------------------------------------------------------
command -v cargo >/dev/null 2>&1 || { echo "dev: cargo not found (see CONTRIBUTING.md, Toolchain)" >&2; exit 1; }
if ! command -v docker >/dev/null 2>&1; then
    echo "dev: warning: docker not found — compose deps cannot start" >&2
fi
if [ ! -f .env ]; then
    cp .env.example .env
    echo "dev: created .env from .env.example (dev placeholders only)"
fi

# --- 2. postgres (compose) --------------------------------------------------
# P0-034: start the compose postgres service and wait until healthy.
# `up --wait` blocks on the pg_isready healthcheck. When docker cannot
# run containers on this VM (DOCKER-001), fall back to a native
# postgres on :5432 if one answers pg_isready; either way the wait
# loop below proves a healthy postgres before the api starts.
if [ -f ops/compose.db.yml ] && docker info >/dev/null 2>&1; then
    echo "dev: starting postgres (ops/compose.db.yml)..."
    if docker compose -f ops/compose.db.yml --env-file .env up -d --wait --wait-timeout 60; then
        echo "dev: compose postgres healthy"
    else
        # A failed start must not kill `just dev` (set -e): fall through
        # to the pg_isready wait below, which the native postgres
        # fallback can satisfy. Remove the stuck container, keep the
        # volume (it is the persistence under test in P0-034).
        echo "dev: compose postgres did not start — falling back to native postgres"
        docker compose -f ops/compose.db.yml down >/dev/null 2>&1 || true
        note_pending "DOCKER-001" "compose postgres (docker cannot run containers on this VM)"
    fi
elif [ -f ops/compose.db.yml ]; then
    note_pending "DOCKER-001" "compose postgres (docker cannot run containers on this VM)"
fi
if [ -f ops/compose.dev.yml ]; then
    echo "dev: starting compose deps..."
    docker compose -f ops/compose.dev.yml up -d
else
    note_pending "P0-085..P0-090" "compose deps (ops/compose.dev.yml)"
fi
echo -n "dev: waiting for postgres on 127.0.0.1:5432 ..."
for _ in $(seq 1 60); do
    if pg_isready -h 127.0.0.1 -p 5432 >/dev/null 2>&1; then
        echo " ok"
        break
    fi
    sleep 1
done
if ! pg_isready -h 127.0.0.1 -p 5432 >/dev/null 2>&1; then
    note_pending "P0-034" "postgres healthy (neither compose nor native postgres answered on :5432)"
fi

# --- 3. build + start api + worker ------------------------------------------
# Run the compiled binaries directly (not `cargo run`): cleanup kills the
# actual processes, not a cargo-run wrapper that would orphan them.
echo "dev: building workspace..."
cargo build --workspace --quiet
[ -x ./target/debug/detag-api ] || { echo "dev: binary missing: target/debug/detag-api" >&2; exit 1; }
[ -x ./target/debug/detag-worker ] || { echo "dev: binary missing: target/debug/detag-worker" >&2; exit 1; }
echo "dev: starting api + worker (logs in .scratch/dev/)..."
./target/debug/detag-api >"$API_LOG" 2>&1 &
API_PID=$!
./target/debug/detag-worker >"$WORKER_LOG" 2>&1 &
WORKER_PID=$!

STOPPING=0
cleanup() {
    if [ "$STOPPING" = 1 ]; then return; fi
    STOPPING=1
    echo "dev: stopping api ($API_PID) + worker ($WORKER_PID)..."
    kill "$API_PID" "$WORKER_PID" 2>/dev/null || true
    wait "$API_PID" "$WORKER_PID" 2>/dev/null || true
}
trap cleanup INT TERM EXIT

# --- 4. wait for health ------------------------------------------------------
echo -n "dev: waiting for $API_URL/healthz ..."
for _ in $(seq 1 60); do
    if curl -fsS --max-time 2 "$API_URL/healthz" >/dev/null 2>&1; then
        echo " ok"
        break
    fi
    sleep 1
    if ! kill -0 "$API_PID" 2>/dev/null; then
        echo " FAILED — api exited; see $API_LOG" >&2
        exit 1
    fi
done
if ! curl -fsS --max-time 2 "$API_URL/healthz" >/dev/null 2>&1; then
    echo " FAILED — /healthz never became ready; see $API_LOG" >&2
    exit 1
fi

if curl -fsS --max-time 2 "$API_URL/readyz" >/dev/null 2>&1; then
    echo "dev: /readyz ok (db component only; redis/storage/models land in P0-063)"
else
    echo "dev: warning: /readyz is not ok — the db component reports down; see $API_LOG" >&2
fi

# --- 5. migrations ------------------------------------------------------------
if [ -d migrations ] && ls migrations/*.sql >/dev/null 2>&1; then
    echo "dev: applying migrations..."
    # The migrate recipe is still the P0-036 stub (exits 1 with a
    # "pending" message) while migration files already exist (P0-022's
    # 0001_feature_flags.sql) — a bare `just migrate` under `set -e`
    # would kill `just dev` before it ever reports "stack up". Swallow
    # ONLY the stub's exact message; any other failure is fatal.
    # P0-036 removes this special case when the tooling lands: then a
    # failed migrate must fail the dev command.
    if ! migrate_out=$(just migrate 2>&1); then
        if echo "$migrate_out" | grep -q "pending: migration tooling lands in P0-036"; then
            note_pending "P0-036" "migrations (migrations/)"
        else
            echo "$migrate_out" >&2
            exit 1
        fi
    fi
else
    note_pending "P0-036" "migrations (migrations/)"
fi

# --- 6. seed ------------------------------------------------------------------
if [ -f ops/seed.sql ]; then
    echo "dev: seeding..."
    just seed
else
    note_pending "P0-043" "seed data (ops/seed.sql)"
fi

# --- 7. URLs ------------------------------------------------------------------
echo ""
echo "dev: stack up"
echo "  api:     $API_URL"
echo "  healthz: $API_URL/healthz"
echo "  web:     pending Phase 4 (Next.js console)"
echo "  grafana: pending P0-085..P0-090 (compose)"
if [ "${#PENDING[@]}" -gt 0 ]; then
    echo ""
    echo "dev: PARTIAL STACK — ${#PENDING[@]} step(s) skipped:"
    for item in "${PENDING[@]}"; do echo "  - $item"; done
    echo "dev: the Phase 0 exit gate re-verifies the full stack once they land."
fi
echo "dev: Ctrl-C to stop."
# Watch both children: if either exits, report it and stop the other
# (cleanup trap) instead of hanging on the survivor.
while [ "$STOPPING" = 0 ] && kill -0 "$API_PID" 2>/dev/null && kill -0 "$WORKER_PID" 2>/dev/null; do
    sleep 1
done
if [ "$STOPPING" = 1 ]; then
    exit 0  # user stopped it with Ctrl-C / SIGTERM
fi
if ! kill -0 "$API_PID" 2>/dev/null; then
    echo "dev: api exited unexpectedly; see $API_LOG" >&2
    exit 1
fi
echo "dev: worker exited unexpectedly; see $WORKER_LOG" >&2
exit 1
