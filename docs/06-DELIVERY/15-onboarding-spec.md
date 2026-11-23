# Onboarding spec (P1-100)

The UI flow contract for Phase 4 (the console): the screens a new
user walks through, the states each screen can be in, the exact copy
the screens use, and the drop-off metric events the console emits.
FR-C-01 — the onboarding family (the P1-096 precedent).

The contract is a READ of what the API already does — it invents no
endpoint and changes no behavior. Every claim below names its source
in the API spec (`docs/03-ARCHITECTURE/03-api-spec.md`), the
onboarding service (`apps/api/src/services/onboarding.rs`), or the
consent service (`apps/api/src/services/consent.rs`). Where the
contract is a decision this ticket makes (screen order, event names,
copy that exists nowhere else), it says so, and ASSUMPTION-061
records it.

## The flow in one paragraph

Register (accepting the current ToS + AUP versions) → verify email
(the link in the verification mail) → the onboarding checklist (five
steps, in order) → first upload → first job → optional API key.
`all_done` is the four required steps; the key never blocks it. The
checklist is the single source of truth for progress: the console
reads `GET /v1/me/onboarding` and renders what it returns — it never
computes step state itself.

## Screens

### 1. Register

- **Route (Phase 4):** `/register` (the console's own route; the API
  is `POST /v1/auth/register`).
- **Fields:** email, password, two version checkboxes — "I accept the
  Terms of Service (v{tos_current_version})" and "I accept the
  Acceptable Use Policy (v{aup_current_version})". The versions shown
  are the CURRENT versions: the register body carries
  `accept_tos_version` + `accept_aup_version`, and each must equal its
  current version, else 422 `tos_version_mismatch` /
  `aup_version_mismatch` (the P1-095 field errors, tos first).
