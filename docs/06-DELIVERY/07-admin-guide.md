# Admin guide — roles, the permission matrix, and audit (P1-047)

For the people who operate Detag: what each admin role may do,
which routes exist, and how to change the matrix safely. Every
claim here is per the single source of truth,
`apps/api/src/authz.rs` (`Capability` + `for_route`), and proven
by the suites in `apps/api/tests/authz_matrix.rs`,
`admin_guard.rs`, `support_readonly.rs`, `api_key_matrix.rs`,
and `privileged_audit.rs` — if this page and the code ever
disagree, the code wins and this page is the bug.

## Roles

Three roles exist on the `users` table: `user`, `support`,
`admin` (FR-A-07). There is no self-serve way to become an
admin or support — the role is set by the owner out of band
(a direct UPDATE; the tests do the same). A plain user holds
no admin capability at all, and neither does an anonymous
caller.

- **admin** — everything: all reads and all writes, including
  impersonation.
- **support** — read-only: admitted on every `.read`
  capability, denied on every write and on impersonation.
- **user** — no admin route at all.

Two rules ride on top of the role table, for every row:

- An **API key never reaches an admin route** — even a key
  minted by an admin user ("no admin ever", P1-040). Keys are
  for the `/v1` API surface; the admin surface takes user
  tokens only.
- An **impersonation token never reaches an admin route** —
  the token acts as its target user (P1-010), so it carries
  no admin rights even when the impersonator is an admin.

## Denial shape

- No credential (or a bad/expired token): 401 `PF-1001`.
- A verified caller the matrix does not admit (wrong role, an
  API key, an impersonator): 403 `PF-1002` — the same code as
  every other authorization denial, so a client cannot tell a
  wrong role from a key or an impersonator.
- A write to an unknown target id: 404 `PF-2010` (no
  enumeration — a foreign id reads exactly like an unknown
  one).
- A malformed body on a write: 422 `PF-2002`.

## The matrix

One row per admin route (21 routes). **Capability** is the
stable code string the guards consult; **Admitted** is who
the matrix lets in. **Status** is whether the handler is
live or still a 501 stub — a stub route is still guarded
(P1-043): the guard rejects first, the 501 answers after,
so an unadmitted caller never sees a 501. **Audit** is the
action the P1-046 middleware writes for a successful write;
reads are never audited — the trail records actions, not
views — and neither are denials, 404s, or 501 stubs.

| Method | Path | Capability | Admitted | Status | Audit |
| --- | --- | --- | --- | --- | --- |
| GET | `/v1/admin/metrics/dashboard` | `admin.dashboard.read` | admin, support | stub | — |
| GET | `/v1/admin/users` | `admin.users.read` | admin, support | live | — |
| GET | `/v1/admin/users/{id}` | `admin.users.read` | admin, support | live | — |
| POST | `/v1/admin/users/{id}/suspend` | `admin.users.write` | admin | live | `user.suspended` |
| POST | `/v1/admin/users/{id}/restore` | `admin.users.write` | admin | live | `user.restored` |
| POST | `/v1/admin/users/{id}/warn` | `admin.users.write` | admin | live | `user.warned` |
| POST | `/v1/admin/users/{id}/terminate` | `admin.users.write` | admin | live | `user.terminated` |
| POST | `/v1/admin/users/{id}/quota-grant` | `admin.users.write` | admin | live | `admin.users.quota_granted` |
| POST | `/v1/admin/users/{id}/impersonate` | `admin.users.impersonate` | admin | live | `user.impersonated` |
| GET | `/v1/admin/jobs` | `admin.jobs.read` | admin, support | live | — |
| POST | `/v1/admin/jobs/{id}/reprocess` | `admin.jobs.write` | admin | stub | `admin.jobs.reprocessed` |
| POST | `/v1/admin/jobs/{id}/cancel` | `admin.jobs.write` | admin | stub | `admin.jobs.cancelled` |
| GET | `/v1/admin/dlq` | `admin.dlq.read` | admin, support | stub | — |
| GET | `/v1/admin/flags` | `admin.flags.read` | admin, support | stub | — |
| PUT | `/v1/admin/flags` | `admin.flags.write` | admin | stub | `admin.flags.updated` |
| GET | `/v1/admin/plans` | `admin.plans.read` | admin, support | live | — |
| PUT | `/v1/admin/plans` | `admin.plans.write` | admin | live | `admin.plans.updated` |
| POST | `/v1/admin/billing/catalog-sync` | `admin.billing.write` | admin | live | `admin.billing.catalog_synced` |
| POST | `/v1/admin/billing/subscriptions/{id}/cancel` | `admin.billing.write` | admin | live | `billing.subscription.status_changed` |
| GET | `/v1/admin/audit` | `admin.audit.read` | admin, support | live | — |
| GET | `/v1/admin/abuse-reports` | `admin.abuse.read` | admin, support | live | — |
| POST | `/v1/admin/abuse-reports/{id}/resolve` | `admin.abuse.write` | admin | live | `admin.abuse_reports.resolved` (+ `abuse.report_reviewing` / `user.suspended`) |
| GET | `/v1/admin/attestations` | `admin.attestations.read` | admin, support | live | — |
| GET | `/v1/admin/system` | `admin.system.read` | admin, support | stub | — |

