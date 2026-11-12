# PHASE 4 — Console & Admin

> **Tickets: 102** · Types: feat 83 · chore 2 · test 11 · docs 6

**Goal:** User-facing upload-centric web console (auth, staged uploader, before/after compare, quality report card, billing UI), full admin panel (dashboard, users, jobs, flags, plans, audit, abuse, attestations, health), public site (landing, docs, status, AUP). Backend owns everything; frontend stays thin.

**Context (read before starting):**
- Upload-centric console only - no feature creep into heavy client logic; the backend owns all processing.
- All copy passes the marketing-honesty rule (compliance doc section 6): no invisible-mark removal claims anywhere.
- Admin actions must all write audit entries (FR-W-05) - the API enforces it, the UI must not hide it.

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] E2e: register, upload, staged progress, result with report card, download - all green on Playwright.
- [ ] Admin e2e smoke: every panel page renders with seeded data; one audited action per page verified.
- [ ] Authz: support read-only and user-to-admin isolation enforced in UI + API (matrix green).
- [ ] Lighthouse >= 90 on landing + app shell; bundle budget gate green.
- [ ] Public pages (landing, pricing, docs, status, AUP/ToS/Privacy) live with honest copy.

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E4.1 Web scaffold | web/base | 10 |
| E4.2 Auth UI | web/auth | 7 |
| E4.3 Uploader UX | web/uploader | 12 |
| E4.4 Results UX | web/results | 9 |
| E4.5 Billing UI | web/billing | 7 |
| E4.6 Admin panel | web/admin | 16 |
| E4.7 Landing & docs | web/public | 9 |
| E4.8 Web platform polish | web/platform | 7 |
| E4.9 API DX | dx | 8 |
| E4.10 Notifications UX | web/notify | 6 |
| E4.11 Web performance | web/perf | 6 |
| E4.12 Support view | web/support | 5 |

---

## E4.1 — Web scaffold (area: web/base)

- [ ] **P4-001** — Next.js scaffold
  - **Type:** feat · **Area:** web/base · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** web/: Next 15 App Router, TS strict, Tailwind, shadcn/ui subset install; layout shell (header/footer); dark mode toggle; env config (NEXT_PUBLIC_API_URL).
  - **AC:**
    - [ ] build green
    - [ ] dark mode toggles

- [ ] **P4-002** — Design tokens
  - **Type:** feat · **Area:** web/base · **Est:** S · **Deps:** P4-001 · **Prio:** P1
  - **Spec:** Color/type/spacing tokens (brand-neutral), shadcn theme mapping, spacing rhythm; tokens.ts single source.
  - **AC:**
    - [ ] tokens documented

- [ ] **P4-003** — API client generation
  - **Type:** feat · **Area:** web/base · **Est:** M · **Deps:** P4-002 · **Prio:** P1
  - **Spec:** OpenAPI to typed client (openapi-typescript + fetch wrapper); auth token injection; problem+json error mapping types.
  - **AC:**
    - [ ] generated client compiles
    - [ ] error mapping tested

- [ ] **P4-004** — Session middleware
  - **Type:** feat · **Area:** web/base · **Est:** M · **Deps:** P4-003 · **Prio:** P1
  - **Spec:** Cookie-based refresh flow, in-memory access token, auto-refresh on 401 once, redirect to /login; auth route group layout.
  - **AC:**
    - [ ] refresh loop guarded

- [ ] **P4-005** — Error/loading states
  - **Type:** feat · **Area:** web/base · **Est:** S · **Deps:** P4-004 · **Prio:** P1
  - **Spec:** Consistent error toasts (PF-code to copy map), skeleton loaders, empty-state component set.
  - **AC:**
    - [ ] PF-code map covered

- [ ] **P4-006** — Responsive shell
  - **Type:** feat · **Area:** web/base · **Est:** S · **Deps:** P4-005 · **Prio:** P1
  - **Spec:** Mobile-first nav (hamburger), uploader-first home, admin responsive tables (scroll-safe); breakpoints documented.
  - **AC:**
    - [ ] mobile layout verified

