# PHASE 5 — Hardening & Release

> **Tickets: 101** · Types: feat 28 · fix 7 · chore 18 · test 27 · docs 21

**Goal:** Ship-ready: security sweep, privacy/retention verification, load + soak, reliability drills, the explicit bug-fixing sprint over ISSUES.md, docs polish, release checklist execution toward v1.0.0-rc1.

**Context (read before starting):**
- The bug-fixing sprint (E5.5) is mandatory - testing + bug fixing is part of the plan, not an afterthought.
- Release stays at rc until owner-input items are answered (live keys, domain, final pricing); do not enable live payments yourself.
- Security suite (05-QUALITY/03-security-testing.md) must be fully green before release checklist runs.

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] Security suite green with zero open S1/S2; authz matrix zero gaps; upload abuse suite all rejects correct.
- [ ] Load + soak: NFR-1..5 met with evidence; 24h soak success >= 97%; no memory leak trend.
- [ ] Backup/restore drill executed with time-to-restore < 30 min recorded.
- [ ] Release checklist (06-DELIVERY/02) fully green except explicitly listed owner-input items.
- [ ] v1.0.0-rc1 tagged, staged, soak report written; defect report from the bug sprint published.

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E5.1 Security hardening | security | 14 |
| E5.2 Privacy & compliance ops | privacy | 9 |
| E5.3 Performance & scale | perf | 12 |
| E5.4 Reliability | reliability | 10 |
| E5.5 Bug-fixing sprint | bugs | 12 |
| E5.6 Release & launch | release | 12 |
| E5.7 Docs & DX final | docs | 8 |
| E5.8 Data & analytics | analytics | 7 |
| E5.9 API versioning & compat | api | 6 |
| E5.10 Load soak | soak | 5 |
| E5.11 Final polish | polish | 6 |

---

## E5.1 — Security hardening (area: security)

- [ ] **P5-001** — ASVS L2 self-audit
  - **Type:** feat · **Area:** security · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Run 05-QUALITY/03 checklist systematically; findings table into ISSUES.md; fix S1/S2 same run; artifact docs/security-audit.md.
  - **AC:**
    - [ ] artifact committed

- [ ] **P5-002** — Authz matrix full sweep
  - **Type:** test · **Area:** security · **Est:** M · **Deps:** P5-001 · **Prio:** P1
  - **Spec:** Regenerate matrix from routes; run; zero gaps; endpoints added since Phase 4 covered.
  - **AC:**
    - [ ] zero gaps

- [ ] **P5-003** — Upload abuse suite final
  - **Type:** test · **Area:** security · **Est:** M · **Deps:** P5-002 · **Prio:** P1
  - **Spec:** All attack fixtures (zip bomb, png bomb, polyglot, svg, exif fuzz corpus) rejected with right codes; guard overhead sane.
  - **AC:**
    - [ ] all rejects correct

- [ ] **P5-004** — SSRF/webhook final
  - **Type:** test · **Area:** security · **Est:** M · **Deps:** P5-003 · **Prio:** P1
  - **Spec:** Private CIDR + redirect + DNS-rebinding guards; constant-time HMAC; replay window enforced.
  - **AC:**
    - [ ] all guards tested

- [ ] **P5-005** — JWT/session final
  - **Type:** test · **Area:** security · **Est:** M · **Deps:** P5-004 · **Prio:** P1
  - **Spec:** alg confusion, kid confusion, reuse detection, fixation (login rotates), cookie flags asserted.
  - **AC:**
    - [ ] all asserted

- [ ] **P5-006** — CSP + headers audit
  - **Type:** feat · **Area:** security · **Est:** S · **Deps:** P5-005 · **Prio:** P1
  - **Spec:** Full header table test on web + api routes; CSP report-only then enforce; violations fixed.
  - **AC:**
    - [ ] enforced CSP green

- [ ] **P5-007** — Dependency audit closure
  - **Type:** chore · **Area:** security · **Est:** M · **Deps:** P5-006 · **Prio:** P1
  - **Spec:** cargo deny + npm audit + gitleaks zero open criticals; pinning policy applied; upgrade notes.
  - **AC:**
    - [ ] zero criticals

