#!/usr/bin/env bash
# Compose smoke (P0-090): up the dev stack, wait for health, ping the pipeline, print URLs.
#
#   just smoke [--skip-up] [--down]
#
# Steps:
#   1. prerequisites: docker + compose plugin, cargo, .env (copied
#      from .env.example when missing — placeholders only).
#   2. compose up -d --wait --wait-timeout 120 (base+dev, --profile
#      dev) — compose itself fails when any service is still
#      unhealthy after 120 s (the P0-088 healthchecks + budget).
#      Skipped with --skip-up: the stack is assumed already up
#      (a previous up, or the native stack from `just dev`).
#   3. `just migrate` — the schema must exist for a truthful smoke.
#      Skipped with --skip-up (the native stack migrates in dev.sh).
#   4. wait for /healthz 200 (60 s), then /readyz 200 (60 s in
#      compose mode; read once in --skip-up mode — the stack is
#      already up and will not change). In --skip-up mode /readyz
#      may report a component down on a partial native stack (no
#      Garage here): warn and continue when db+redis are ok, fail
#      only when they are down.
#   5. `just smoke-ping` (P0-082): enqueue a ping job on the
#      pipeline queue and assert the worker processes it.
#   6. print URLs. With --down, tear the compose stack down at
#      the end (ignored with --skip-up: the script did not start
#      that stack).
#
# Exit 0 only when every step passed. On failure after the up,
# the script prints `docker compose ps` + the api/worker logs so
# CI shows the diagnosis instead of a bare timeout.
#
# The `docker compose up` step needs a working daemon — on this VM
# it fails loudly (DOCKER-001: containers cannot start here). The
# wait/ping/URL legs are provable with --skip-up against the native
# stack (`just dev` in another shell); the full run, including the
# up, is the CI `smoke` job's.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

SKIP_UP=0
DOWN=0
for arg in "$@"; do
    case "$arg" in
        --skip-up) SKIP_UP=1 ;;
        --down) DOWN=1 ;;
        *) echo "smoke: unknown argument: $arg (usage: just smoke [--skip-up] [--down])" >&2; exit 2 ;;
    esac
done

API_PORT="${PF_API_PORT:-8080}"
API_URL="http://127.0.0.1:${API_PORT}"
COMPOSE="docker compose -f ops/compose.base.yml -f ops/compose.dev.yml --profile dev --env-file .env"

fail() { # <message>
    echo "smoke: FAILED — $1" >&2
    if [ "$SKIP_UP" = 0 ]; then
        echo "smoke: compose state at failure:" >&2
        $COMPOSE ps >&2 || true
        echo "smoke: api/worker logs (tail 50):" >&2
        $COMPOSE logs --tail=50 api worker >&2 || true
        echo "smoke: garage logs (tail 50):" >&2
        $COMPOSE logs --tail=50 garage >&2 || true
    fi
    exit 1
}

# --- 1. prerequisites -------------------------------------------------------
command -v docker >/dev/null 2>&1 || fail "docker not found (see CONTRIBUTING.md, Toolchain)"
command -v cargo >/dev/null 2>&1 || fail "cargo not found (see CONTRIBUTING.md, Toolchain)"
command -v curl >/dev/null 2>&1 || fail "curl not found"
if [ ! -f .env ]; then
    cp .env.example .env
    echo "smoke: created .env from .env.example (dev placeholders only)"
fi

# Garage refuses to start unless the RPC secret is 32 bytes of random
# hex (CI-FAIL-008: the .env.example placeholder was the literal string
# "<redacted>" and the server died on start, so the stack never went
# healthy). Fail fast here with a clear message instead of a 120 s
# timeout and an "unhealthy" container.
RPC_SECRET="$(grep -E '^GARAGE_RPC_SECRET=' .env | cut -d= -f2-)"
if ! printf '%s' "$RPC_SECRET" | grep -Eq '^[0-9a-f]{64}$'; then
    fail "GARAGE_RPC_SECRET in .env is not 32 bytes of random hex (see .env.example)"
fi

# --- 2. compose up ----------------------------------------------------------
if [ "$SKIP_UP" = 0 ]; then
    docker info >/dev/null 2>&1 || fail "docker daemon is not answering (DOCKER-001: containers cannot start on this VM — the full smoke runs in CI)"
    echo "smoke: starting compose stack (base+dev)..."
    $COMPOSE up -d --wait --wait-timeout 120 || fail "compose stack did not go healthy in 120s"
    echo "smoke: compose stack healthy"