- [ ] **P4-007** — a11y baseline
  - **Type:** feat · **Area:** web/base · **Est:** S · **Deps:** P4-006 · **Prio:** P1
  - **Spec:** axe CI on key pages, focus management in dialogs, keyboard paths for uploader; ARIA patterns for progress.
  - **AC:**
    - [ ] axe green

- [ ] **P4-008** — SEO base
  - **Type:** feat · **Area:** web/base · **Est:** S · **Deps:** P4-007 · **Prio:** P1
  - **Spec:** Metadata API per page, OG defaults, sitemap.xml, robots.txt; honest default copy.
  - **AC:**
    - [ ] meta tags verified

- [ ] **P4-009** — Analytics hook
  - **Type:** feat · **Area:** web/base · **Est:** S · **Deps:** P4-008 · **Prio:** P1
  - **Spec:** Self-host umami (compose) or noop; event list (signup, upload, job_done, upgrade_click); no PII events (NFR-42).
  - **AC:**
    - [ ] event list documented

- [ ] **P4-010** — Web CI
  - **Type:** test · **Area:** web/base · **Est:** M · **Deps:** P4-009 · **Prio:** P1
  - **Spec:** Build + lint + type-check + axe CI job; preview notes; any error fails.
  - **AC:**
    - [ ] CI green

---

## E4.2 — Auth UI (area: web/auth)

- [ ] **P4-011** — Login page
  - **Type:** feat · **Area:** web/auth · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Form (zod validation), error copy map, rate-limit-aware messaging (429), redirect handling.
  - **AC:**
    - [ ] 429 copy shows

- [ ] **P4-012** — Register + verify notice
  - **Type:** feat · **Area:** web/auth · **Est:** M · **Deps:** P4-011 · **Prio:** P1
  - **Spec:** Register form, ToS/AUP checkbox with links, post-register verify-email notice screen, resend (rate-limited).
  - **AC:**
    - [ ] notice screen green

- [ ] **P4-013** — Forgot/reset pages
  - **Type:** feat · **Area:** web/auth · **Est:** M · **Deps:** P4-012 · **Prio:** P1
  - **Spec:** Request + reset forms, token-expired handling, success path to login with notice.
  - **AC:**
    - [ ] expired-token path green

- [ ] **P4-014** — Session expired UX
  - **Type:** feat · **Area:** web/auth · **Est:** S · **Deps:** P4-013 · **Prio:** P1
  - **Spec:** Interceptor: refresh once then redirect with return-url; no infinite loops; toast copy.
  - **AC:**
    - [ ] no loop verified

- [ ] **P4-015** — Logout
  - **Type:** feat · **Area:** web/auth · **Est:** S · **Deps:** P4-014 · **Prio:** P1
  - **Spec:** Menu logout clears state and goes to /login; logout-all lands in sessions epic.
  - **AC:**
    - [ ] state cleared

- [ ] **P4-016** — Auth e2e
  - **Type:** test · **Area:** web/auth · **Est:** M · **Deps:** P4-015 · **Prio:** P1
  - **Spec:** Playwright: register (mailhog link) to verify to login to me page; wrong-password error; reset flow.
  - **AC:**
    - [ ] e2e green

- [ ] **P4-017** — Auth UX spec
  - **Type:** docs · **Area:** web/auth · **Est:** S · **Deps:** P4-016 · **Prio:** P1
  - **Spec:** States table (idle/loading/error/success), copy deck, edge cases.
  - **AC:**
    - [ ] spec committed

---

## E4.3 — Uploader UX (area: web/uploader)

- [ ] **P4-018** — Dropzone component
  - **Type:** feat · **Area:** web/uploader · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Drag-drop, click, clipboard paste; client checks (type/size by plan); multi-file queue; a11y (focus/keys).
  - **AC:**
    - [ ] paste path works

- [ ] **P4-019** — Upload engine
  - **Type:** feat · **Area:** web/uploader · **Est:** M · **Deps:** P4-018 · **Prio:** P1
  - **Spec:** Presign init to parts (concurrency 3) to complete; per-part retry; aggregate progress; small-PUT path.
  - **AC:**
    - [ ] part retry verified

