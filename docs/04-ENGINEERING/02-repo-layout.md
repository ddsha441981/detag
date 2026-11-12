# Repo Layout (target end-state Phase 0/1)

```
pureframe/
├── Cargo.toml                  # workspace
├── rust-toolchain.toml
├── deny.toml                   # cargo-deny: advisories, licenses
├── .env.example                # every PF_* var with dev defaults
├── justfile                    # just dev|verify|test|bench|migrate|seed|smoke
├── migrations/                 # sqlx migrations
├── apps/
│   ├── api/                    # axum server (main.rs, routes/, services/, repos/, middleware/)
│   └── worker/                 # apalis workers (pipeline.rs, cleanup.rs, reconciliation.rs, email.rs)
├── crates/
│   ├── core/                   # config, error taxonomy, domain types, job state machine, audit
│   ├── engine/                 # detect/ (templates.rs, textdet.rs, hints.rs), mask/, inpaint/ (lama.rs, diffusion.rs),
│   │                           # encode/, metrics/ (ssim.rs, psnr.rs, vmaf.rs), registry.rs (model pinning), cli/
│   ├── storage/                # provider trait + s3 impl + presign + keys
│   ├── billing/                # provider trait + stripe/ + razorpay/ (flagged)
│   └── mailer/                 # templates/ + smtp.rs + outbox drain
├── fixtures/                   # golden media: images/, videos/, masks/, templates/ (synthesized)
├── ops/
│   ├── compose.dev.yml  compose.prod.yml
│   ├── caddy/Caddyfile
│   ├── grafana/dashboards/*.json   prometheus/prometheus.yml
│   ├── loadtest/*.js               # k6
│   └── backup.sh
├── web/                        # Next.js 15 (app/, components/, lib/api/, e2e/)
├── docs/                       # this kit (or kit lives at repo root — agent normalizes on first run)
└── 09-STATE/                   # PROGRESS.md, DECISIONS.md, ISSUES.md, NIGHT-REPORTS/
```

## Ownership map (where things go)

| Change type | Location |
| --- | --- |
| New endpoint | `apps/api/src/routes/` + service + repo + OpenAPI derive + integration test — checklist: `04-adding-an-endpoint.md` |
| New job stage | `apps/worker/src/pipeline.rs` + `crates/engine` unit + state machine update |
| New model | `crates/engine/src/registry.rs` (sha256 pin) + `fixtures/` golden pair + license note |
| New table/index | `migrations/` + `03-ARCHITECTURE/04-data-model.md` same commit |
| New UI page | `web/app/` + a11y pass + e2e when flow-critical |
| New flag | env default in `.env.example` + `feature_flags` seed + admin editor support |
| Dashboard/panel | `ops/grafana/dashboards/` |

## First-run normalization

If this kit sits at repo root, the agent may keep `docs/` pointing at these files or move them under `docs/` — either is fine, but `PROGRESS.md` must record which layout was chosen so later runs do not hunt for files.