- **Success:** 201 + the verification mail is sent. The console goes
  to screen 2. The checklist already reads 1/5: `accept_tos` is
  complete at creation (register accepts at creation and writes no
  audit row, so its `completed_at` is the account's `created_at`).
- **Failure states:** 422 field errors (weak/common password,
  bad email, version mismatch — shown inline on the field, never a
  bare code); 429 on the per-email register bucket (5 burst / 1 per
  min) — "Too many attempts. Try again in a minute." with
  `Retry-After`.
- **Copy:** heading "Create your account". Subcopy: the §6 approved
  sentence verbatim — "Removes visible marks from AI media you own,
  with a verifiable no-quality-loss report." No "invisible",
  "undetectable", "defeats SynthID", or similar anywhere (the
  compliance rule in `05-compliance-aup.md` §6; the Phase 5
  copy-accuracy check enforces it).

### 2. Verify email

- **Route (Phase 4):** `/verify-email` (the link in the mail lands
  here with `?token=…`; the API is `POST /v1/auth/verify-email`,
  token → 204).
- **States:**
  - *Waiting:* "Check your inbox — we sent a verification link to
    {email}." A "Resend link" action calls
    `POST /v1/auth/resend-verification` (always 204 — no enumeration:
    an unknown email, an already-verified user, and an over-limit
    resend all answer the same 204 with no mail). At most 3 tokens
    minted per user per hour; a resend invalidates the earlier link
    (one live token per user). The token is single-use, 24 h TTL.
  - *Verified:* the 204 lands → "Your email is verified." The
    welcome mail is sent in the same transaction as the token consume
    (exactly one per account, P1-097) — subject "Welcome to Detag",
    carrying the quickstart link `{base_url}/quickstart`.
  - *Expired / used token:* the link was already used or is past 24 h
    → "This link has expired." + the resend action. Unknown, used,
    and expired tokens are deliberately indistinguishable in the
    response (no enumeration).
- **Login does NOT require a verified email** (api-spec). An
  unverified user can log in and sees the checklist with
  `verify_email` incomplete. The gate that needs verification is
  job-create: the `require_verified` guard answers 403 `PF-1009`
  (Phase 2 — the guard exists today as a pure function with its own
  tests, and the Phase 2 handler calls it first; the live stub
  handler does not call it yet, so an unverified user currently
  reaches the 501 stub after the quota + attestation gates).

### 3. Onboarding checklist (the home screen until `all_done`)

- **Route (Phase 4):** `/` (or `/onboarding` while `all_done` is
  false — the console's choice; the API is `GET /v1/me/onboarding`).
- **Render rule:** render the five steps exactly as returned, in the
  returned order, with the returned titles — the titles are the API's
  canonical copy (`Verify your email`, `Accept the Terms of Service`,
  `Upload your first image`, `Create your first job`,
  `Create an API key`). Do not reword them in the console: the API
  and the UI must agree word for word.
- **Progress:** `completed_count / total_count` (a fully-onboarded
  user without a key reads 4/5 with `all_done: true` — the key is
  optional and never blocks `all_done`). A completed step shows its
  `completed_at` ("Done 22 Nov 2026"); an incomplete step shows its
  action.
- **Step actions (each deep-links to its screen):**

  | Step | Action | Destination |
  | --- | --- | --- |
  | `verify_email` | "Verify email" | screen 2 (resend) |
  | `accept_tos` | "Review and accept" | the consent banner (screen 3b) |
  | `first_upload` | "Upload an image" | the upload dropzone (FR-W-01) |
  | `first_job` | "Create a job" | the job-create form for the first upload |
  | `create_api_key` (optional) | "Create a key" | the API-keys page |

- **States:**
  - *Fresh user:* 1/5, only `accept_tos` done (`completed_at` is the
    account's creation time).
  - *Bump reopened:* a config bump flips `up_to_date` false and the
    `accept_tos` step reopens with `completed_at` null until the
    re-accept lands (P1-096). The step's time after the re-accept is
    the re-accept's `user.consent_accepted` audit row — never the old
    re-accept row, never the account's creation.
  - *All done:* the checklist collapses to a "You're set up" state
    with the four required steps ticked and the optional key still
    offered. `all_done` is the console's signal to stop showing the
    onboarding home and show the normal workspace.
- **Polling:** the console re-reads the checklist after every action
  that can complete a step (verify, re-accept, upload, job, key).
  The read writes nothing (no audit row on a read, P1-046) and emits
  no event — the steps' `completed_at` values ARE the completion
  events (P1-096).

### 3b. Consent banner (re-accept, shown whenever `up_to_date` is false)

- **Source:** `GET /v1/me` consent section (accepted tos/aup
  versions, current versions, `up_to_date`). Shown on every
  authenticated screen until cleared — a bump gates NO other endpoint
  (ASSUMPTION-056), so the banner is a nudge, never a block.
- **Copy:** "Our Terms of Service and Acceptable Use Policy have
  changed. Please review and accept the new versions to continue."
  (Spec-defined copy — it exists nowhere else.)
- **Action:** "Review and accept" → `POST /v1/me/consent` with the
  CURRENT versions. A stale re-accept is 422 with both mismatch
  codes; a current re-accept is 200, updates the row, and writes
  exactly one `user.consent_accepted` audit row; an already-current
  re-accept is a 200 no-op (no write, no audit). On success the
  banner clears and the `accept_tos` step closes.

### 4. First upload

- **Screen:** the upload dropzone (FR-W-01: dropzone, staged
  progress upload → queue → detect → inpaint → encode → QC,
  before/after compare, download, job history).
- **API (target contract):** `POST /v1/uploads` (filename, bytes,
  kind → upload id, part size, presigned part URLs, multipart) with
  the attestation required in the body; then
  `POST /v1/uploads/{id}/complete` (parts + sha256 verify → `ready`).
- **Attestation at upload time:** the upload body carries the
  ownership attestation (text version + client) — the client attests
  to the AUP text in `05-compliance-aup.md` §2, version
  `CURRENT_TEXT_VERSION` ("v1" today). The version is recorded
  evidence, not a gate input (ADR-0062): a row captured at a
  superseded version still satisfies the job gate.
- **Honest limitation (Phase 2, not this spec):** `POST /v1/uploads`
  is a 501 stub today (route table), so no user can complete
  `first_upload` over HTTP until Phase 2 lands the intake. The
  screen exists in this spec as the target contract; until then the
  checklist's `first_upload` step never completes in production and
  `all_done` is unreachable. Do not fake it: the console must not
  mark the step done on a 501.

### 5. First job

- **Screen:** the job-create form for the first upload (mode,
  hints, format policy — the `POST /v1/jobs` body).
- **Gates, in order:** quota pre-check (429 `PF-3001`/`PF-3002`/
  `PF-3008` with `X-Quota-*` headers and `upgrade_url`) → the
  attestation gate (no row for the caller + upload → 403 `PF-4001`
  `attestation_missing`, with the quota headers) → the job. A
  foreign upload is 404 `PF-2010` before the gate runs (the P1-045
  IDOR rule). An unverified user is 403 `PF-1009` before either
  (Phase 2 — see screen 2; the live stub handler does not call the
  guard yet).
- **Copy on the 403:** "We need your ownership confirmation for
  this upload before a job can run." + a link back to the upload
  screen (the attestation is captured at upload time — screen 4).
  (Spec-defined copy.)
- **Honest limitation (Phase 2):** `POST /v1/jobs` answers the 501
  stub after the gates today, so `first_job` cannot complete over
  HTTP until Phase 2. Same rule as screen 4: never mark it done on
  a 501.

### 6. API key (optional)

- **Screen:** the API-keys page. `POST /v1/api-keys` (name, scopes
  ≥ 1) → 201, the full secret shown ONCE (`pf_live_` + 32 chars);
  listings show prefix + last4 only.
- **Gates:** Free is 403 `PF-3003` (the `api_keys` feature flag) —
  the console shows the upgrade prompt, not a dead form. Key
  creation needs NO verified email (ASSUMPTION-015) and no plan gate
  beyond the feature flag. Max 10 live keys per user (revoked do not
  count); the 11th is 409 `PF-2011`.
- **Copy on the Free gate:** "API keys are a Pro feature. Upgrade
  to create keys." + a link to `/plans`. (Spec-defined copy.)
- **Done state:** a minted key — live or later revoked — still
  counts: the step proves the action was done once, not that a key
  is currently held (ASSUMPTION-057).

## Copy points (the full list)

| Where | Copy | Source |
| --- | --- | --- |
| Register heading | "Create your account" | spec-defined |
| Register subcopy | "Removes visible marks from AI media you own, with a verifiable no-quality-loss report." | §6 approved sentence, verbatim (`05-compliance-aup.md` §6) |
| Register version checkboxes | "I accept the Terms of Service (v{version})" / "I accept the Acceptable Use Policy (v{version})" | spec-defined (versions from the API) |
| Verify waiting | "Check your inbox — we sent a verification link to {email}." | spec-defined |
| Verify verified | "Your email is verified." | spec-defined |
| Verify expired | "This link has expired." | spec-defined |
| Register rate-limit | "Too many attempts. Try again in a minute." | spec-defined |
| Consent banner | "Our Terms of Service and Acceptable Use Policy have changed. Please review and accept the new versions to continue." | spec-defined |
| Job 403 attestation | "We need your ownership confirmation for this upload before a job can run." | spec-defined |
| API-key Free gate | "API keys are a Pro feature. Upgrade to create keys." | spec-defined |
| Step titles (5) | The API's titles, verbatim — see screen 3 | `onboarding.rs` |
| Welcome mail | Subject "Welcome to Detag"; body per `crates/mailer/templates/welcome.*.tera` (quality-promise one-liner = the §6 sentence; quickstart link `{base_url}/quickstart`) | P1-097 |

Rules for all copy: no "invisible", "undetectable", "defeats
SynthID", or similar (compliance §6); no invented dates, employers,
or metrics; error copy names the fix, never a bare PF code (the code
stays in the response for the console to log, not to show).

## Drop-off metric events

The funnel is the five steps in order, starting at register:

`register` → `verify_email` → `accept_tos` → `first_upload` →
`first_job` → (`create_api_key`, optional) → `all_done`.

Drop-off at a step means: the user reached the step and had not
completed it 7 days after registering (the window is this ticket's
choice — ASSUMPTION-061). The funnel is measured per weekly
registration cohort.

### The event list (client-side, Phase 4)

Event names follow the audit-action convention (dotted,
lowercase, `onboarding.` prefix). No event carries PII: no email,
no raw IP, no token, no secret — only the step id and timings
(NFR-42). Until Phase 4 wires them, these events do not exist
anywhere: the API emits nothing (the checklist is a read model),
and drop-off can only be derived from `completed_at` values
(cohort analysis on `users.created_at` vs each step's time).

| Event | Fired when | Properties |
| --- | --- | --- |
| `onboarding.registered` | `POST /v1/auth/register` returns 201 | `cohort_week` (ISO week of `created_at`) |
| `onboarding.step_viewed` | a checklist step's screen is shown | `step` (step id), `completed` (bool) |
| `onboarding.step_completed` | a step flips to completed (the checklist re-read after the action shows it) | `step`, `seconds_since_register` |
| `onboarding.resend_verification` | the resend action is used | `attempt` (1–3 in the hour) |
| `onboarding.consent_banner_shown` | the banner renders while `up_to_date` is false | `tos_current_version`, `aup_current_version` |
| `onboarding.consent_reaccepted` | `POST /v1/me/consent` returns 200 with a write | `tos_version`, `aup_version` |
| `onboarding.all_done` | the checklist first reads `all_done: true` | `seconds_since_register`, `key_created` (bool) |
| `onboarding.abandoned` | 7 days after register with `all_done` false (computed, not fired live) | `last_completed_step` (step id or `none`) |

### How drop-off is read (until the events exist)

For each weekly cohort, count users by their furthest completed
step (`verify_email` → `accept_tos` → `first_upload` → `first_job`),
using the `completed_at` values (a step with a time is done; a step
with no time is not). The drop-off at a step is the share of the
cohort that completed the previous step but not this one. Two
readings this method CANNOT give (recorded honestly): it cannot tell
a user who never saw a screen from one who saw it and left (no
`step_viewed`), and it cannot time the abandonment (no event
timestamps — only the completion times). Those need the Phase 4
events above.

## Pointers

- API: `GET /v1/me/onboarding` + `POST /v1/me/consent` +
  `GET /v1/me` (consent section) — `docs/03-ARCHITECTURE/03-api-spec.md`
  rows 680/683/684.
- Service: `apps/api/src/services/onboarding.rs` (step order, titles,
  `all_done` rule, `completed_at` semantics) +
  `apps/api/src/services/consent.rs` (field-error order, no-op 200).
- Tests: `apps/api/tests/onboarding.rs` (4 live tests incl. the AC)
  + `apps/api/tests/consent_attestation.rs` (P1-099, the bump journey
  + the reopening).
- Mail: `docs/06-DELIVERY/05-email-ops.md` (`welcome` row) +
  `crates/mailer/templates/welcome.*.tera`.
- Compliance: `docs/02-REQUIREMENTS/05-compliance-aup.md` §2
  (attestation text) + §6 (approved copy sentence).
- Console (Phase 4): FR-W-01 (upload-centric console) + FR-W-06
  (public pages incl. docs quickstart at `/quickstart`, P4-063 —
  the welcome mail's link 404s until then).

## Honesty

- The spec is a contract for screens that do not exist yet (Phase 4)
  over steps two of which cannot complete today: `first_upload` and
  `first_job` need the Phase 2 intake (`POST /v1/uploads` and
  `POST /v1/jobs` are 501 stubs), so `all_done` is unreachable in
  production until Phase 2. The console must never mark a step done
  on a 501.
- The drop-off events are a list, not an implementation: no analytics
  transport is chosen, no event is emitted anywhere today, and the
  7-day abandonment window is this ticket's choice (ASSUMPTION-061),
  not a measured threshold.
- The copy in this spec is the console's to use as written, except
  the §6 sentence and the step titles, which are quoted verbatim
  from their sources and must not be reworded.