- [ ] **P4-020** — Resume + integrity
  - **Type:** feat · **Area:** web/uploader · **Est:** M · **Deps:** P4-019 · **Prio:** P1
  - **Spec:** Upload id persisted per page session; resume UI on retry; optional client sha256 (webcrypto); part-failure recovery.
  - **AC:**
    - [ ] resume e2e green

- [ ] **P4-021** — Staged progress UI
  - **Type:** feat · **Area:** web/uploader · **Est:** M · **Deps:** P4-020 · **Prio:** P1
  - **Spec:** Stages: upload, queue, detect, inpaint, encode, QC with API weights; ETA (moving average); SSE consumer.
  - **AC:**
    - [ ] stage chips render live

- [ ] **P4-022** — Cancel/retry buttons
  - **Type:** feat · **Area:** web/uploader · **Est:** S · **Deps:** P4-021 · **Prio:** P1
  - **Spec:** Cancel (queued/running), retry (failed/quality_failed); optimistic state + API truth.
  - **AC:**
    - [ ] optimistic + reconcile tested

- [ ] **P4-023** — Multi-file queue view
  - **Type:** feat · **Area:** web/uploader · **Est:** M · **Deps:** P4-022 · **Prio:** P1
  - **Spec:** Queue list with per-file status chips + aggregate progress; disabled buttons carry reasons.
  - **AC:**
    - [ ] reasons visible

- [ ] **P4-024** — Mode picker
  - **Type:** feat · **Area:** web/uploader · **Est:** S · **Deps:** P4-023 · **Prio:** P1
  - **Spec:** auto/manual/detect_only/metadata_only selector with honest one-line explanations; hint editor entry (next epic).
  - **AC:**
    - [ ] explanations honest

- [ ] **P4-025** — Keyboard + paste flow
  - **Type:** feat · **Area:** web/uploader · **Est:** S · **Deps:** P4-024 · **Prio:** P1
  - **Spec:** Ctrl+V image uploads; keyboard-only path completes a job (e2e).
  - **AC:**
    - [ ] keyboard e2e green

- [ ] **P4-026** — Mobile uploader
  - **Type:** feat · **Area:** web/uploader · **Est:** S · **Deps:** P4-025 · **Prio:** P1
  - **Spec:** Touch-friendly, camera-roll pick, reduced part concurrency on cellular hint; layout test.
  - **AC:**
    - [ ] mobile layout green

- [ ] **P4-027** — Uploader unit tests
  - **Type:** test · **Area:** web/uploader · **Est:** M · **Deps:** P4-026 · **Prio:** P1
  - **Spec:** Upload-engine state machine (vitest), retry logic, progress math.
  - **AC:**
    - [ ] state machine covered

- [ ] **P4-028** — Uploader e2e
  - **Type:** test · **Area:** web/uploader · **Est:** M · **Deps:** P4-027 · **Prio:** P1
  - **Spec:** Full flow: drop image to stages to result; small video fixture flow; cancel mid-way.
  - **AC:**
    - [ ] e2e green

- [ ] **P4-029** — Uploader spec
  - **Type:** docs · **Area:** web/uploader · **Est:** S · **Deps:** P4-028 · **Prio:** P1
  - **Spec:** States, error copy map, network resilience notes, screenshot inventory.
  - **AC:**
    - [ ] spec committed

---

## E4.4 — Results UX (area: web/results)

- [ ] **P4-030** — Before/after slider
  - **Type:** feat · **Area:** web/results · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Compare slider component (webp previews), zoom-pan compare view; keyboard accessible.
  - **AC:**
    - [ ] keyboard path works

- [ ] **P4-031** — Quality report card
  - **Type:** feat · **Area:** web/results · **Est:** M · **Deps:** P4-030 · **Prio:** P1
  - **Spec:** Metric chips (SSIM/PSNR/VMAF pass-fail), engine/model versions, retry note; link to full JSON.
  - **AC:**
    - [ ] chips reflect API values

- [ ] **P4-032** — Download flow
  - **Type:** feat · **Area:** web/results · **Est:** M · **Deps:** P4-031 · **Prio:** P1
  - **Spec:** Signed URL download; expiry notice with refresh; filename preserved.
  - **AC:**
    - [ ] expiry notice correct