- [ ] **P5-008** — Secrets rotation runbook final
  - **Type:** feat · **Area:** security · **Est:** S · **Deps:** P5-007 · **Prio:** P1
  - **Spec:** JWT dual-key window procedure, stripe/smtp/s3 key rotation; rehearsed on staging with evidence.
  - **AC:**
    - [ ] rehearsal recorded

- [ ] **P5-009** — Rate-limit tuning
  - **Type:** test · **Area:** security · **Est:** M · **Deps:** P5-008 · **Prio:** P1
  - **Spec:** Under k6 load: thresholds fair (no user starvation, abuser throttled); configs tuned; evidence table.
  - **AC:**
    - [ ] tuning evidence recorded

- [ ] **P5-010** — Admin pen-test pass
  - **Type:** test · **Area:** security · **Est:** M · **Deps:** P5-009 · **Prio:** P1
  - **Spec:** Attempts: role escalation, flag-lock bypass, audit tamper (db grant), impersonation abuse - all fail correctly.
  - **AC:**
    - [ ] all attempts blocked

- [ ] **P5-011** — Abuse automation detection
  - **Type:** feat · **Area:** security · **Est:** M · **Deps:** P5-010 · **Prio:** P1
  - **Spec:** Velocity rules tuned with realistic load patterns; auto-suspend stays OFF; thresholds reported.
  - **AC:**
    - [ ] thresholds documented

- [ ] **P5-012** — Security changelog
  - **Type:** docs · **Area:** security · **Est:** S · **Deps:** P5-011 · **Prio:** P1
  - **Spec:** security.md: enforced controls summary + audit date; honest scope statement.
  - **AC:**
    - [ ] summary committed

- [ ] **P5-013** — Session fixation + CSRF
  - **Type:** test · **Area:** security · **Est:** M · **Deps:** P5-012 · **Prio:** P1
  - **Spec:** Fixation (pre-auth token invalid post-auth), CSRF posture (same-site + method policy) tests.
  - **AC:**
    - [ ] tests green

- [ ] **P5-014** — Error internals leak check
  - **Type:** feat · **Area:** security · **Est:** S · **Deps:** P5-013 · **Prio:** P1
  - **Spec:** Prod-profile error bodies contain no stack/internal paths (fuzz over error paths); logging redaction final test.
  - **AC:**
    - [ ] leak fuzz clean

---

## E5.2 — Privacy & compliance ops (area: privacy)

- [ ] **P5-015** — GDPR export verify
  - **Type:** test · **Area:** privacy · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Export endpoint produces complete zip (profile, jobs metadata, attestations, usage) within SLA; PII map checked.
  - **AC:**
    - [ ] export complete

- [ ] **P5-016** — Delete cascade verify
  - **Type:** test · **Area:** privacy · **Est:** M · **Deps:** P5-015 · **Prio:** P1
  - **Spec:** Delete user purges artifacts/keys/sessions/attestations per retention; audit keeps minimal record (policy); orphan check.
  - **AC:**
    - [ ] cascade verified

- [ ] **P5-017** — Retention jobs verify
  - **Type:** feat · **Area:** privacy · **Est:** M · **Deps:** P5-016 · **Prio:** P1
  - **Spec:** All NFR-40 schedules run in staging; evidence table (what expired when); plan TTL end-to-end.
  - **AC:**
    - [ ] evidence table complete

- [ ] **P5-018** — Privacy page accuracy
  - **Type:** docs · **Area:** privacy · **Est:** M · **Deps:** P5-017 · **Prio:** P1
  - **Spec:** Privacy text matches actual behavior (retention, sub-processors, essential-only cookies); owner review flagged.
  - **AC:**
    - [ ] accuracy reviewed

- [ ] **P5-019** — Sub-processors list
  - **Type:** chore · **Area:** privacy · **Est:** M · **Deps:** P5-018 · **Prio:** P1
  - **Spec:** Doc + page: payment, email, storage/hosting; update procedure.
  - **AC:**
    - [ ] list published

