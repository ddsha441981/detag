# DECISIONS.md — ADR log + owner-input queue (append-only)

> Agent: append, never rewrite history. Owner: edit `owner-input-needed` items by adding `ANSWER:` lines; the agent applies answers next run.

## ADR template

```
## ADR-<seq> — <title> (date, phase)
**Context:** ...
**Decision:** ...
**Consequences:** ... (incl. what becomes harder)
```

## ADR log (kit's initial decisions live in 03-ARCHITECTURE/05-adrs.md as ADR-0001..0012)

(appended below by the agent as work proceeds)


## ADR-0013 — Product renamed PureFrame → Detag (2026-11-12, Phase 0)
**Context:** Owner renamed the product and created repo https://github.com/ddsha441981/detag.git (private, empty, default branch main). Kit docs still say PureFrame.
**Decision:** Code/repo/brand use `detag`; kit docs under `docs/` keep the PureFrame name as the spec source until the owner asks for a doc rename. Binary/crate names use `detag-*`. Pricing/plan defaults unchanged (Free ₹0 / Pro ₹499 / Business ₹1,999).
**Consequences:** Docs refer to PureFrame; code refers to Detag. A doc-rename ticket may be added later; no functional impact.

## ADR-0014 — Docker on this VM via systemd + egress proxy, no bridge networking (2026-11-12, Phase 0)
**Context:** VM has no NET_ADMIN capability: stock dockerd fails creating the NAT bridge (iptables permission denied). Egress to registries goes via the VM proxy.
**Decision:** docker.io 29.1.3 installed via apt; daemon runs via systemd with an http-proxy drop-in pointing at the VM egress proxy; daemon.json sets `"iptables": false, "bridge": "none"`. Containers therefore run with `--network host` (compose services use `network_mode: host`) until/unless bridge networking becomes available. Compose plugin v2.40.3 installed.
**Consequences:** Compose files in this repo use host networking in dev; prod compose keeps bridge networks as documented and is validated with `docker compose config` only on this VM. `just smoke` must pass with host networking.

**Amendment (2026-11-12, P0-034 — what it actually took to get dockerd up, and where it still stops):** the ADR's setup had never been applied (`/etc/docker/` did not exist, no proxy drop-in, no compose plugin). Applied now: `/etc/docker/daemon.json` (`iptables:false`, `bridge:none`, plus `storage-driver:vfs` — overlayfs mounts fail with "invalid argument" on this VM — and `exec-opts: native.cgroupdriver=cgroupfs`); proxy drop-in (`http://198.19.0.1:3128`, no credentials); compose plugin v2.40.3 installed from the GitHub release (sha256-verified; not in Ubuntu's repos). Containers still cannot START on this VM (DOCKER-001): runc fails joining the session keyring (no keyctl); crun 1.14.1 (apt) fails the OCI spec version dockerd emits ("unknown version specified"); crun 1.30.1 (GitHub release, `.scratch/tools/`) fixes that but hits the same keyring wall, then sd-bus (no system bus socket), then the cgroup v2 root delegating zero controllers — worked around with a wrapper (`.scratch/tools/crun-no-keyring`: `--no-new-keyring` after create/run, `--cgroup-manager=disabled`) — and finally the network namespace: loopback bring-up fails (`SIOCSIFFLAGS: Operation not permitted`, no NET_ADMIN) and `--network host` fails in dockerd's sandbox creation. So: `docker compose config` validates, images load, volumes create, but no container runs. Dev fallback until this changes: native PostgreSQL 16 on :5432 (role/db `pf`, matching `.env.example`); `just dev` tries compose first and falls back. Live compose verification is deferred to CI (where docker works) and E0.9's smoke (P0-090). Do not re-attempt the runtime rabbit hole without new kernel capabilities.

## ADR-0015 — Toolchain pinned (2026-11-12, Phase 0)
**Context:** Rust 1.99.1 stable was already installed in ~/.cargo/bin (not on PATH by default); `just` 1.42.0 installed to ~/.local/bin.
**Decision:** `rust-toolchain.toml` pins channel `1.99.1`; PATH in CI/dev setup must include ~/.cargo/bin and ~/.local/bin. `CARGO_BUILD_JOBS=2` in `.cargo/config.toml` (4 CPUs, ~7.7 GB RAM) to avoid OOM during parallel rustc.
**Consequences:** Toolchain upgrades are deliberate (ADR on majors).

