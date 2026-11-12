# Tech Stack (decisions + constraints)

Agent may bump minor versions freely; majors require an ADR. Rust toolchain pinned in `rust-toolchain.toml`.

## Backend

| Concern | Choice | Rationale / constraint | Alternative rejected |
| --- | --- | --- | --- |
| HTTP | **Axum 0.8** | tower ecosystem, SSE support, typed extractors | actix-web (actor overhead), rocket |
| Async runtime | **tokio** (current stable) | default for axum/apalis | smol |
| DB access | **sqlx 0.8** (Postgres 16) | compile-time checked queries, migrations built-in | diesel (sync-heavy), sea-orm (layer churn) |
| Queue/jobs | **apalis 0.6** (Redis-backed) | Rust-native, retries, scheduled jobs | custom DB queue (later scale ADR), sidekiq-like crates |
| Cache/counters | **Redis 7** | quota counters, rate limits, SSE pubsub | in-proc (breaks NFR-10) |
| Auth | **jsonwebtoken** + **argon2** (Argon2id) | standard, audited | bcrypt (fine but argon2id preferred) |
| OpenAPI | **utoipa** + swagger UI | spec stays in-code | manual spec drift |
| Validation | **validator** crate + magic-byte sniffing | declarative | hand-rolled |
| Errors | **thiserror** + problem+json mapper | FR-X-01 | anyhow in API surface |
| Telemetry | **tracing** + **tracing-opentelemetry** + prometheus exporter | NFR-62 | log-only |
| Payments | **async-stripe** behind `BillingProvider` trait | FR-P-05 | direct REST calls |
| Email | **lettre** + tera templates | outbox pattern | third-party SaaS lock-in |

## Engine (the actual watermark removal)

| Concern | Choice | Rationale | Alternative rejected |
| --- | --- | --- | --- |
| ONNX runtime | **ort 2.x** (onnxruntime) | mature, CPU+GPU, model zoo compatible | tract (pure-Rust fallback behind feature flag — ADR if ort build pain on target VPS) |
| Text detection | DBNet-class detector ONNX (PaddleOCR-det exported) | solid, small, exportable | EAST (older), tesseract (wrong tool) |
| Inpainting primary | **LaMa (big-lama) ONNX**, tiled <= 1024 px with overlap blend | high quality for large masks | diffusion samplers (slow on CPU) |
| Inpainting fallback | in-house push-pull diffusion inpaint (deterministic, ~500 LOC, property-tested) | no-opencv build pain; testable | opencv crate (heavy system dep) — ADR documented |
| Template matching | normalized cross-correlation, multi-scale, `imageproc` | deterministic, fast, unit-testable | ML badge detector (Phase 6+ once dataset exists) |
| Mask ops | `imageproc` morphology + custom feather/blend | pure Rust | opencv |
| EXIF/ICC | **kamadak-exif**; ICC passed through as blob | FR-D-07 | exiftool subprocess (fine for ops, not hot path) |
| Video | **ffmpeg/ffprobe** (system binary via ffmpeg-sidecar or package), serde-parsed ffprobe JSON | industry standard | gstreamer |
| Video metrics | libvmaf-enabled ffmpeg build; MS-SSIM fallback implemented in-crate | quality-policy §3 | no-gate (forbidden) |
| Perceptual hash | custom (aHash/dHash via imageproc) | FR-U-05 | — |

**Model governance:** only permissively licensed, redistributable model files; license audit ticket in Phase 2; models pinned by sha256 in a registry file; no scraping of watermark assets — templates synthesized in-house (render text/badges onto clean backgrounds).

## Frontend (`web/`)

| Concern | Choice |
| --- | --- |
| Framework | Next.js 15 App Router, TypeScript strict |
| Styling | Tailwind + shadcn/ui subset (button, card, dialog, table, toast, slider, progress) |
| State | server components + minimal client state (zustand where needed) |
| Uploads | custom uploader: multipart presign + resume; SSE consumer for progress |
| Auth | httpOnly cookie session (refresh) + access token in memory |
| e2e | Playwright |
| Analytics | self-hosted cookie-less (umami), no PII events |

## Infra

| Concern | Choice |
| --- | --- |
| Containers | multi-stage Dockerfiles (rust:slim builder → distroless/slim runtime) |
| Orchestration | docker-compose (dev + prod profiles); k8s deliberately out of v1 |
| TLS/proxy | caddy (auto-HTTPS) in prod compose |
| CI | GitHub Actions: fmt, clippy `-D warnings`, cargo-deny, test matrix, llvm-cov, docker build, gitleaks, OpenAPI diff check |
| Load testing | k6 scripts in `ops/loadtest/` |
| Metrics | Prometheus + Grafana (dashboards as code in `ops/grafana/`) |

## Config surface

All runtime config via `PF_*` env vars (see runbook). Feature flags: static (env) + DB-backed (`feature_flags` table, cached in Redis 30 s) — admin-editable at runtime.

## Explicit non-goals for v1

Kubernetes, microservice split beyond crate boundaries, GraphQL, multi-region, websocket-first UX (SSE suffices), native mobile.
