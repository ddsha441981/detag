# Functional Requirements

IDs are stable. Every phase ticket traces to one or more FRs. "SHALL" = must ship in v1.

## A. Uploads & intake

| ID | Requirement |
| --- | --- |
| FR-U-01 | User SHALL upload images via drag-drop, file picker, or clipboard paste. |
| FR-U-02 | User SHALL upload videos via resumable chunked upload (S3 multipart) that survives network interruption. |
| FR-U-03 | System SHALL reject unsupported formats by magic bytes, not extension, with error code PF-2001. |
| FR-U-04 | System SHALL enforce plan file-size and dimension limits before job creation. |
| FR-U-05 | System SHALL record sha256 and perceptual hash of every input for dedupe and abuse analytics. |
| FR-U-06 | System SHALL extract and persist EXIF/ICC/container metadata at intake for later policy decisions. |
| FR-U-07 | Uploads unused for 24h SHALL be garbage-collected. |
| FR-U-08 | Every upload SHALL be linked to an ownership attestation record (see FR-C-01). |

## B. Jobs & pipeline

| ID | Requirement |
| --- | --- |
| FR-J-01 | User SHALL create a job from a completed upload with params: `mode` (auto/manual/detect_only/metadata_only), optional `hints` (boxes/polygons), `format_policy`, `strip_metadata` (EXIF), `provenance` (preserve default; strip is gated). |
| FR-J-02 | Job creation SHALL accept an `Idempotency-Key`; duplicate creation returns the original job. |
| FR-J-03 | Job SHALL expose states: queued, running, succeeded, failed, cancelled, quality_failed, partial (batch). |
| FR-J-04 | Job SHALL emit progress events (stage + percent) consumable via SSE and polling. |
| FR-J-05 | User SHALL cancel a queued/running job; running jobs cancel at cooperative checkpoints. |
| FR-J-06 | User SHALL retry a failed job (new attempt row, original preserved). |
| FR-J-07 | Jobs SHALL run with per-plan concurrency and priority lanes; Free lanes can be slower. |
| FR-J-08 | On quality-gate failure the system SHALL auto-retry once with the stronger backend, then return status `quality_failed` with the full report — never silently degrade. |
| FR-J-09 | Results SHALL be served via signed URLs with per-plan TTL (6/24/72h) and then deleted. |
| FR-J-10 | Batch jobs (ZIP in, ZIP out) SHALL report per-file status and support partial success. |
| FR-J-11 | Completed/failed jobs SHALL trigger user webhooks (HMAC-signed, retried, delivery-logged) for entitled plans. |

## C. Detection, removal, quality

| ID | Requirement |
| --- | --- |
| FR-D-01 | System SHALL detect common AI-generation badges/marks via multi-scale template matching (deterministic, versioned template pack). |
| FR-D-02 | System SHALL detect overlaid text marks via an ONNX text detector (DBNet-class) routed through a pluggable runtime. |
| FR-D-03 | System SHALL accept user hint regions and merge them with automatic detections in `manual` mode. |
| FR-D-04 | `detect_only` SHALL return a mask image + detection JSON without altering pixels. |
| FR-D-05 | Removal SHALL use LaMa-class ONNX inpainting tiled for large images, with an in-house diffusion fallback. |
| FR-D-06 | Engine SHALL be deterministic for identical input + params + model version (bit-identical output). |
| FR-D-07 | Output SHALL preserve input format, resolution, ICC profile, and (policy-applied) EXIF; see FR-Q-*. |
| FR-D-08 | Video removal SHALL process keyframe detections with mask tracking across frames, re-encode H.264 CRF<=16, and stream-copy audio bit-exact. |
| FR-D-09 | `metadata_only` mode SHALL stream-copy A/V untouched (true lossless) and only rewrite container metadata. |
| FR-Q-01 | Every job SHALL compute masked-region quality metrics: SSIM >= 0.98 and PSNR >= 38 dB on unmasked pixels (images); VMAF >= 93 (video). |
| FR-Q-02 | Quality report SHALL be persisted, returned in the job result, and rendered in UI. |
| FR-Q-03 | Encode policies: JPEG re-encode q >= 95 4:4:4; PNG lossless; WebP q >= 90; video CRF <= 16 preset slow; audio `-c:a copy`. |

## D. Identity & access