- [ ] **P5-020** — Audit append-only verify
  - **Type:** test · **Area:** privacy · **Est:** M · **Deps:** P5-019 · **Prio:** P1
  - **Spec:** DB grant test (update/delete denied); optional integrity hash chain decision recorded as ADR.
  - **AC:**
    - [ ] append-only proven

- [ ] **P5-021** — Attestation enforcement e2e
  - **Type:** test · **Area:** privacy · **Est:** M · **Deps:** P5-020 · **Prio:** P1
  - **Spec:** Job without attestation impossible (api + worker double-check); flagged-attestation flow works.
  - **AC:**
    - [ ] double-check verified

- [ ] **P5-022** — AUP workflow test
  - **Type:** test · **Area:** privacy · **Est:** M · **Deps:** P5-021 · **Prio:** P1
  - **Spec:** Violation to warning to suspension ladder end-to-end with emails + audit.
  - **AC:**
    - [ ] ladder e2e green

- [ ] **P5-023** — Compliance checklist artifact
  - **Type:** docs · **Area:** privacy · **Est:** S · **Deps:** P5-022 · **Prio:** P1
  - **Spec:** docs/compliance-checklist.md: status of 05-compliance sections with evidence links.
  - **AC:**
    - [ ] artifact committed

---

## E5.3 — Performance & scale (area: perf)

- [ ] **P5-024** — k6 suite final
  - **Type:** test · **Area:** perf · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Scenarios: auth burst, job create steady, status poll storm, SSE fanout; thresholds from NFR-1..5; results table.
  - **AC:**
    - [ ] thresholds met

- [ ] **P5-025** — Image p95 verify
  - **Type:** feat · **Area:** perf · **Est:** M · **Deps:** P5-024 · **Prio:** P1
  - **Spec:** 4-core constraint: p95 <= 30s (NFR-2); profile + fix or ADR with evidence.
  - **AC:**
    - [ ] evidence recorded

- [ ] **P5-026** — Video realtime verify
  - **Type:** feat · **Area:** perf · **Est:** M · **Deps:** P5-025 · **Prio:** P1
  - **Spec:** >= 0.5x realtime (NFR-3); detection budget tuning if needed; evidence.
  - **AC:**
    - [ ] evidence recorded

- [ ] **P5-027** — DB slow-query closure
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P5-026 · **Prio:** P1
  - **Spec:** All queries < 50ms at load; EXPLAIN evidence; additive index additions.
  - **AC:**
    - [ ] evidence recorded

- [ ] **P5-028** — Redis latency check
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P5-027 · **Prio:** P1
  - **Spec:** p99 < 5ms ops; slowlog clean; memory fit documented.
  - **AC:**
    - [ ] latency verified

- [ ] **P5-029** — SSE fanout test
  - **Type:** test · **Area:** perf · **Est:** M · **Deps:** P5-028 · **Prio:** P1
  - **Spec:** 10k synthetic conns: delivery p95 <= 500ms (NFR-5), no leak (conn count stable).
  - **AC:**
    - [ ] no leak + latency met

- [ ] **P5-030** — GPU vs CPU bench report
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P5-029 · **Prio:** P1
  - **Spec:** Same fixtures both paths (if GPU available) into capacity doc; flag default stance confirmed.
  - **AC:**
    - [ ] report committed

- [ ] **P5-031** — Memory ceiling verify
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P5-030 · **Prio:** P1
  - **Spec:** RSS caps per service under soak; OOM-kill impossible (budget guards tested); evidence.
  - **AC:**
    - [ ] evidence recorded

- [ ] **P5-032** — Backpressure verification
  - **Type:** test · **Area:** perf · **Est:** M · **Deps:** P5-031 · **Prio:** P1
  - **Spec:** Flood beyond capacity: 429s correct, no queue runaway, clean recovery.
  - **AC:**
    - [ ] recovery clean

- [ ] **P5-033** — Cost-per-job final
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P5-032 · **Prio:** P1
  - **Spec:** Real numbers from soak; model + pricing sanity note (owner-visible).
  - **AC:**
    - [ ] model updated