- [ ] **P4-033** — Job history page
  - **Type:** feat · **Area:** web/results · **Est:** M · **Deps:** P4-032 · **Prio:** P1
  - **Spec:** List with status/stage/date, filters, pagination; row opens detail.
  - **AC:**
    - [ ] pagination tested

- [ ] **P4-034** — Job detail page
  - **Type:** feat · **Area:** web/results · **Est:** M · **Deps:** P4-033 · **Prio:** P1
  - **Spec:** Params, regions summary (mask thumb), report, artifacts, events timeline (job_events).
  - **AC:**
    - [ ] timeline renders

- [ ] **P4-035** — Error states map
  - **Type:** feat · **Area:** web/results · **Est:** S · **Deps:** P4-034 · **Prio:** P1
  - **Spec:** PF-code to human copy (PF-4002 explains honestly with report link); no dead-ends.
  - **AC:**
    - [ ] no dead-end states

- [ ] **P4-036** — Privacy notes
  - **Type:** feat · **Area:** web/results · **Est:** S · **Deps:** P4-035 · **Prio:** P1
  - **Spec:** No public sharing by default; TTL countdown chip; self-serve export/delete links.
  - **AC:**
    - [ ] TTL chip accurate

- [ ] **P4-037** — Results e2e
  - **Type:** test · **Area:** web/results · **Est:** M · **Deps:** P4-036 · **Prio:** P1
  - **Spec:** Slider renders, report chips match API, download starts (blob check).
  - **AC:**
    - [ ] e2e green

- [ ] **P4-038** — Results spec
  - **Type:** docs · **Area:** web/results · **Est:** S · **Deps:** P4-037 · **Prio:** P1
  - **Spec:** Component states, chip thresholds, edge cases (expired result).
  - **AC:**
    - [ ] spec committed

---

## E4.5 — Billing UI (area: web/billing)

- [ ] **P4-039** — Pricing page
  - **Type:** feat · **Area:** web/billing · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Plan matrix from /v1/plans (feature ticks, quotas), INR/USD toggle (config), honest copy per compliance; FAQ link.
  - **AC:**
    - [ ] matrix from API

- [ ] **P4-040** — Checkout redirect
  - **Type:** feat · **Area:** web/billing · **Est:** M · **Deps:** P4-039 · **Prio:** P1
  - **Spec:** Plan select to POST checkout to redirect; cancel-back handling; signed-in gate.
  - **AC:**
    - [ ] cancel-back handled

- [ ] **P4-041** — Usage meters
  - **Type:** feat · **Area:** web/billing · **Est:** M · **Deps:** P4-040 · **Prio:** P1
  - **Spec:** Quota bars (images/video-seconds) from /v1/usage + quota headers; overage banners with upgrade CTA.
  - **AC:**
    - [ ] bars match API

- [ ] **P4-042** — Billing page
  - **Type:** feat · **Area:** web/billing · **Est:** M · **Deps:** P4-041 · **Prio:** P1
  - **Spec:** Subscription state, portal button, invoices list, cancel flow note (portal-driven).
  - **AC:**
    - [ ] state matches API

- [ ] **P4-043** — Plan change confirm
  - **Type:** feat · **Area:** web/billing · **Est:** S · **Deps:** P4-042 · **Prio:** P1
  - **Spec:** Upgrade/downgrade modal with proration explanation (API copy).
  - **AC:**
    - [ ] proration copy shown

- [ ] **P4-044** — Billing e2e
  - **Type:** test · **Area:** web/billing · **Est:** M · **Deps:** P4-043 · **Prio:** P1
  - **Spec:** Free flow (stripe test keys optional): meters render, checkout redirects to stripe test page; gates for CI without keys.
  - **AC:**
    - [ ] CI-safe gating works

- [ ] **P4-045** — Billing UX spec
  - **Type:** docs · **Area:** web/billing · **Est:** S · **Deps:** P4-044 · **Prio:** P1
  - **Spec:** States, copy, edge cases (past_due banner).
  - **AC:**
    - [ ] spec committed

---

## E4.6 — Admin panel (area: web/admin)

- [ ] **P4-046** — Admin layout + guard
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** /admin routes, admin role guard (API-backed), nav; impersonation banner component.
  - **AC:**
    - [ ] guard + banner verified

