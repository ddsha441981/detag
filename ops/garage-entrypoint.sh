#!/bin/sh
# garage-entrypoint.sh — single-node Garage bootstrap + server (CI-FAIL-004).
#
# Runs as PID 1 in the garage container. Flow:
#   1. start the server in the background (the local CLI needs a live node),
#   2. layout: give this node a role (zone dc1, 10G) + apply, once,
#   3. key: import the deterministic dev key (PF_S3_* from the env),
#   4. bucket: create it + allow the key RW+owner (private by default —
#      Garage has no ACLs/policies; buckets are private unless website
#      access is turned on, which this stack never does),
#   5. restart the server in the foreground so PID 1 is the server.
# Every step is idempotent: re-running `up` is a no-op for finished work.
# Any failure exits non-zero, so compose shows a dead garage container
# instead of a stack that serves without a bucket.
set -eu

CONFIG=/etc/garage.toml

# 1. Server in the background.
garage -c "$CONFIG" server &
SERVER_PID=$!

# 2. Wait for the RPC endpoint (bounded: fail loudly after ~60 s).
i=0
until garage -c "$CONFIG" status >/dev/null 2>&1; do
  i=$((i + 1))
  if [ "$i" -ge 60 ]; then
    echo "garage-entrypoint: server did not answer in 60s" >&2
    kill "$SERVER_PID" 2>/dev/null || true
    exit 1
  fi
  sleep 1
done

# 3. Layout (only while the node has no role).
if garage -c "$CONFIG" status 2>/dev/null | grep -q "NO ROLE ASSIGNED"; then
  NODE_ID=$(garage -c "$CONFIG" status 2>/dev/null | awk '$1 ~ /^[0-9a-f]{16}$/ {print $1; exit}')
  garage -c "$CONFIG" layout assign -z dc1 -c 10G "$NODE_ID"
  garage -c "$CONFIG" layout apply --version 1
fi

# 4. Access key (Garage secrets are >= 16 chars — the .env.example
# placeholder respects that; `key info` proves the key already exists).
garage -c "$CONFIG" key info detag-dev >/dev/null 2>&1 || \
  garage -c "$CONFIG" key import --yes -n detag-dev "$PF_S3_ACCESS_KEY" "$PF_S3_SECRET_KEY"

# 5. Bucket + permissions.
garage -c "$CONFIG" bucket info "$PF_S3_BUCKET" >/dev/null 2>&1 || \
  garage -c "$CONFIG" bucket create "$PF_S3_BUCKET"
garage -c "$CONFIG" bucket allow --read --write --owner "$PF_S3_BUCKET" --key detag-dev

# 6. Foreground server (proper PID 1 + signal handling).
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
exec garage -c "$CONFIG" server
