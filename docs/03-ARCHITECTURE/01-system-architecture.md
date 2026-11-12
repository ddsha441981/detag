# System Architecture

## Component diagram

```
                      ┌──────────────────────────────────────────────┐
                      │                  VPS / host                   │
 ┌────────┐  HTTPS    │  ┌─────────┐   jobs    ┌──────────────────┐  │
 │  Web   ├───────────┼─►│ api     ├──────────►│ redis (queues,    │  │
 │ Next.js│           │  │ axum    │◄──────────│ rate, cache, SSE) │  │
 │ console│           │  │ :8080   │  enqueue  └────────┬─────────┘  │
 └────────┘           │  └──┬───┬──┘                     │ dequeue     │
                      │     │   │ presigned   ┌──────────▼─────────┐  │
 ┌────────┐  webhooks │     │   └────────────►│ minio / S3 storage │  │
 │ Stripe ├───────────┼─────┘                 └──────────┬─────────┘  │
 └────────┘           │                                  │ objects    │
                      │  ┌───────────────────────────────▼─────────┐  │
 ┌────────┐  SMTP     │  │ workers (apalis)                        │  │
 │ Mailer ├◄──────────┼──┤ detect → mask → inpaint → encode → QC   │  │
 └────────┘           │  │ crates/engine (ort ONNX, ffmpeg)        │  │
                      │  └───────┬─────────────────────────────────┘  │
                      │          │ metrics/traces                     │
                      │  ┌───────▼──────┐  ┌────────────┐  ┌────────┐ │
                      │  │ postgres :5432│  │ prometheus │  │ grafana│ │
                      │  └──────────────┘  └────────────┘  └────────┘ │
                      └──────────────────────────────────────────────┘
```

## Crates / apps (cargo workspace)

| Member | Responsibility |
| --- | --- |
| `apps/api` | Axum HTTP server: auth, uploads, jobs, billing, admin, SSE, webhooks receiver |
| `apps/worker` | Apalis workers: pipeline stages, cleanup, reconciliation, email send |
| `crates/core` | Domain types, error taxonomy, config, audit events, job state machine |
| `crates/engine` | Detection + mask ops + inpainting + encode + QC metrics (pure library, ONNX via `ort`, ffmpeg via subprocess) |
| `crates/storage` | `StorageProvider` trait: S3-compatible (MinIO local, R2/S3 prod), presign, multipart |
| `crates/billing` | `BillingProvider` trait + Stripe adapter + Razorpay adapter (flagged) |
| `crates/mailer` | SMTP + template rendering, outbox pattern |
| `web/` | Next.js 15 console (user + admin), talks only to `/v1` API |
| `ops/` | compose files, grafana dashboards, prometheus config, runbooks |

## Key flows

### Upload → job → result (image)
1. `POST /v1/uploads` → api creates upload row, returns multipart presigned part URLs.
2. Client uploads parts directly to storage (api stays out of the byte path), `POST /v1/uploads/{id}/complete`.
3. `POST /v1/jobs` with `upload_id` + params + attestation → quota check → job row (queued) → enqueue.
4. Worker: intake sniff/decode → detect (templates → ONNX text det → hints merge) → mask plan → tiled inpaint → encode (format-preserving) → QC metrics → gate → artifacts to storage → job `succeeded` + report.
5. Client polls `GET /v1/jobs/{id}` or subscribes SSE `GET /v1/jobs/{id}/events`; downloads via signed URL (TTL per plan).

### Billing webhook
Stripe → `POST /v1/webhooks/stripe` (signature verify, event table idempotency) → subscription state machine → entitlement cache invalidation → emails (dunning/confirmation).

### Admin
Web admin routes → same `/v1` API with role guard → every mutating action writes audit entry.

## Failure modes & handling

| Failure | Behavior |
| --- | --- |
| Worker killed mid-job | apalis visibility timeout re-queues; job resumes at stage boundary (idempotent stages) |
| Redis down | job creation 503 + Retry-After; status reads served from Postgres cache row |
| Postgres failover/restart | api reconnect loop; in-flight requests fail fast with PF-5001 |
| Storage unavailable | jobs queue-hold (`storage_hold`), alerts fire |
| Model download corrupt | checksum verify at boot → refuse start, alert |
| Quality gate fail after retry | `quality_failed` + report; no silent output |

## Security boundaries

- Web/clients never touch storage credentials; only presigned, scoped, TTL-bound URLs.
- API keys and JWTs checked at a single auth extractor; role guard middleware for admin/support.
- Worker runs engine models; it does not accept inbound network connections.
- Storage buckets: private, per-environment prefix, lifecycle rules for TTL deletion.

## Environments

| Env | Compose file | Notes |
| --- | --- | --- |
| dev | `ops/compose.dev.yml` | hot-reload, MailHog, MinIO console exposed |
| staging | `ops/compose.prod.yml` | same images as prod, test keys |
| prod | `ops/compose.prod.yml` | caddy TLS front, backups cron |

## Scale path (documented, not built in v1)

1. Vertical: bigger worker + more replicas (NFR-10).
2. Storage/CDN: presigned URLs already offload bytes; add CDN in front of results.
3. DB: read replica for analytics/admin queries.
4. GPU: `PF_ENGINE_GPU=1` + model warm pool (NFR-63).