- [ ] **P5-034** — Capacity plan final
  - **Type:** docs · **Area:** perf · **Est:** M · **Deps:** P5-033 · **Prio:** P1
  - **Spec:** Scale decision table finalized with evidence links.
  - **AC:**
    - [ ] table finalized

- [ ] **P5-035** — Perf regression CI gate
  - **Type:** chore · **Area:** perf · **Est:** S · **Deps:** P5-034 · **Prio:** P1
  - **Spec:** Bench thresholds gate in CI (fail > 10% regression); baseline stored.
  - **AC:**
    - [ ] gate active

---

## E5.4 — Reliability (area: reliability)

- [ ] **P5-036** — Backup/restore drill
  - **Type:** test · **Area:** reliability · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Full drill per runbook; time-to-restore recorded (< 30 min target); gaps fixed.
  - **AC:**
    - [ ] drill evidence recorded

- [ ] **P5-037** — Zero-downtime rehearsal
  - **Type:** test · **Area:** reliability · **Est:** M · **Deps:** P5-036 · **Prio:** P1
  - **Spec:** Expand-contract example migration rehearsed on staging clone; app versions cross-compatible; doc updated.
  - **AC:**
    - [ ] rehearsal green

- [ ] **P5-038** — Graceful drain drill
  - **Type:** test · **Area:** reliability · **Est:** M · **Deps:** P5-037 · **Prio:** P1
  - **Spec:** Deploy during active jobs: no lost jobs, drain within timeout; evidence.
  - **AC:**
    - [ ] no lost jobs

- [ ] **P5-039** — Redis outage drill final
  - **Type:** test · **Area:** reliability · **Est:** M · **Deps:** P5-038 · **Prio:** P1
  - **Spec:** Degraded mode correct end-to-end; alerts; recovery; runbook updated.
  - **AC:**
    - [ ] drill green

- [ ] **P5-040** — Storage outage drill final
  - **Type:** test · **Area:** reliability · **Est:** M · **Deps:** P5-039 · **Prio:** P1
  - **Spec:** Holds + resume; no artifact loss.
  - **AC:**
    - [ ] drill green

- [ ] **P5-041** — Kill -9 recovery final
  - **Type:** test · **Area:** reliability · **Est:** M · **Deps:** P5-040 · **Prio:** P1
  - **Spec:** Requeue + complete; duplicate-guard verified.
  - **AC:**
    - [ ] drill green

- [ ] **P5-042** — Webhook replay safety
  - **Type:** test · **Area:** reliability · **Est:** M · **Deps:** P5-041 · **Prio:** P1
  - **Spec:** Replay storm simulation: idempotent consumers unaffected; delivery logs sane.
  - **AC:**
    - [ ] storm safe

- [ ] **P5-043** — Clock skew handling
  - **Type:** feat · **Area:** reliability · **Est:** S · **Deps:** P5-042 · **Prio:** P1
  - **Spec:** Signed URL/TTL/token behavior under +/- 5 min skew documented + tests (monotonic where possible).
  - **AC:**
    - [ ] skew tests green

- [ ] **P5-044** — Incident runbook
  - **Type:** docs · **Area:** reliability · **Est:** M · **Deps:** P5-043 · **Prio:** P1
  - **Spec:** Severity ladder, comms template, roles (owner+agent), postmortem template.
  - **AC:**
    - [ ] runbook committed

- [ ] **P5-045** — Error budget policy
  - **Type:** docs · **Area:** reliability · **Est:** S · **Deps:** P5-044 · **Prio:** P1
  - **Spec:** SLO targets (NFR-20) + budget math; when to freeze features.
  - **AC:**
    - [ ] policy committed

---

## E5.5 — Bug-fixing sprint (area: bugs)

- [ ] **P5-046** — Triage backlog
  - **Type:** chore · **Area:** bugs · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Full ISSUES.md sweep: severities current, repros fresh, dups merged; burn-down plan for the sprint.
  - **AC:**
    - [ ] backlog clean

- [ ] **P5-047** — Fix S1/S2 batch
  - **Type:** fix · **Area:** bugs · **Est:** M · **Deps:** P5-046 · **Prio:** P1
  - **Spec:** All open S1/S2 fixed with regression tests (repeat until zero; multiple commits).
  - **AC:**
    - [ ] zero open S1/S2

