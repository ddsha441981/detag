# Non-Functional Requirements

IDs `NFR-*`, stable, testable. These are acceptance criteria, not aspirations.

## Performance

| ID | Requirement | Verification |
| --- | --- | --- |
| NFR-1 | API p99 latency <= 250 ms for auth/jobs/status endpoints (excluding uploads) | k6 suite, Phase 5 |
| NFR-2 | Image job p95 <= 30 s at <= 4K on 4 vCPU CPU-only worker | bench harness |
| NFR-3 | Video throughput >= 0.5x realtime (1080p) CPU, >= 2x with GPU enabled | bench harness |
| NFR-4 | Upload init p95 <= 300 ms; part presign p99 <= 150 ms | k6 |
| NFR-5 | SSE event delivery latency p95 <= 500 ms; reconnect <= 2 s | integration test |
| NFR-6 | Worker startup (model warm) <= 60 s; model download verified at boot | compose smoke |
| NFR-7 | Engine determinism: identical input+params+model = bit-identical output | determinism test |

## Scalability

| ID | Requirement |
| --- | --- |
| NFR-10 | Workers scale horizontally (>= 3 replicas) with no config change beyond replica count. |
| NFR-11 | Queue lanes prevent starvation: a Free-lane flood cannot delay Pro jobs > 60 s p95. |
| NFR-12 | DB: single-writer Postgres to 100k jobs/day with correct indexes; slow-query log clean (< 50 ms) at that load. |
| NFR-13 | Redis failure degrades gracefully: API returns 503 + Retry-After on job creation, reads stay up. |

## Reliability

| ID | Requirement |
| --- | --- |
| NFR-20 | Job success >= 97% rolling 7-day; quality first-pass >= 90%. |
| NFR-21 | Kill -9 of a worker mid-job SHALL NOT lose the job: it re-enters queue via visibility timeout. |
| NFR-22 | Daily automated backups (Postgres dump + storage sync), restore drill documented and rehearsed once in Phase 5. |
| NFR-23 | Zero-downtime deploy path: additive migrations (expand-contract), health-gated rollout. |
| NFR-24 | Idempotency: duplicate webhook/delivery/message processing causes no duplicate effects. |

## Security

| ID | Requirement |
| --- | --- |
| NFR-30 | OWASP ASVS L2 subset self-audit passes (authz matrix, session, upload abuse, headers). |
| NFR-31 | All admin endpoints deny by default; authorization tested per-role (IDOR sweep). |
| NFR-32 | Upload abuse suite rejected: zip bombs, decompression bombs (PNG), polyglots, SVG (rejected outright), oversized dimensions. |
| NFR-33 | Secrets only via env; rotation runbook exists; gitleaks CI clean. |
| NFR-34 | Signed URLs: scoped, short-TTL, single-use where supported. |
| NFR-35 | Webhook endpoints are HTTPS-only, SSRF-guarded (private CIDR denied). |

## Privacy & data governance

| ID | Requirement |
| --- | --- |
| NFR-40 | Result artifacts expire per plan TTL and are hard-deleted; audit log 365 d; app logs 90 d; webhook delivery logs 30 d. |
| NFR-41 | GDPR export and delete-cascade verified by tests. |
| NFR-42 | No PII in logs (emails, raw IPs beyond abuse counters, file contents). |
| NFR-43 | EXIF stripped on output by default (privacy); orientation applied; ICC preserved. C2PA manifests preserved unless gated strip is enabled. |

## Maintainability

| ID | Requirement |
| --- | --- |
| NFR-50 | Coverage: crates/core >= 85%, crates/engine >= 80%, apps/api >= 75% (llvm-cov, CI-reported). |
| NFR-51 | `cargo clippy -- -D warnings` clean; `cargo deny` (advisories + licenses) clean. |
| NFR-52 | Conventional commits; every commit references ticket ID + FR/NFR. |
| NFR-53 | ADR exists for every durable architectural decision. |
| NFR-54 | MSRV pinned; workspace builds on pinned toolchain via rust-toolchain.toml. |

## Portability & ops

| ID | Requirement |
| --- | --- |
| NFR-60 | Runs anywhere Docker runs: `docker compose up` on a 4 vCPU / 8 GB VPS serves the SLO set for Free+Pro load. |
| NFR-61 | All configuration via `PF_`-prefixed env vars, documented in `06-DELIVERY/03-deploy-runbook.md`. |
| NFR-62 | Observability: request/job traces, queue-depth gauge, per-stage histograms, dashboards as code in `ops/grafana/`. |
| NFR-63 | API images < 150 MB compressed; GPU optional (feature flag), CPU is the default supported path. |

## Cost

| ID | Requirement |
| --- | --- |
| NFR-70 | Cost-per-job model documented (CPU-seconds, storage-days, egress) and tracked in the Phase 5 capacity plan. |