Notes on the live routes:

- **Suspend / restore** (P1-008): suspend takes
  `{"reason": "..."}` (1–500 characters after trimming, no
  control characters), revokes every live session, holds
  every queued job, and audits `user.suspended` with the
  reason and `{sessions_revoked, jobs_held}` in `after`.
  Suspending an already-suspended account is a 200 no-op
  (no second audit). Restore returns the account to
  `active`, releases held jobs to `queued`, leaves sessions
  revoked (the user logs in again), and audits
  `user.restored`. A suspend cancels a pending deletion.
- **Enforcement ladder** (P1-091, FR-C-02): warning →
  suspension → termination, one endpoint per rung. `warn`
  takes `{"reason": "..."}` and does not change the status —
  each call is a new warning event, audited `user.warned`
  (the `warnings` count in `after` is the running total)
  and mailed (`enforcement-warning`). It requires an
  `active` account. `terminate` takes the same body and
  requires a `suspended` account — the ladder is
  sequential, an active account cannot be terminated
  outright (422). It runs the P1-007 deletion grace
  (`deletion_pending`, 30 days), revokes every session,
  holds every queued job, audits `user.terminated`, and
  mails (`enforcement-termination`). Terminating an
  already-terminated account is a 200 no-op. The rung
  comes from the status plus the `user.warned` count, so a
  warning survives a restore — a warned-then-restored
  repeat offender goes straight to suspension next time.
  Termination is sticky: the terminated user cannot log
  back in (403 `PF-1004`), unlike a self-requested
  deletion, which a login cancels. Termination does not
  cancel subscriptions — cancel those separately (P1-071).
  Suspension mails the user too, from the P1-008 endpoint
  and from the abuse-queue resolve, on a real transition
  only.
- **Impersonate** (P1-010): mints a 15-minute scoped token
  that acts as the target user (`expires_in: 900`,
  `impersonator_id` in the claims, no refresh, no session
  row). The console MUST show both ids for the whole
  session. Audited once as `user.impersonated`.
- **Users search** (P1-009): `GET /v1/admin/users?query=`
  (email / display-name / id prefix), cursor pagination,
  returns plan + status + today's usage per user. Support
  sees the same rows — no key material is ever in them.
- **User view** (P1-106): `GET /v1/admin/users/{id}`
  returns the composite for one user — profile, plan,
  usage summary (today + all-time totals), the 10 newest
  jobs, and the live sessions (P1-019 shape: device label +
  ip hash). No secrets anywhere in the shape (ADR-0065):
  no password hash, no refresh hash, no provider ids, no
  job params. A user with no subscription is Free, no jobs
  or no live sessions is `[]`; only an unknown id is 404.
  No audit row (a read creates nothing).