## ADR-0016 — Push policy for this run (2026-11-12, Phase 0)
**Context:** GitHub access to ddsha441981/detag verified by the owner (private, empty, main). The parent agent batch-pushes commits.
**Decision:** This coordinator commits locally only, one commit per ticket, and never pushes or tags. The parent pushes/tags after the run report.
**Consequences:** Local `git log` is the source of truth until the parent pushes.


## ADR-0017 — `just verify` excludes compose smoke until P0-090 (2026-11-12, Phase 0)
**Context:** Kit DoD defines `just verify` as fmt + clippy + tests + compose smoke. Compose files land in E0.9 (P0-085..P0-090) and Docker Hub pulls through the VM egress proxy fail with EOF/timeouts (5 attempts, overlayfs + vfs drivers); skopeo-to-archive + `docker load` is the working workaround (see ISSUES.md).
**Decision:** Until P0-090, `just verify` = fmt-check + clippy `-D warnings` + `cargo test --workspace`; `just smoke` exists but fails loudly as "pending P0-090"; `just verify-full` = verify + smoke for use once smoke lands. No ticket is marked done on a faked smoke run.
**Consequences:** Early-phase verify is honest but partial; the smoke gap is tracked in ISSUES.md and closed by P0-090.
**Amendment (2026-11-12, P0-090 — smoke landed):** `just smoke` now exists (`scripts/smoke.sh`: compose up -d --wait --wait-timeout 120, migrate, /healthz + /readyz waits, `just smoke-ping`, print URLs) and runs in CI as its own `smoke` job. `just verify` still excludes it — not because smoke is pending, but because this VM has no working docker daemon (DOCKER-001), so smoke would always fail here; `just verify-full` (= verify + smoke) is the full gate on docker-capable machines and in CI. The "no ticket marked done on a faked smoke run" rule stands: the P0-090 AC ("smoke green in CI") stays unticked until the workflow files reach GitHub (CI-WORKFLOWS-001) and the smoke job goes green there.

## ADR-0018 — PF-5003 `CacheUnavailable`: the Redis failover error (2026-11-12, Phase 0)
**Context:** P0-051 ("failover error mapping to typed errors") found no typed error for Redis failures: `AppError` had only `DatabaseUnavailable` (PF-5001, Postgres) and the redis modules each carried their own `Pool`/`Redis` error variants with no mapping to the API taxonomy. PF-5002 was already taken (`InvalidStateTransition`).
**Decision:** New variant `AppError::CacheUnavailable` — PF-5003, HTTP 503, title `cache_unavailable`, safe detail "the cache is temporarily unavailable". `From` impls map `deadpool_redis::PoolError`, `redis::RedisError`, and the `Pool`/`Redis` variants of `CacheError`/`RateLimitError`/`PubSubError` to it; all other module-error variants map to `Internal` (programmer/caller errors, not failover). The provider error is discarded at the boundary — its detail never reaches a problem body.
**Consequences:** Any Redis outage now surfaces as a typed, PII-free 503. The exhaustive taxonomy tests (34 variants) pin the code. If a future ticket needs to distinguish checkout vs command failures, it must add variants, not match on provider strings.