else
    echo "smoke: --skip-up: assuming the stack is already up"
fi

# --- 3. migrate --------------------------------------------------------------
if [ "$SKIP_UP" = 0 ]; then
    echo "smoke: applying migrations..."
    just migrate || fail "migrations failed"
fi

# --- 4. wait for /healthz + /readyz ------------------------------------------
echo -n "smoke: waiting for $API_URL/healthz ..."
for _ in $(seq 1 60); do
    if curl -fsS --max-time 2 "$API_URL/healthz" >/dev/null 2>&1; then
        echo " ok"
        break
    fi
    sleep 1
done
curl -fsS --max-time 2 "$API_URL/healthz" >/dev/null 2>&1 || fail "/healthz never became ready"

# /readyz: in compose mode the stack is still starting, so poll up to
# 60 s for a 200. In --skip-up mode the stack is already up and will
# not change — read it once instead of burning the whole minute:
# a 503 still carries the component body, so parse it.
if [ "$SKIP_UP" = 0 ]; then
    echo -n "smoke: waiting for $API_URL/readyz ..."
    READYZ_OK=0
    for _ in $(seq 1 60); do
        if curl -fsS --max-time 5 "$API_URL/readyz" >/dev/null 2>&1; then
            READYZ_OK=1
            echo " ok"
            break
        fi
        sleep 1
    done
    [ "$READYZ_OK" = 1 ] || fail "/readyz never became ready (a component reports down — curl $API_URL/readyz for the per-component diagnosis)"
    curl -fsS --max-time 5 "$API_URL/readyz" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("smoke: /readyz components: " + ", ".join(c["name"] + " ok (" + str(c["latency_ms"]) + "ms)" for c in d["components"]))'
else
    # (No -f here: a 503 /readyz still carries the component body,
    # and that body is what this branch reads.)
    READYZ_BODY="$(curl -sS --max-time 5 "$API_URL/readyz" 2>/dev/null || true)"
    if [ -z "$READYZ_BODY" ]; then
        fail "/readyz unreachable after /healthz was ok (api died? see the api log)"
    fi
    echo "$READYZ_BODY" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("smoke: /readyz components: " + ", ".join(c["name"] + " " + ("ok" if c["ok"] else "DOWN") + " (" + str(c["latency_ms"]) + "ms)" for c in d["components"]))'
    DOWN_COMPONENTS="$(echo "$READYZ_BODY" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(",".join(c["name"] for c in d["components"] if not c["ok"]))')"
    case ",$DOWN_COMPONENTS," in
        *,db,*|*,redis,*) fail "/readyz reports down: $DOWN_COMPONENTS" ;;
    esac
    if [ -n "$DOWN_COMPONENTS" ]; then
        # Partial native stack: storage/models may be down (no Garage
        # on this VM). db + redis are ok (checked above), so warn
        # and continue — the ping leg still proves redis+worker.
        echo "smoke: warning: /readyz is degraded (down: $DOWN_COMPONENTS) — storage/models are down on the native stack without Garage; continuing"
    fi
fi

# --- 5. ping the pipeline ------------------------------------------------------
echo "smoke: enqueueing ping job (smoke-ping)..."
just smoke-ping || fail "smoke-ping failed (the ping job was not processed)"

# --- 6. URLs -------------------------------------------------------------------
echo ""
echo "smoke: stack up"
echo "  api:     $API_URL"
echo "  healthz: $API_URL/healthz"
echo "  readyz:  $API_URL/readyz"
echo "  docs:    $API_URL/docs (Swagger UI, dev only)"
echo "  web:     http://127.0.0.1:3000 (console placeholder, P0-085)"
echo "  s3:      http://127.0.0.1:9000 (Garage S3 API, dev only — no console)"
echo "  mailhog: http://127.0.0.1:8025 (dev only)"
echo "  metrics: api :9090, worker :9092 (dev only)"
if [ "$DOWN" = 1 ] && [ "$SKIP_UP" = 0 ]; then
    echo "smoke: tearing the stack down (--down)..."
    $COMPOSE down
elif [ "$DOWN" = 1 ]; then
    echo "smoke: --down ignored with --skip-up (this script did not start the stack)"
else
    echo "smoke: stack left up — tear it down with: docker compose -f ops/compose.base.yml -f ops/compose.dev.yml --profile dev down"
fi
echo "smoke: OK"