- **Jobs list** (P1-038): `GET /v1/admin/jobs` newest
  first, cursor pagination, `?user_id=` / `?key_id=`
  filters. Each row shows the creating key's display prefix
  (`key_prefix`); a web-created job shows no prefix at all
  — never the secret, never its hash.
- **Plans editor** (P1-056): `GET /v1/admin/plans` lists
  the catalog cheapest first (code, price in paise,
  currency, Stripe price id, the seven parsed entitlement
  keys); support reads it, only admin edits it. `PUT` is a
  batch edit — per-plan updates plus a `remove` list — and
  applies in one transaction: a plan in neither list is
  untouched, so an edit never deletes by omission. The
  entitlements object must be complete (all seven keys);
  Free stays ₹0 and takes no Stripe price. A plan cannot be
  removed while any subscription references it (409
  `PF-2012`), and Free can never be removed. The edit
  invalidates the plan cache at once, and audits once as
  `admin.plans.updated` with the changed codes in `after`. The pricing
  change procedure (where defaults live, Stripe dashboard steps,
  rollout) is `docs/06-DELIVERY/10-pricing-config.md`.
- **Support cancel** (P1-071): `POST /v1/admin/billing/subscriptions/{id}/cancel` cancels a subscription immediately, for the support workflow — admin only (support stays read-only, so support files the request and an admin executes it). Body is the audited note (`{"reason": "..."}`, 1–500 chars); the row moves to `canceled` now (not at period end, like a P1-069 downgrade-to-Free), a pending scheduled change is mooted, the `subscription-canceled` mail goes out, and the call audits as `billing.subscription.status_changed` with the admin as actor. Already-canceled is a 200 no-op. The response carries the ToS policy snippet and the Stripe-dashboard refund guidance — refunds are dashboard-only, and the Stripe subscription must ALSO be canceled in the dashboard, or billing continues (ASSUMPTION-034). The refund & cancel policy is `docs/06-DELIVERY/10-pricing-config.md`.
- The public abuse intake (`POST /v1/abuse`) is **not** an
  admin route: it takes no credential and never reaches the
  matrix or the audit middleware.
- **Abuse queue** (P1-090): `GET /v1/admin/abuse-reports`
  lists newest first, `?status=` filters to `open` |
  `reviewing` | `resolved`; every item carries `sla_due_at`
  (`created_at` + 72 h) and `sla_breached`. The operator
  procedure — triage checklist, evidence preservation
  within the artifact TTLs, and the DMCA-style takedown
  response stub — is `docs/06-DELIVERY/14-abuse-ops-playbook.md`
  (P1-093). `POST
  /v1/admin/abuse-reports/{id}/resolve` takes
  `{"action": "start_review" | "reject_job" |
  "suspend_user" | "dismiss", "reason": "...", "notes"?}`:
  `start_review` picks the report up, `reject_job` sets the
  reported job's status to `rejected`, `suspend_user` runs
  the full P1-008 sequence on the reported user, `dismiss`
  drops it — all three terminal actions resolve the report
  (`dismissed` | `job_rejected` | `user_suspended`) with
  `resolved_at` / `resolved_by` stamped. Resolving an
  already-resolved report is a 200 no-op; `reject_job` needs
  a job on the report and `suspend_user` needs a user (both
  422 otherwise).

- **Attestation review** (P1-098): `GET
  /v1/admin/attestations` lists newest first, `?flagged=`
  filters to flagged rows only (`true`), unflagged only
  (`false`), or all rows (absent — any other value is 422).
  Every item carries `velocity_count` (the user's
  attestation count in the 24 h window ending at that row's
  `created_at`), `flagged` (the count exceeds 10),
  `flag_reasons` (`["velocity"]` when flagged, `[]`
  otherwise), and `abuse_queue_url` (the abuse queue for
  that user). The flag is a review signal, never a block —
  the queue only surfaces rows for a human to open next to
  the abuse queue; there is no resolve/flag action in v1
  (read-model only). The `user_id` filter on `GET
  /v1/admin/abuse-reports` is what the link lands on: an
  unknown user is an empty page, a malformed uuid is 422.