## ADR-0019 — S3 backend: MinIO → Garage v2.5.0 (2026-11-13, Phase 1)
**Context:** CI #5 smoke died because MinIO's distribution is gone on every channel — Hub removed minio/minio, Quay denies anonymous pulls of quay.io/minio/minio and quay.io/minio/mc, dl.min.io answers 410 (open-source server/mc archived). The stack needs an S3-compatible server for dev/smoke/CI.
**Decision:** Garage v2.5.0 (Deuxfleurs, Rust, AGPL-3.0, single static binary, dxflrs/garage on Docker Hub, pinned tag). Single node, replication_factor 1, S3 API on :9000 (endpoint unchanged from the MinIO days), s3_region us-east-1 (must match the api's signing region — Garage rejects a mismatched scope). Layout + deterministic key import (PF_S3_* from env) + bucket + RWO allow run in the garage entrypoint; the 72 h lifecycle ceiling is applied by a separate bootstrap-lifecycle service (amazon/aws-cli, S3 PutBucketLifecycleConfiguration). Buckets are private by default (Garage has no ACLs/policies — the old `mc anonymous set none` step has no equivalent and is not needed). Verified end-to-end locally with the real binary + boto3 before landing (all S3 ops + lifecycle + presigned URLs 200).
**Consequences:** The dev/smoke S3 backend is Garage, not MinIO. The local-dev compose.minio.yml file and the testcontainers live legs still reference MinIO (TESTKIT-MINIO-001, follow-up). If Garage ever becomes unsuitable, the swap point is ops/compose.base.yml + ops/garage.toml + the entrypoint — the api/worker only see a standard S3 endpoint.

## ADR-0020 — Garage RPC secret must be 32 bytes of random hex (2026-11-13, Phase 1)
**Context:** CI #6/#7 smoke died with the garage container stuck unhealthy. Root cause (CI-FAIL-008): `.env.example` held `GARAGE_RPC_SECRET=<redacted>` — literally that string — and Garage refuses to start with anything but 32 bytes of random hex (`Error: Invalid RPC secret key: expected 32 bytes of random hex`, proven with the real v2.5.0 binary).
**Decision:** The dev placeholders in `.env.example` are real 32-byte-hex values (dev-only, same standing as minioadmin; generate fresh ones for prod with `openssl rand -hex 32`). The admin token takes the same format. smoke.sh validates the RPC-secret format before `up` so a future bad placeholder fails fast with a clear message instead of a 120 s timeout, and prints the garage logs on failure.
**Consequences:** Any future change to the GARAGE_* placeholders must keep the 64-hex-char format, or the smoke job (locally and in CI) fails at the guard. The guard does not validate the admin token (Garage accepts any non-empty bearer token there); both are hex here for uniformity.

## ADR-0021 — Password blocklist: zxcvbn's embedded dictionary, not a SecLists download (2026-11-13, Phase 1)
**Context:** P1-026 needs a top-10k common-password blocklist. The natural source is the SecLists top-10000 list from raw.githubusercontent.com, but that host needed the owner's download approval (pending) and the fetch to it timed out on this VM anyway. zxcvbn was already the strength library and embeds a 30,000-entry ranked passwords dictionary (RockYou lineage — the same lineage as the SecLists list).
**Decision:** Blocklist = zxcvbn's embedded dictionary, top 10k by rank: a password is `common_password` when zxcvbn matches it as a single dictionary word covering the whole password with rank <= 10,000 (identified by the feedback warning). Strength gate is zxcvbn score >= 3 (the crate's own "too weak" line). The optional HIBP k-anonymity check stays behind `PF_PASSWORD_PWNED_CHECK` (default off, fail-open on any error) because it contacts a third party per registration/reset.
**Consequences:** No external list file to fetch, version, or bundle — the blocklist ships with the zxcvbn crate. A rank boundary is pinned by test (rank 10023 is NOT blocklisted). If the owner later approves the SecLists import, the swap is one function + a data file; the error code and callers do not change. Recorded as ASSUMPTION-014 in ISSUES.md.

## ADR-0022 — Argon2id defaults m = 64 MiB, t = 3, p = 4 + rehash-on-login (2026-11-13, Phase 1)
**Context:** P1-027's spec sets the Argon2id defaults to m = 64 MB, t = 3, p = 4 — stronger than the OWASP first choice (m = 19 MiB, t = 2, p = 1) that P1-001 shipped. Every account created before this change carries the old params in its stored PHC hash.
**Decision:** Defaults are m = 65536 KiB, t = 3, p = 4 (`PF_ARGON2_*`, still operator-configurable with the same validated ranges). Old hashes are NOT bulk-rewritten: login upgrades them lazily — on a successful login, when the stored PHC params differ from the current policy, the just-verified password is rehashed with the current params and the new hash is written in the same transaction as the session row (`needs_rehash` in `crates/core/src/password.rs`, wired in `apps/api/src/services/login.rs`). The rehash is opportunistic (a hash failure logs a warn and the login still succeeds) and runs before the transaction so it never holds a pool connection. Measured cost (`cargo bench -p detag-core --bench argon2`, criterion, 10 samples, 2026-11-13 on the 1.5-CPU build VM — an upper bound for the 4-vCPU target VPS): hash ~195 ms and verify ~192 ms at the new defaults, vs ~26 ms / ~29 ms at the old ones. The full note (incl. the 64 MiB-per-hash sizing for login bursts) is in the deploy runbook.
**Consequences:** Every login/register pays ~0.2 s of Argon2id (was ~0.03 s); a pre-P1-027 user pays one extra ~0.2 s hash on their first login after the upgrade. The sync-hash-on-async-worker concern (PERF-001) covers this path too — the rehash follows register's existing inline-hash pattern rather than inventing a new one. If the cost ever needs cutting, the operator lowers `PF_ARGON2_*` in the deployment env; the fleet then converges via rehash-on-login with no migration.

