# Release Checklist (v1.0.0-rc → v1.0.0)

Run top-to-bottom; anything red blocks the release (owner-input items get listed, not skipped).

## Build & CI
- [ ] `just verify` green from clean clone; `cargo deny` clean; gitleaks clean.
- [ ] Full test pyramid green (unit, integration, golden, e2e, property).
- [ ] Coverage floors met (NFR-50); bench snapshot attached, no unexplained >10% regression.
- [ ] OpenAPI diff vs previous release reviewed; breaking changes = version bump decision (v1 freeze policy).

## Schema & data
- [ ] All migrations additive-or-rehearsed; `migrate up` on a prod-shaped copy succeeds; rollback plan written.
- [ ] Retention jobs verified (results TTL, audit 365d, logs 90d).

## Quality & security
- [ ] Golden suite green on release build; thresholds unchanged since last release or ADR'd.
- [ ] Security suite (`05-QUALITY/03-security-testing.md`) green; zero open S1/S2.
- [ ] Dependency advisories: zero known-vulnerable versions shipped.

## Product & compliance
- [ ] Pricing/plans match owner-confirmed values (config + Stripe dashboard).
- [ ] ToS/AUP/Privacy pages deployed; attestation flow e2e tested.
- [ ] Marketing-honesty copy check done (no invisible-mark removal claims anywhere).
- [ ] Feature flags reviewed: `provenance_strip` OFF, `razorpay` OFF (until owner enables), GPU OFF by default.
- [ ] Admin panel: all FR-W-04 features present and matrix-tested.

## Deploy & ops
- [ ] `ops/compose.prod.yml` + caddy TLS rehearsed on clean VPS (NFR-60).
- [ ] Backups cron active; restore drill executed once (NFR-22).
- [ ] Alert rules loaded (queue depth, error rate, job failure, disk, redis down); test alert fired.
- [ ] `/healthz` `/readyz` `/metrics` wired to uptime monitor.
- [ ] Model registry checksums verified at boot on prod image.

## Ship
- [ ] Tag `v1.0.0-rc1` → staging deploy → 24h soak (error budget respected).
- [ ] CHANGELOG written from conventional commits; night report summarizes.
- [ ] Owner sign-off recorded in `09-STATE/DECISIONS.md` (live keys, domain, final pricing).
- [ ] Tag `v1.0.0`, prod deploy, smoke job on prod, announcement draft (honest copy) prepared for owner.
