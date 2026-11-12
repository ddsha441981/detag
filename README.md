# Detag

[![CI](https://github.com/ddsha441981/detag/actions/workflows/ci.yml/badge.svg)](https://github.com/ddsha441981/detag/actions/workflows/ci.yml)

Detag removes watermarks, badges and overlaid marks from AI-generated
images and videos that the user owns or has rights to — with a hard,
machine-verified promise: the output is indistinguishable from the input
everywhere except the removed regions. No resizing, no recompression soup,
no blurred smudges. Every job returns a quality report (image SSIM ≥ 0.98
and PSNR ≥ 38 dB on unmasked pixels; video VMAF ≥ 93; audio bit-exact).

Rust backend (Axum + Postgres + Redis + S3-compatible storage), LaMa ONNX
inpainting, Next.js console, admin panel, JWT auth, plans + billing,
quality gates, observability. Provenance marks (C2PA) are preserved by
default; stripping them is a gated feature, and invisible-watermark
(SynthID-class) removal is research-only — see the compliance doc.

## Docs

The full build plan and spec live in [`docs/`](docs/) (the agent kit):
start at [`docs/00-AGENT-BOOT.md`](docs/00-AGENT-BOOT.md), then
[`docs/01-WORKFLOW.md`](docs/01-WORKFLOW.md). Product overview:
[`docs/02-REQUIREMENTS/01-product-overview.md`](docs/02-REQUIREMENTS/01-product-overview.md).
Compliance and acceptable use:
[`docs/02-REQUIREMENTS/05-compliance-aup.md`](docs/02-REQUIREMENTS/05-compliance-aup.md).

## Develop

```sh
cp .env.example .env   # dev placeholders only, never real secrets
just dev               # build + start api/worker, wait for /healthz
just verify            # fmt + clippy -D warnings + tests
just deny              # RUSTSEC advisories + license allow-list
just secrets           # gitleaks full-repo scan
just env-check         # .env.example drift vs code + runbook
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for toolchain, commits and the
definition of done.

## Ops quickstart

Three commands to a working stack (dev placeholders only — never real
secrets; the full deploy is the runbook's "First deploy" section):

```sh
cp .env.example .env   # 1. config (dev placeholders)
just dev               # 2. build + start api/worker, migrate + seed, wait /healthz
just smoke --skip-up   # 3. prove it: healthz/readyz, ping job through the worker
```

`just dev` uses compose (base+dev) when docker runs and falls back to
native postgres/redis + native binaries on this VM (DOCKER-001 —
containers cannot start here). On a docker machine use `just smoke`
(full compose up) instead of `--skip-up`. If smoke fails, diagnose in
this order: `docker compose ps` → that service's logs → `curl
localhost:8080/readyz` (the per-component table names the down
dependency). Full detail:
[`docs/06-DELIVERY/03-deploy-runbook.md`](docs/06-DELIVERY/03-deploy-runbook.md).

### Ports (dev stack)

| Service | Port | Notes |
| --- | --- | --- |
| api | 8080 | `http://127.0.0.1:8080` — `/healthz`, `/readyz`, `/docs` (Swagger, dev only) |
| web (console placeholder) | 3000 | dev only; the real console lands in Phase 4 |
| postgres | 5432 | dev only — internal network in prod |
| redis | 6379 | dev only — internal network in prod |
| minio (S3) | 9000 | dev only — internal network in prod |
| minio (console) | 9001 | dev only, not published in prod |
| mailhog (UI) | 8025 | dev only; SMTP :1025 internal — dev mail never leaves the stack |
| api metrics | 9090 | dev only |
| worker metrics | 9092 | dev only |
| caddy | 80 / 443 | prod only — the only published ports in prod; auto-TLS for `PF_DOMAIN` |

### Troubleshooting (top 5)

| Symptom | Cause | Fix |
| --- | --- | --- |
| `docker: not found`, or containers fail to start | No working docker on this VM (DOCKER-001) | Use `just dev` — it falls back to the native stack automatically; live compose waits for CI |
| `just dev` / `just verify` fails on postgres or redis refused | A VM reset wiped the native services (ASSUMPTION-010) | `pg_ctlcluster 16 main start`, recreate the `pf` role/db from `.env`, restart redis detached (`setsid redis-server /etc/redis/redis.conf --daemonize yes --save "" --maxmemory 256mb --maxmemory-policy allkeys-lru`) — see `docs/09-STATE/ISSUES.md` |
| `/readyz` reports storage or models DOWN | Expected on the native stack (no MinIO here); under compose the bootstrap didn't complete | Native: ignore the warning — smoke fails only when db or redis is down. Compose: `docker compose ps` + `docker compose logs bootstrap` |
| Port already in use (`:8080`, `:5432`, `:6379`) | A leftover stack (or host postgres/redis) already holds it | Stop the other stack (`docker compose down`, or kill the leftover binary); or change the port in `.env` (`PF_API_PORT`) |
| `just verify` red on `sigterm_drains_in_flight_request_and_exits_zero` | Known flaky race, not a regression (FLAKY-001) | Re-run `just verify` until green; the test passes in isolation |

## License

Apache-2.0. See [`LICENSE`](LICENSE).