- [ ] **P4-047** — Dashboard page
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-046 · **Prio:** P1
  - **Spec:** Cards: jobs today, success rate, p95, queue depth, MRR, DAU; charts from /v1/admin/metrics/dashboard.
  - **AC:**
    - [ ] cards match API

- [ ] **P4-048** — Users table
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-047 · **Prio:** P1
  - **Spec:** Search/filter/paginate; row actions: suspend/restore, quota grant, impersonate (reason modal), notes view.
  - **AC:**
    - [ ] actions audited (API side verified)

- [ ] **P4-049** — User detail
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-048 · **Prio:** P1
  - **Spec:** Profile, plan, usage 30d, recent jobs, sessions, api keys (prefix), audit trail link.
  - **AC:**
    - [ ] all sections render

- [ ] **P4-050** — Jobs browser
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-049 · **Prio:** P1
  - **Spec:** Filter by status/kind/date; inspect JSON (params/report); actions: cancel, reprocess; DLQ view link.
  - **AC:**
    - [ ] inspect + actions work

- [ ] **P4-051** — Flags editor
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-050 · **Prio:** P1
  - **Spec:** Flag list + toggle + rollout JSON editor; changes audited; danger flags (provenance_strip) visually locked with gate explainer.
  - **AC:**
    - [ ] danger flag locked

- [ ] **P4-052** — Plans editor
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-051 · **Prio:** P1
  - **Spec:** Entitlement form (numbers, features) + stripe price mapping; validation; audit.
  - **AC:**
    - [ ] validation tested

- [ ] **P4-053** — Audit viewer
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-052 · **Prio:** P1
  - **Spec:** Query by actor/action/target/date; expandable before/after JSON; CSV export.
  - **AC:**
    - [ ] export parses

- [ ] **P4-054** — Abuse queue
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-053 · **Prio:** P1
  - **Spec:** Open reports, evidence links (job/artifact preview within TTL), actions (reject job, suspend, dismiss) with reasons; SLA timers.
  - **AC:**
    - [ ] SLA timers visible

- [ ] **P4-055** — Attestation review
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-054 · **Prio:** P1
  - **Spec:** Flagged attestations list (velocity anomalies), user/job links; mark reviewed (audited).
  - **AC:**
    - [ ] review flow audited

- [ ] **P4-056** — Webhook deliveries viewer
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-055 · **Prio:** P1
  - **Spec:** Per endpoint: deliveries, status codes, replay button; error bodies.
  - **AC:**
    - [ ] replay works

- [ ] **P4-057** — System health page
  - **Type:** feat · **Area:** web/admin · **Est:** M · **Deps:** P4-056 · **Prio:** P1
  - **Spec:** readyz components, storage usage, backup freshness, model registry state, queue stats; auto-refresh.
  - **AC:**
    - [ ] auto-refresh works

- [ ] **P4-058** — Kill switches
  - **Type:** feat · **Area:** web/admin · **Est:** S · **Deps:** P4-057 · **Prio:** P1
  - **Spec:** Flag-driven: pause job intake (maintenance), disable signups; big visible states; audited.
  - **AC:**
    - [ ] switches audited

- [ ] **P4-059** — Admin e2e smoke
  - **Type:** test · **Area:** web/admin · **Est:** M · **Deps:** P4-058 · **Prio:** P1
  - **Spec:** Login as admin: each page renders with seeded data; one action per page (suspend+restore) verified.
  - **AC:**
    - [ ] smoke green

- [ ] **P4-060** — Web role tests
  - **Type:** test · **Area:** web/admin · **Est:** M · **Deps:** P4-059 · **Prio:** P1
  - **Spec:** support role read-only enforced in UI + API (403 toasts); user cannot reach /admin (redirect).
  - **AC:**
    - [ ] isolation verified

- [ ] **P4-061** — Admin guide
  - **Type:** docs · **Area:** web/admin · **Est:** S · **Deps:** P4-060 · **Prio:** P1
  - **Spec:** Per-page runbook: what to do when (queue stuck, abuse report, refund), escalation notes.
  - **AC:**
    - [ ] guide committed

---

## E4.7 — Landing & docs (area: web/public)