- [ ] **P5-048** — Fix S3 batch 1
  - **Type:** fix · **Area:** bugs · **Est:** M · **Deps:** P5-047 · **Prio:** P1
  - **Spec:** Engine/pipeline S3s (quality-adjacent first).
  - **AC:**
    - [ ] batch closed

- [ ] **P5-049** — Fix S3 batch 2
  - **Type:** fix · **Area:** bugs · **Est:** M · **Deps:** P5-048 · **Prio:** P1
  - **Spec:** API/web S3s (UX-adjacent).
  - **AC:**
    - [ ] batch closed

- [ ] **P5-050** — Fix S4 polish batch
  - **Type:** fix · **Area:** bugs · **Est:** S · **Deps:** P5-049 · **Prio:** P1
  - **Spec:** Copy/log/noise items.
  - **AC:**
    - [ ] batch closed

- [ ] **P5-051** — Regression suite green
  - **Type:** test · **Area:** bugs · **Est:** M · **Deps:** P5-050 · **Prio:** P1
  - **Spec:** Full pyramid green after fixes; golden suite; flaky re-run.
  - **AC:**
    - [ ] all green

- [ ] **P5-052** — Flaky elimination
  - **Type:** fix · **Area:** bugs · **Est:** M · **Deps:** P5-051 · **Prio:** P1
  - **Spec:** All quarantined tests fixed or deleted (no stragglers).
  - **AC:**
    - [ ] zero quarantined

- [ ] **P5-053** — Error-message UX sweep
  - **Type:** fix · **Area:** bugs · **Est:** S · **Deps:** P5-052 · **Prio:** P1
  - **Spec:** Every user-visible PF-code has honest, actionable copy (map reviewed end-to-end).
  - **AC:**
    - [ ] map complete

- [ ] **P5-054** — Log noise reduction
  - **Type:** chore · **Area:** bugs · **Est:** S · **Deps:** P5-053 · **Prio:** P1
  - **Spec:** Sampled success logs, keep error fidelity; noise audit vs dashboards.
  - **AC:**
    - [ ] dashboards readable

- [ ] **P5-055** — Race-condition audit
  - **Type:** fix · **Area:** bugs · **Est:** M · **Deps:** P5-054 · **Prio:** P1
  - **Spec:** tokio::select!/shared-state spots reviewed (list in commit); tests where risky.
  - **AC:**
    - [ ] audit list + tests

- [ ] **P5-056** — Clippy pedantic pass
  - **Type:** chore · **Area:** bugs · **Est:** S · **Deps:** P5-055 · **Prio:** P1
  - **Spec:** Selected pedantic lints adopted where they catch real issues; diff reviewed.
  - **AC:**
    - [ ] pedantic clean

- [ ] **P5-057** — Defect report
  - **Type:** docs · **Area:** bugs · **Est:** S · **Deps:** P5-056 · **Prio:** P1
  - **Spec:** Summary: counts by sev/component, root-cause themes, prevention actions into kit (retro).
  - **AC:**
    - [ ] report committed

---

## E5.6 — Release & launch (area: release)

- [ ] **P5-058** — v1.0.0-rc1 tag
  - **Type:** chore · **Area:** release · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Version bump, CHANGELOG from commits, release CI (images tagged, checksums); rc deployed to staging.
  - **AC:**
    - [ ] rc deployed

- [ ] **P5-059** — Staging smoke + soak
  - **Type:** test · **Area:** release · **Est:** M · **Deps:** P5-058 · **Prio:** P1
  - **Spec:** 24h soak on staging: error budget respected, zero S1/S2; report.
  - **AC:**
    - [ ] soak report green

- [ ] **P5-060** — Migration runbook final
  - **Type:** chore · **Area:** release · **Est:** M · **Deps:** P5-059 · **Prio:** P1
  - **Spec:** Prod upgrade steps validated on prod-shaped copy; rollback rehearsed.
  - **AC:**
    - [ ] validated

