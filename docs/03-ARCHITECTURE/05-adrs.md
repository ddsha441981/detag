# Architecture Decision Records (initial set)

New ADRs are appended to `09-STATE/DECISIONS.md` (same template). Status: accepted unless noted.

## ADR-0001 — Rust monolith with crate boundaries (no microservices)
**Context:** solo-scale team (one agent), single VPS target. **Decision:** one binary pair (api, worker) over shared crates; boundaries enforced by crate graph. **Consequences:** simple deploys; later service extraction stays possible because engine/storage/billing are libraries.

## ADR-0002 — Axum + tokio + sqlx/apalis/Redis
**Context:** need SSE, multipart presign flows, background workers, compile-time query checks. **Decision:** per `02-tech-stack.md`. **Consequences:** one ecosystem (tower/tokio); queue portability later = new adapter behind `JobQueue` trait if ever needed.

## ADR-0003 — ONNX via `ort`; tract as pure-Rust fallback feature
**Context:** models (DBNet det, LaMa inpaint) ship as ONNX. **Decision:** `ort` default; if onnxruntime build/target friction appears, `PF_ENGINE_BACKEND=tract` keeps a reduced pipeline alive. **Consequences:** two backends to smoke-test; CI runs engine tests on ort only.

## ADR-0004 — In-house diffusion inpaint as fallback (no OpenCV dep)
**Context:** opencv crate pulls heavy system deps, breaks slim containers. **Decision:** fallback inpainting implemented in-crate (deterministic, property-tested); opencv never enters the build. **Consequences:** slightly weaker fallback quality — acceptable because it only runs when LaMa is unavailable, and gates still apply.

## ADR-0005 — Detection: template pack first, ML text-det second, learned badge detector later
**Context:** known AI badges are few and stable; text marks vary. **Decision:** v1 = deterministic template matching (synthesized pack) + DBNet ONNX text detection + user hints. Training a custom YOLO badge detector is deferred until >= 500 labeled samples exist (Phase 6+). **Consequences:** no dataset cold-start problem; detection quality is measurable per source (template vs text vs hint) in reports.

## ADR-0006 — Format-preserving encode, EXIF stripped, ICC kept, C2PA preserved
**Context:** quality policy + privacy. **Decision:** images re-encoded at policy quality (JPEG q>=95 4:4:4, PNG lossless verify, WebP q>=90); EXIF stripped by default with orientation applied; ICC blob preserved; C2PA manifests preserved (strip = gated Phase 6). **Consequences:** predictable outputs; privacy-safe default; provenance default is conservative.

## ADR-0007 — Billing: provider trait, Stripe first, Razorpay flagged adapter
**Context:** likely India-market user; Stripe is the fastest correct path. **Decision:** `BillingProvider` trait; Stripe adapter v1; Razorpay adapter behind flag with webhook HMAC mapping. **Consequences:** no core changes needed to switch/add providers; entitlements live in our DB, not the provider.

## ADR-0008 — Entitlements enforced server-side only
**Context:** client cannot be trusted; quotas are revenue. **Decision:** quota check middleware (Redis atomic counters, UTC-day), entitlement snapshot cached per request; UI meters are cosmetic copies. **Consequences:** race-safe limits; nightly reconciliation job detects drift.

## ADR-0009 — SSE for progress (not WebSockets), polling remains first-class
**Context:** one-way progress events; proxies friendly; API consumers may prefer polling. **Decision:** SSE endpoint + full polling parity; webhooks for machine consumers. **Consequences:** simpler infra; reconnect logic documented in API spec.

## ADR-0010 — Single-writer Postgres; no read replica in v1
**Context:** scale target 100k jobs/day is fine on one writer with correct indexes. **Decision:** vertical scale + indexes; analytics/admin queries may use `SELECT` on same DB until load proves otherwise. **Consequences:** simplest ops; revisit at NFR-12 breach.

## ADR-0011 — Storage keys carry no PII
**Context:** bucket names can leak. **Decision:** keys = `{env}/{yyyy-mm}/{job_id}/{artifact_type}.{ext}`; no emails/usernames in keys or bucket names. **Consequences:** safe defaults even if a URL leaks.

## ADR-0012 — Ownership attestation is a hard, stored, versioned artifact
**Context:** AUP enforceability. **Decision:** per-job attestation row (FR-C-01) with text version; AUP text versioned like ToS. **Consequences:** enforcement actions have evidence; audits possible.