| ID | Requirement |
| --- | --- |
| FR-A-01 | Email + password registration with Argon2id, email verification required before job creation. |
| FR-A-02 | JWT access token 15 min + rotating refresh token 30 days with reuse detection; web uses httpOnly cookie, API uses Bearer. |
| FR-A-03 | Password reset (single-use, 30-min token), email change with re-verification, account deletion with 30-day grace. |
| FR-A-04 | Brute-force protection: per-IP and per-account rate limits with lockout backoff. |
| FR-A-05 | Session listing and per-device revocation. |
| FR-A-06 | API keys: `pf_live_`-prefixed, sha256-stored, scoped, rotatable, revocable, per-key rate limits. |
| FR-A-07 | RBAC roles: user, support (read-only), admin. Privileged actions audited. |
| FR-A-08 | Support impersonation (time-boxed, reason required, fully audited, banner-shown). |

## E. Plans, quotas, billing

| ID | Requirement |
| --- | --- |
| FR-P-01 | Plans (defaults, owner-changeable): Free (10 images/day, no video, 25 MB, 6h TTL, 1 concurrent), Pro (500 images/day, 60 video-min/day, 1 GB, 24h TTL, 3 concurrent, API keys + webhooks), Business (5,000 images/day, 300 video-min/day, 4 GB, 72h TTL, 10 concurrent, priority lane, batch). |
| FR-P-02 | Quota enforcement SHALL be atomic (Redis counters, UTC-day reset) and surface remaining quota in headers and UI. |
| FR-P-03 | Stripe checkout, billing portal, subscription lifecycle (trial/active/past_due/canceled), invoices list. |
| FR-P-04 | Billing webhooks SHALL be signature-verified and idempotent (event table). |
| FR-P-05 | A `BillingProvider` trait SHALL allow a Razorpay adapter behind a feature flag without touching core code. |
| FR-P-06 | Overage SHALL reject with 429 + upgrade hint, never silently degrade service. |
| FR-P-07 | Usage metering SHALL count images and video-seconds per day per user, reconciled nightly. |

## F. Console (user-facing web) & admin panel

| ID | Requirement |
| --- | --- |
| FR-W-01 | Upload-centric console: dropzone, staged progress (upload → queue → detect → inpaint → encode → QC), before/after compare, download, job history. |
| FR-W-02 | Pricing page with plan matrix; billing portal link; usage meters. |
| FR-W-03 | Quality report card in UI (metric chips, pass/fail, before/after of edited regions). |
| FR-W-04 | Admin panel: dashboard (jobs today, success rate, p95, queue depth, MRR, active users), user management (search, suspend, quota grant, impersonate), job browser (inspect, reprocess, cancel), feature-flag editor, plan editor, audit viewer, abuse-report queue, attestation review queue, system health page. |
| FR-W-05 | Admin actions SHALL all write audit entries with actor, reason, before/after. |
| FR-W-06 | Public pages: landing, docs quickstart (curl/TS/Python), OpenAPI viewer, changelog, status, AUP/ToS/Privacy. |

## G. Compliance & safety

| ID | Requirement |
| --- | --- |
| FR-C-01 | Every job creation SHALL require an explicit ownership attestation ("I own or have rights to this content and any mark originates from my own AI generation"), stored with timestamp, IP-hashed, user, upload id. |
| FR-C-02 | AUP enforcement: blocklisted signals (stock-agency template matches, report abuse) route to a review queue; violation leads to warning → suspension → termination. |
| FR-C-03 | All privileged and provenance-related actions SHALL append to an append-only audit log (retained 365 days). |
| FR-C-04 | Provenance handling default: PRESERVE C2PA manifests; strip capability is feature-flagged OFF and requires owner legal sign-off + per-job attestation + audit (see `05-compliance-aup.md`). |
| FR-C-05 | GDPR: data export endpoint, delete-cascade verification, retention jobs (results per plan TTL, logs 90d, audit 365d). |
| FR-C-06 | Abuse report intake endpoint + admin queue with SLA tracking. |

## H. Platform

| ID | Requirement |
| --- | --- |
| FR-X-01 | REST API `/v1` fully documented via OpenAPI (utoipa), versioned, error model problem+json with PF-codes. |
| FR-X-02 | Health (`/healthz`), readiness (`/readyz`), Prometheus `/metrics` (internal). |
| FR-X-03 | Structured logs with request-id and job-id correlation; OpenTelemetry traces. |
| FR-X-04 | Deployment: single `docker compose up` brings up api, worker, web, postgres, redis, minio, prometheus, grafana. |