- [ ] **P5-061** — Pricing finalization
  - **Type:** chore · **Area:** release · **Est:** M · **Deps:** P5-060 · **Prio:** P1
  - **Spec:** Owner-answered values applied (config + stripe test-to-live checklist doc); if unanswered: rc holds (owner-input note).
  - **AC:**
    - [ ] owner answers recorded

- [ ] **P5-062** — Stripe live checklist doc
  - **Type:** chore · **Area:** release · **Est:** S · **Deps:** P5-061 · **Prio:** P1
  - **Spec:** Webhook live endpoint, live key rotation, tax settings, dashboard settings - owner checklist finalized.
  - **AC:**
    - [ ] checklist final

- [ ] **P5-063** — ToS/AUP/Privacy final
  - **Type:** docs · **Area:** release · **Est:** M · **Deps:** P5-062 · **Prio:** P1
  - **Spec:** Owner-reviewed text (or flagged pending), versioned, deployed; honesty-rule lint green.
  - **AC:**
    - [ ] pages deployed

- [ ] **P5-064** — Launch checklist run
  - **Type:** chore · **Area:** release · **Est:** M · **Deps:** P5-063 · **Prio:** P1
  - **Spec:** 06-DELIVERY/02 executed top-to-bottom; evidence links filled; owner-input items listed explicitly.
  - **AC:**
    - [ ] checklist green

- [ ] **P5-065** — Beta users plan
  - **Type:** feat · **Area:** release · **Est:** S · **Deps:** P5-064 · **Prio:** P1
  - **Spec:** Invite flow (manual codes feature flag), beta feedback intake (form), cohort metrics note.
  - **AC:**
    - [ ] invite flow works

- [ ] **P5-066** — Support macros final
  - **Type:** docs · **Area:** release · **Est:** M · **Deps:** P5-065 · **Prio:** P1
  - **Spec:** Support playbooks finalized (refund, quality_failed, AUP, TTL questions).
  - **AC:**
    - [ ] playbooks final

- [ ] **P5-067** — Launch retro template
  - **Type:** docs · **Area:** release · **Est:** S · **Deps:** P5-066 · **Prio:** P1
  - **Spec:** Post-launch retro doc scaffold (metrics, incidents, learnings).
  - **AC:**
    - [ ] template committed

- [ ] **P5-068** — Docs site final
  - **Type:** chore · **Area:** release · **Est:** S · **Deps:** P5-067 · **Prio:** P1
  - **Spec:** All pages render, search stub ok, links green (CI link check).
  - **AC:**
    - [ ] links green

- [ ] **P5-069** — Release notes page
  - **Type:** chore · **Area:** release · **Est:** S · **Deps:** P5-068 · **Prio:** P1
  - **Spec:** Public notes from CHANGELOG (honest tone) + status page incident history stub filled.
  - **AC:**
    - [ ] notes live

---

## E5.7 — Docs & DX final (area: docs)

- [ ] **P5-070** — README prod quickstart
  - **Type:** docs · **Area:** docs · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Repo README: 3-command deploy, env table pointer, real screenshots replace placeholders.
  - **AC:**
    - [ ] quickstart accurate

- [ ] **P5-071** — Architecture diagrams refresh
  - **Type:** docs · **Area:** docs · **Est:** M · **Deps:** P5-070 · **Prio:** P1
  - **Spec:** Diagrams updated to final reality (components, data flow, queue lanes); mermaid sources in docs.
  - **AC:**
    - [ ] diagrams accurate

- [ ] **P5-072** — Engine tuning guide
  - **Type:** docs · **Area:** docs · **Est:** M · **Deps:** P5-071 · **Prio:** P1
  - **Spec:** Knobs (tiles, thresholds, budgets), calibration, examples; honest limits (fast motion, huge masks).
  - **AC:**
    - [ ] guide final

- [ ] **P5-073** — Runbook index
  - **Type:** docs · **Area:** docs · **Est:** S · **Deps:** P5-072 · **Prio:** P1
  - **Spec:** ops/runbooks: index linking drills, incidents, rotation, deploy; printable.
  - **AC:**
    - [ ] index complete