## The audit trail

A successful admin write writes exactly one audit row —
actor (the admin), action (the table above), target type +
id, and before/after where the service has them. The live writes write their row inside the same transaction as
the mutation (P1-008's atomicity) and mark the response
`AuditHandled`; every other write is audited by the P1-046
middleware from the route metadata, so a handler can never
forget its row. One exception to "exactly one": a queue
suspension writes two rows in one transaction —
`user.suspended` for the suspension itself plus
`admin.abuse_reports.resolved` for the resolution (P1-090) —
because the suspension is a privileged action of its own
and the trail must show it under its own name. An audit insert that fails answers 500: a
privileged action without its audit row is a failed request,
not a silent gap — re-read state rather than blindly
retrying (the admin writes are idempotent, so a retry after
such a 500 is safe).

What is NOT audited: reads, denials (403), unknown targets
(404), and admitted stub calls (501). The trail records
actions, not attempts or views.

## Change procedure — matrix + tests together

The matrix is the single source; the tests are the registry
that pins it. A change to one without the other is
incomplete and the suite will fail you. For any admin-route
change, do all of these in the same commit:

1. **Capability** (`apps/api/src/authz.rs`): if the change
   is a new operation, add a `Capability` variant with its
   stable code string — never a bare string elsewhere. The
   code string is what the docs, the audit trail, and the
   test registry name.
2. **Route row**: add the route to `Capability::for_route`
   (method + template, `{id}` stays a template). If the
   route is a write, add its action to
   `audit_action_for` and its target type to
   `audit_target_type_for` in the same change — a write
   without an audit row fails the
   `audit_actions_cover_every_admin_write_and_no_read` unit
   test.
3. **Guard**: wire the route to `AdminContext` (writes) /
   `AdminReadContext` (reads) — or `admin_stub` /
   `admin_read_stub` while its handler has not landed.
   Never wire an admin guard to a non-admin route:
   `for_route` returns `None` there and the guards fail
   closed.
4. **Tests**: the registry pins the change three ways and
   each must be updated with it —
   `apps/api/tests/authz_matrix.rs` (completeness: a route
   without a row fails; truth-table: every row resolves
   through `for_route` and admits exactly the promised
   roles; live walk: the live routes × anonymous / user /
   support / admin / an admin user's key),
   `apps/api/tests/admin_guard.rs` (the 21-route walk, the
   audit proof, the denied/stub audit-nothing proof), and
   `apps/api/tests/api_key_matrix.rs` if the counts move
   (its rows are per `/v1` route, not per admin route — a
   new admin route flips one row `Unprotected` →
   `Forbidden`). A new admin write also belongs in
   `apps/api/tests/privileged_audit.rs` (exactly one row,
   actor + target + before/after).
5. **Docs**: update the table above and the Admin section
   of the API spec (`docs/03-ARCHITECTURE/03-api-spec.md`).
   If the change adds a capability, say so in the matrix
   table's notes; if it changes who is admitted, say so in
   the Roles section.
6. **Verify**: targeted binaries first (`privileged_audit`,
   `admin_guard`, `authz_matrix`, `api_key_matrix`,
   `route_table`, the `detag-api` lib tests), then the full
   `just verify` plus the drift checks (`just
   openapi-check`, `just route-table-check`, `just
   config-table-check`, `just env-check`, `just
   sqlx-prepare-check`, `just snapshots-check`) — all green
   before committing. One conventional commit per ticket;
   the linter (`just lint-commits`) accepts the ticket id in
   the subject's ticket position.

What never changes without an owner decision: admitting a
new role to a write capability, letting API keys reach the
admin surface, or auditing reads. Those are policy, not
plumbing — record them in
`docs/09-STATE/DECISIONS.md` tagged `owner-input-needed`
first.