## ADR-0023 — Email send caps enforced at the drain, not at enqueue (2026-11-14, Phase 1)
**Context:** P1-031's spec sets per-recipient hourly caps per template class (verify 3, reset 3, marketing 0) with abuse-log entries on trips. The cap is on SENDS, and mails are enqueued from many services (register, resend, forgot, email-change, lockout, …).
**Decision:** Enforce in the drain (`crates/mailer/src/drain.rs`), the single choke point every queued mail passes through — not at each enqueue site, where a new service would silently dodge the cap. The drain counts sent rows for the recipient's hash + the class's templates in the last hour (`email_outbox::count_sent_recent_for_recipient`; `sent_at` in-window, so queued-but-unsent rows spend nothing) and, on a trip, parks the row (the mail is dropped, not retried into the next hour — its token would be stale by then) and writes an abuse audit row (`email.rate_limit_tripped`, hash + template + class + cap + count, never a raw address, NFR-42). A failed audit insert logs loudly but the row is still parked — enforcement never depends on the log write. A count-query failure is transient (the row is rescheduled, not lost). Caps are operator config (`PF_MAILER_VERIFY_CAP_PER_HOUR` 3 / `PF_MAILER_RESET_CAP_PER_HOUR` 3 / `PF_MAILER_MARKETING_CAP_PER_HOUR` 0). Transactional alerts (`lockout-notice`, `new-login-digest`, …) are never capped — a lockout notice must reach the user precisely when the account is being hammered. Marketing is 0 with no marketing template shipping in v1, so any marketing-class row is parked on sight until the operator deliberately raises the cap.
**Consequences:** The endpoint-level limits stay as the first line (resend 3/hour per user+template, P1-002; forgot 10 burst / 1 per min per account, P1-025); the drain cap is the backstop that catches cross-endpoint combinations (register + resends + an email change adding up to more than 3 verification mails to one address in an hour). A capped mail's caller already got its 204 — the drop is visible only in the audit log and the row's `last_error`, which is the same visibility the lockout-notice path already has.

## owner-input-needed queue

| # | Date | Question | Temporary default in use | Owner answer |
| --- | --- | --- | --- | --- |
| 1 | kit start | Product name / brand | PureFrame (working title) | Detag — owner decided 2026-11-12, repo ddsha441981/detag |
| 2 | kit start | Final pricing (INR/USD) | Free ₹0 / Pro ₹499 / Business ₹1,999 | pending |
| 3 | kit start | Live payment keys (Stripe/Razorpay) | TEST MODE only | pending |
| 4 | kit start | Domain + email sender | localhost + MailHog dev defaults | pending |
| 5 | kit start | Legal-review gate: provenance_strip (C2PA) | flag OFF; preserve-only shipped | pending |
| 6 | kit start | SynthID scope beyond research | research memo only | pending |
| 7 | 2026-11-12 | Relation of ~/workspace/cleanupload skeleton to Detag (merge or fresh?) | Fresh repo, no reuse, no silent merge | pending |
| 8 | 2026-11-12 | Admin invite: P0-043 seeds plans + flags only — who owns the runbook's `pf-admin-create` CLI (prints one-time setup link) and is there an invites table/flow? | Safe default: seed invents NO admin user/invite/credential (no table or flow defined in the kit); the CLI needs an owning ticket (Phase 1 admin epic) | owner-input-needed |

## Research log (Phase 6)

(appended by agent; each entry: date, question, sources with URLs + access dates, finding, recommendation)