- [ ] **P5-074** — Ops FAQ
  - **Type:** docs · **Area:** docs · **Est:** S · **Deps:** P5-073 · **Prio:** P1
  - **Spec:** Top 10 ops questions (disk full, queue stuck, key rotation...).
  - **AC:**
    - [ ] FAQ committed

- [ ] **P5-075** — Glossary
  - **Type:** docs · **Area:** docs · **Est:** S · **Deps:** P5-074 · **Prio:** P1
  - **Spec:** Domain terms (attestation, lane, envelope, gate) single definitions.
  - **AC:**
    - [ ] glossary committed

- [ ] **P5-076** — Onboarding doc
  - **Type:** docs · **Area:** docs · **Est:** M · **Deps:** P5-075 · **Prio:** P1
  - **Spec:** New contributor (human or agent) 1-day path: setup, standards, first-ticket walkthrough.
  - **AC:**
    - [ ] path verified by fresh-eye pass

- [ ] **P5-077** — Docs lint
  - **Type:** chore · **Area:** docs · **Est:** S · **Deps:** P5-076 · **Prio:** P1
  - **Spec:** Link check + markdown lint CI; broken links impossible to merge.
  - **AC:**
    - [ ] lint gate green

---

## E5.8 — Data & analytics (area: analytics)

- [ ] **P5-078** — Event taxonomy
  - **Type:** feat · **Area:** analytics · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Product events (no PII): signup, verify, upload, job_done, job_failed, quality_failed, upgrade_click, api_key_created; schema doc.
  - **AC:**
    - [ ] schema committed

- [ ] **P5-079** — Retention policy
  - **Type:** feat · **Area:** analytics · **Est:** S · **Deps:** P5-078 · **Prio:** P1
  - **Spec:** Analytics retention + opt-out stance documented; no third-party trackers (self-host only).
  - **AC:**
    - [ ] policy committed

- [ ] **P5-080** — Weekly report script
  - **Type:** feat · **Area:** analytics · **Est:** S · **Deps:** P5-079 · **Prio:** P1
  - **Spec:** Script: jobs, success, quality pass, conversion events to markdown digest (for owner).
  - **AC:**
    - [ ] digest generated

- [ ] **P5-081** — KPI dashboard
  - **Type:** feat · **Area:** analytics · **Est:** M · **Deps:** P5-080 · **Prio:** P1
  - **Spec:** Grafana/product dashboard: activation, job success, quality pass, MRR; queries documented.
  - **AC:**
    - [ ] dashboard live

- [ ] **P5-082** — Funnel events wiring
  - **Type:** feat · **Area:** analytics · **Est:** S · **Deps:** P5-081 · **Prio:** P1
  - **Spec:** Instrument web flows per taxonomy; e2e event assertions (event fired once).
  - **AC:**
    - [ ] assertions green

- [ ] **P5-083** — Analytics tests
  - **Type:** test · **Area:** analytics · **Est:** S · **Deps:** P5-082 · **Prio:** P1
  - **Spec:** Event emission unit/integration (no PII in payload), opt-out honored.
  - **AC:**
    - [ ] tests green

- [ ] **P5-084** — Analytics doc
  - **Type:** docs · **Area:** analytics · **Est:** S · **Deps:** P5-083 · **Prio:** P1
  - **Spec:** Taxonomy, dashboards, privacy stance, how to add an event.
  - **AC:**
    - [ ] doc committed

---

## E5.9 — API versioning & compat (area: api)

- [ ] **P5-085** — /v1 freeze policy
  - **Type:** docs · **Area:** api · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Versioning policy: additive-only within v1, deprecation headers process, v2 trigger criteria; ADR.
  - **AC:**
    - [ ] ADR committed

- [ ] **P5-086** — Deprecation headers
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P5-085 · **Prio:** P1
  - **Spec:** Deprecation/Sunset header plumbing + registry; docs.
  - **AC:**
    - [ ] headers emitted

- [ ] **P5-087** — Compat suite
  - **Type:** test · **Area:** api · **Est:** M · **Deps:** P5-086 · **Prio:** P1
  - **Spec:** Golden client (frozen curl/TS/py scripts) against current API: contract holds.
  - **AC:**
    - [ ] contract holds