- [ ] **P4-062** — Landing page
  - **Type:** feat · **Area:** web/public · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Hero (honest promise + quality-report screenshot), how-it-works 3 steps, quality-gate explainer, pricing CTA, AUP summary link; no dark patterns.
  - **AC:**
    - [ ] honesty lint green

- [ ] **P4-063** — Docs quickstart
  - **Type:** feat · **Area:** web/public · **Est:** M · **Deps:** P4-062 · **Prio:** P1
  - **Spec:** API quickstart (curl/TS/Python) from api-spec; auth, upload, job, webhook snippets.
  - **AC:**
    - [ ] snippets runnable

- [ ] **P4-064** — OpenAPI viewer
  - **Type:** feat · **Area:** web/public · **Est:** M · **Deps:** P4-063 · **Prio:** P1
  - **Spec:** Embedded viewer consuming /openapi.json; version banner.
  - **AC:**
    - [ ] viewer renders spec

- [ ] **P4-065** — Changelog page
  - **Type:** feat · **Area:** web/public · **Est:** S · **Deps:** P4-064 · **Prio:** P1
  - **Spec:** Generated from CHANGELOG.md (build feed); product-honest tone.
  - **AC:**
    - [ ] page renders

- [ ] **P4-066** — Status page
  - **Type:** feat · **Area:** web/public · **Est:** S · **Deps:** P4-065 · **Prio:** P1
  - **Spec:** Public status from readyz snapshot (component table), incident history stub; uptime link.
  - **AC:**
    - [ ] status from readyz

- [ ] **P4-067** — AUP/ToS/Privacy pages
  - **Type:** feat · **Area:** web/public · **Est:** M · **Deps:** P4-066 · **Prio:** P1
  - **Spec:** From compliance doc + privacy commitments; versioned footer; owner review flagged in DECISIONS.md.
  - **AC:**
    - [ ] versions shown

- [ ] **P4-068** — FAQ
  - **Type:** feat · **Area:** web/public · **Est:** S · **Deps:** P4-067 · **Prio:** P1
  - **Spec:** Top 10 questions (quality guarantee, provenance stance, ownership requirement, pricing, retention).
  - **AC:**
    - [ ] answers honest

- [ ] **P4-069** — OG images
  - **Type:** feat · **Area:** web/public · **Est:** S · **Deps:** P4-068 · **Prio:** P1
  - **Spec:** Static OG image set (brand-neutral), per-page overrides for docs.
  - **AC:**
    - [ ] og tags verified

- [ ] **P4-070** — Landing a11y + copy check
  - **Type:** test · **Area:** web/public · **Est:** M · **Deps:** P4-069 · **Prio:** P1
  - **Spec:** axe green; honesty-rule lint: copy assertions (no invisible-mark claims) tested on strings.
  - **AC:**
    - [ ] lint + axe green

---

## E4.8 — Web platform polish (area: web/platform)

- [ ] **P4-071** — i18n scaffold
  - **Type:** feat · **Area:** web/platform · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** next-intl setup, en default, keys structured for hi later; nav-only hi strings as proof (config flag).
  - **AC:**
    - [ ] hi proof strings render

- [ ] **P4-072** — Rate-limit UX
  - **Type:** feat · **Area:** web/platform · **Est:** S · **Deps:** P4-071 · **Prio:** P1
  - **Spec:** 429 handling with friendly copy + retry-after countdown; no silent failures.
  - **AC:**
    - [ ] countdown works

- [ ] **P4-073** — Empty/skeleton states
  - **Type:** feat · **Area:** web/platform · **Est:** S · **Deps:** P4-072 · **Prio:** P1
  - **Spec:** Component set applied to all lists; no content-free screens.
  - **AC:**
    - [ ] applied everywhere

- [ ] **P4-074** — Toast system
  - **Type:** feat · **Area:** web/platform · **Est:** S · **Deps:** P4-073 · **Prio:** P1
  - **Spec:** Unified toasts (success/error/progress), stacking, a11y; PF-code mapping.
  - **AC:**
    - [ ] mapping covered

- [ ] **P4-075** — Web vitals RUM
  - **Type:** feat · **Area:** web/platform · **Est:** S · **Deps:** P4-074 · **Prio:** P1
  - **Spec:** Report CLS/LCP/INP to self-hosted analytics; budget note; fix pass if LCP > 2.5s.
  - **AC:**
    - [ ] vitals within budget

- [ ] **P4-076** — Web CI complete
  - **Type:** chore · **Area:** web/platform · **Est:** M · **Deps:** P4-075 · **Prio:** P1
  - **Spec:** type-check + lint + test + build + axe + e2e smoke job; preview deploy note.
  - **AC:**
    - [ ] CI complete

- [ ] **P4-077** — Web README
  - **Type:** docs · **Area:** web/platform · **Est:** S · **Deps:** P4-076 · **Prio:** P1
  - **Spec:** Run, env vars, structure, where to add pages/components.
  - **AC:**
    - [ ] README committed

---

## E4.9 — API DX (area: dx)

- [ ] **P4-078** — SDK snippets
  - **Type:** feat · **Area:** dx · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** TS + Python + curl client examples (copy-paste runnable) in docs; generated from spec where possible.
  - **AC:**
    - [ ] snippets run

- [ ] **P4-079** — Postman collection
  - **Type:** feat · **Area:** dx · **Est:** S · **Deps:** P4-078 · **Prio:** P1
  - **Spec:** Exported collection v1 (auth to upload to job to result); link from docs; regenerate script.
  - **AC:**
    - [ ] collection imports

- [ ] **P4-080** — Error-code reference
  - **Type:** feat · **Area:** dx · **Est:** M · **Deps:** P4-079 · **Prio:** P1
  - **Spec:** PF-code table page (meaning, action) generated from code registry (single source); CI fails when a new code lacks docs.
  - **AC:**
    - [ ] registry-linked

- [ ] **P4-081** — Rate-limit docs
  - **Type:** feat · **Area:** dx · **Est:** S · **Deps:** P4-080 · **Prio:** P1
  - **Spec:** Tiers, headers, retry semantics; examples.
  - **AC:**
    - [ ] docs committed

- [ ] **P4-082** — Webhooks guide
  - **Type:** feat · **Area:** dx · **Est:** S · **Deps:** P4-081 · **Prio:** P1
  - **Spec:** Signature verification snippets (TS/py), retry semantics, replay window; security notes.
  - **AC:**
    - [ ] snippets verified

- [ ] **P4-083** — Quickstart video script
  - **Type:** feat · **Area:** dx · **Est:** S · **Deps:** P4-082 · **Prio:** P1
  - **Spec:** Text storyboard for a 90s demo video (owner records later); honest claims only.
  - **AC:**
    - [ ] script committed

- [ ] **P4-084** — API changelog
  - **Type:** feat · **Area:** dx · **Est:** S · **Deps:** P4-083 · **Prio:** P1
  - **Spec:** /docs/changelog-api fed by OpenAPI diff summary + manual notes.
  - **AC:**
    - [ ] page live

- [ ] **P4-085** — DX review
  - **Type:** chore · **Area:** dx · **Est:** S · **Deps:** P4-084 · **Prio:** P1
  - **Spec:** Self-review: can a new dev integrate in < 30 min? Fix top frictions; record in night report.
  - **AC:**
    - [ ] frictions fixed

---

## E4.10 — Notifications UX (area: web/notify)

- [ ] **P4-086** — Email brand pass
  - **Type:** feat · **Area:** web/notify · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Templates restyled (tokens shared with web), dark-mode friendly, plain-text parity check.
  - **AC:**
    - [ ] parity verified

- [ ] **P4-087** — In-app toasts wiring
  - **Type:** feat · **Area:** web/notify · **Est:** S · **Deps:** P4-086 · **Prio:** P1
  - **Spec:** Job done/failed toasts when tab active (SSE present); suppressed on job page.
  - **AC:**
    - [ ] suppression correct

- [ ] **P4-088** — Browser notifications
  - **Type:** feat · **Area:** web/notify · **Est:** S · **Deps:** P4-087 · **Prio:** P1
  - **Spec:** Opt-in Notification API for completion; permission UX; graceful denial.
  - **AC:**
    - [ ] opt-in flow green