- [ ] **P5-088** — OpenAPI diff CI
  - **Type:** chore · **Area:** api · **Est:** S · **Deps:** P5-087 · **Prio:** P1
  - **Spec:** Breaking-change detector (openapi-diff) gates merge; changelog comment auto-generated.
  - **AC:**
    - [ ] gate active

- [ ] **P5-089** — Changelog automation
  - **Type:** chore · **Area:** api · **Est:** S · **Deps:** P5-088 · **Prio:** P1
  - **Spec:** API changelog generated from spec diffs + conventional commits.
  - **AC:**
    - [ ] automation live

- [ ] **P5-090** — Compat doc
  - **Type:** docs · **Area:** api · **Est:** S · **Deps:** P5-089 · **Prio:** P1
  - **Spec:** Consumer migration notes template; example of a past change.
  - **AC:**
    - [ ] doc committed

---

## E5.10 — Load soak (area: soak)

- [ ] **P5-091** — 24h soak script
  - **Type:** test · **Area:** soak · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Sustained mixed load (image + small video) with chaos interruptions; success >= 97% (NFR-20).
  - **AC:**
    - [ ] success target met

- [ ] **P5-092** — Soak report
  - **Type:** feat · **Area:** soak · **Est:** S · **Deps:** P5-091 · **Prio:** P1
  - **Spec:** Metrics: success, latency drift, memory trend (leak check RSS slope), disk growth; issues filed.
  - **AC:**
    - [ ] report committed

- [ ] **P5-093** — Leak check
  - **Type:** chore · **Area:** soak · **Est:** S · **Deps:** P5-092 · **Prio:** P1
  - **Spec:** RSS/heap trend analysis across soak; suspicious growth becomes profiling ticket.
  - **AC:**
    - [ ] no unexplained growth

- [ ] **P5-094** — DB bloat check
  - **Type:** chore · **Area:** soak · **Est:** S · **Deps:** P5-093 · **Prio:** P1
  - **Spec:** Table/index bloat after soak (pgstattuple); autovacuum tuning note.
  - **AC:**
    - [ ] bloat sane

- [ ] **P5-095** — Soak findings
  - **Type:** docs · **Area:** soak · **Est:** S · **Deps:** P5-094 · **Prio:** P1
  - **Spec:** Findings + capacity plan updates; runbook notes.
  - **AC:**
    - [ ] findings committed

---

## E5.11 — Final polish (area: polish)

- [ ] **P5-096** — UI copy pass
  - **Type:** feat · **Area:** polish · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** All strings reviewed: honest, helpful, tone-consistent; honesty-rule lint green.
  - **AC:**
    - [ ] lint green

- [ ] **P5-097** — Brand assets
  - **Type:** feat · **Area:** polish · **Est:** S · **Deps:** P5-096 · **Prio:** P1
  - **Spec:** Favicon set, neutral logo, OG images final; brand name = owner-input placeholder respected.
  - **AC:**
    - [ ] assets complete

- [ ] **P5-098** — 404/500 pages
  - **Type:** feat · **Area:** polish · **Est:** S · **Deps:** P5-097 · **Prio:** P1
  - **Spec:** Branded error pages with recovery paths; tested.
  - **AC:**
    - [ ] pages tested

- [ ] **P5-099** — robots/sitemap final
  - **Type:** feat · **Area:** polish · **Est:** S · **Deps:** P5-098 · **Prio:** P1
  - **Spec:** Index policy (docs indexed, app noindex), sitemap complete.
  - **AC:**
    - [ ] policy verified

- [ ] **P5-100** — OG images final
  - **Type:** feat · **Area:** polish · **Est:** S · **Deps:** P5-099 · **Prio:** P1
  - **Spec:** Per-page OG tags; preview check.
  - **AC:**
    - [ ] previews verified

- [ ] **P5-101** — Release notes page
  - **Type:** feat · **Area:** polish · **Est:** S · **Deps:** P5-100 · **Prio:** P1
  - **Spec:** Public-facing notes rendered (feeds from CHANGELOG).
  - **AC:**
    - [ ] page live