- [ ] **P4-089** — Quota warning banner
  - **Type:** feat · **Area:** web/notify · **Est:** S · **Deps:** P4-088 · **Prio:** P1
  - **Spec:** 80/100% banners (usage API); upgrade CTA; dismissible per day.
  - **AC:**
    - [ ] dismissal respected

- [ ] **P4-090** — Maintenance banner
  - **Type:** feat · **Area:** web/notify · **Est:** S · **Deps:** P4-089 · **Prio:** P1
  - **Spec:** Kill-switch driven banner (read-only mode notice) when intake paused.
  - **AC:**
    - [ ] banner reflects flag

- [ ] **P4-091** — Notification tests
  - **Type:** test · **Area:** web/notify · **Est:** S · **Deps:** P4-090 · **Prio:** P1
  - **Spec:** Component tests for banners/toasts; e2e for notification prompt path.
  - **AC:**
    - [ ] tests green

---

## E4.11 — Web performance (area: web/perf)

- [ ] **P4-092** — Code splitting
  - **Type:** feat · **Area:** web/perf · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Route-level splitting; dynamic import for slider/editor components; bundle analysis baseline recorded.
  - **AC:**
    - [ ] baseline recorded

- [ ] **P4-093** — Image optimization
  - **Type:** feat · **Area:** web/perf · **Est:** S · **Deps:** P4-092 · **Prio:** P1
  - **Spec:** next/image for static assets, OG sizes, favicon set; no layout shift (CLS budget).
  - **AC:**
    - [ ] CLS within budget

- [ ] **P4-094** — Lighthouse >= 90
  - **Type:** feat · **Area:** web/perf · **Est:** S · **Deps:** P4-093 · **Prio:** P1
  - **Spec:** CI lighthouse on landing + app shell; fix pass to meet budget (perf/a11y/SEO/best-practices).
  - **AC:**
    - [ ] all >= 90

- [ ] **P4-095** — Web vitals RUM wiring
  - **Type:** feat · **Area:** web/perf · **Est:** S · **Deps:** P4-094 · **Prio:** P1
  - **Spec:** Connect analytics real numbers after seed traffic; regression note.
  - **AC:**
    - [ ] RUM live

- [ ] **P4-096** — Bundle budget
  - **Type:** feat · **Area:** web/perf · **Est:** S · **Deps:** P4-095 · **Prio:** P1
  - **Spec:** CI check: route chunks under budget (200KB gzip first-load js); fail with fix hints.
  - **AC:**
    - [ ] budget gate green

- [ ] **P4-097** — Caching headers
  - **Type:** feat · **Area:** web/perf · **Est:** S · **Deps:** P4-096 · **Prio:** P1
  - **Spec:** Static immutable caching, API no-cache; caddy notes; verified with curl.
  - **AC:**
    - [ ] headers verified

---

## E4.12 — Support view (area: web/support)

- [ ] **P4-098** — Support read-only UI
  - **Type:** feat · **Area:** web/support · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Support role sees user detail + job inspect (no actions); banner explains scope.
  - **AC:**
    - [ ] scope enforced

- [ ] **P4-099** — Refund flow UI
  - **Type:** feat · **Area:** web/support · **Est:** S · **Deps:** P4-098 · **Prio:** P1
  - **Spec:** Checklist modal (deep-link + fields) for support; audited note only (no direct refund API v1).
  - **AC:**
    - [ ] checklist complete

- [ ] **P4-100** — Manual QC review screen
  - **Type:** feat · **Area:** web/support · **Est:** S · **Deps:** P4-099 · **Prio:** P1
  - **Spec:** Flagged jobs (quality_failed/abuse) list with side-by-side compare + decision buttons (approve/keep blocked), audited.
  - **AC:**
    - [ ] decisions audited

- [ ] **P4-101** — Support macros
  - **Type:** feat · **Area:** web/support · **Est:** S · **Deps:** P4-100 · **Prio:** P1
  - **Spec:** Canned response snippets for email replies (docs list; mail client out of scope).
  - **AC:**
    - [ ] macros listed

- [ ] **P4-102** — Support suite
  - **Type:** test · **Area:** web/support · **Est:** S · **Deps:** P4-101 · **Prio:** P1
  - **Spec:** Role gating, QC review flow e2e-lite, audit rows on decisions.
  - **AC:**
    - [ ] suite green
