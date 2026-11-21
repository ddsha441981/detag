# Abuse ops playbook (P1-093)

> Operator procedure for the abuse queue: triage, evidence, and the
> DMCA-style takedown response. This is a **procedure doc**, not a
> product change — the endpoints it names are the P1-089 intake, the
> P1-090 queue, and the P1-091 ladder, all live. The takedown
> response in §4 is a **stub**: it is not legal advice and has had
> no legal review — the owner must verify it with counsel before
> it is sent to anyone (see ASSUMPTION-054).

## 1. What exists today

- **Intake** — `POST /v1/abuse`, public, no credential. One
  transaction files the `abuse_reports` row, writes the
  `abuse.report_filed` audit row, and enqueues the
  `abuse-report-ack` mail. The honeypot field `website` drops
  bots before any work (a filled field answers the same 201
  shape with a random id — the drop is visible in no log and
  no counter). Per-IP rate limit `abuse-intake` 5 burst / 5 per
  min; over it is 429 `PF-3007`. An unknown job id is 404
  `PF-2010`.
- **Queue** — `GET /v1/admin/abuse-reports`, admin + support.
  `?status=` filters to `open` | `reviewing` | `resolved`.
  Every item carries `sla_due_at` (`created_at` + 72 h) and
  `sla_breached`. The item shows the reporter's contact
  (`reporter_contact`, stored in the row) — but the **audit**
  row for the filing carries only the contact's sha256
  (`email_hash`), never the address.
- **Resolve** — `POST /v1/admin/abuse-reports/{id}/resolve`,
  admin only. Body `{"action": "start_review" | "reject_job" |
  "suspend_user" | "dismiss", "reason": "...", "notes"?}`.
  `start_review` picks the report up (writes
  `abuse.report_reviewing`, no resolved row);
  `reject_job` sets the reported job's status to `rejected`;
  `suspend_user` runs the full P1-008 sequence on the
  reported user (and writes `user.suspended` under its own
  name, alongside the resolution row); `dismiss` drops it.
  The three terminal actions resolve the report
  (`dismissed` | `job_rejected` | `user_suspended`) with
  `resolved_at` / `resolved_by` stamped. Resolving an
  already-resolved report is a 200 no-op with no second
  audit row. `reject_job` needs a job on the report and
  `suspend_user` needs a user (both 422 otherwise).
- **The audit trail** — every resolve step is audited in the
  same transaction as the mutation (P1-090 ordering rule:
  resolve first, effects second). Reads, denials (403),
  unknown targets (404), and admitted stub calls (501) are
  **not** audited — the trail records actions, not attempts.
  The P1-092 suite proves the whole matrix live.
- **The ladder** — warn → suspend → terminate (P1-091), one
  endpoint per rung. A suspension done through the queue
  composes with the ladder (the rung is derived from status
  + the `user.warned` count), and terminating a suspended
  user from the queue's aftermath is the ladder's last rung.
- **No console UI yet** — Phase 4 adds the admin console
  (open reports, evidence links, SLA timers). Until then ops
  work the queue over the API (curl / Postman). Support
  reads the queue; only an admin resolves (support warn is
  403, support resolve is 403).

## 2. Triage checklist

Work the queue oldest-first, and never let an item pass its
SLA without a decision. The SLA clock starts at **filing**
(`created_at`), not at pickup — `sla_due_at` is filing +
72 h, and `sla_breached` is a read-time comparison.

1. **List the open queue** (`GET /v1/admin/abuse-reports?status=open&limit=100`)
   and sort by `sla_due_at` — the nearest deadline first.
   Anything with `sla_breached: true` is already late: say
   so in the report notes and resolve it first.
2. **Pick it up** (`start_review`) before doing anything
   else. The pickup stamps `updated_at`, writes the
   `abuse.report_reviewing` row, and tells the next operator
   the report is owned. Do not investigate an `open` report
   without picking it up — two operators working the same
   open report is how a job gets rejected twice.
3. **Read the report**: `reason`, `reporter_contact`,
   `job_id` / `user_id`. If the report names a job, read
   the job (`GET /v1/admin/jobs` — the admin jobs view);
   if it names a user, read the user. A report with
   neither is still triageable — it is a complaint about
   an account, not a job — but `reject_job` on it is 422,
   so do not attempt it.
4. **Preserve the evidence now** — before anything else if
   the clock is short (see §3). The output may have hours
   left, not days.
5. **Check the audit trail for the target** (`GET
   /v1/admin/audit?target=…`): has this user been warned
   or suspended before? Is there a `abuse.spike_detected`
   row (the P1-083 velocity check, off by default — if it
   fired, the report is one of a burst, not a one-off)?
   A repeat offender goes down the ladder; a first
   offence usually stops at `reject_job` or `dismiss`.
6. **Decide, with a reason** (every resolve needs one,
   1–500 chars — it is the audited `reason` on the
   resolution row):
   - **Dismiss** — the report does not describe a
     violation (wrong job, the content is the reporter's
     own, the claim is empty). Say why in `notes`.
   - **Reject the job** — the reported job violates the
     AUP. The job becomes `rejected`; the Phase 2/3
     pipeline must skip `rejected` jobs (no job runs yet,
     so today this is a status flip only).
   - **Suspend the user** — the account is the problem,
     not one job (repeat offences, a spike, a stolen-
     content ring). The suspension revokes sessions and
     holds queued jobs, mails the user, and audits under
     its own name. Terminating from here is the ladder's
     last rung and needs the user `suspended` first.
7. **Resolve once.** A second resolve of the same report
   is a 200 no-op — it changes nothing and writes nothing,
   so a double-click is harmless, but do not rely on it:
   re-read the item (`status: resolved`, `resolved_at` /
   `resolved_by` set) before acting again.
8. **Answer the reporter** when the report came with a
   contact: the `abuse-report-ack` mail went out at
   filing; the outcome notice goes out when the report
   resolves (the ack template is the only mail wired
   today — the outcome notice is a Phase 4 follow-up, so
   until then reply from the reporter's contact in the
   queue item, by hand).

## 3. Evidence preservation — artifacts die on their TTL

Result artifacts expire per plan TTL and are hard-deleted
(object + row) by the daily `detag-gc` artifact sweep
(NFR-40). The TTLs, from the plans seed:

| Plan | Artifact TTL |
| --- | --- |
| Free | 6 h |
| Pro | 24 h |
| Business | 72 h |

The 72 h review SLA is **longer than the Free (6 h) and Pro
(24 h) artifact TTLs and exactly equal to the Business (72 h)
one** — a report filed on day 0 can outlive its own evidence. So:

1. **Check `expires_at` first.** Every artifact row
   carries it (`created + plan TTL`). If the output is a
   Free user's, you have 6 hours from creation, not 72.
2. **Record the storage keys in the report notes
   immediately** (`artifacts.storage_key` for the output,
   mask, previews, and report). The keys are the only
   handle the sweep leaves behind — once the sweep runs,
   the row is gone too.
3. **Copy the objects out of the bucket before
   `expires_at`** using the operator's storage access.
   There is no admin download endpoint yet (the Phase 3
   result endpoint returns a presigned GET within TTL;
   until then the copy is manual). Do not rely on the
   queue item to hold the evidence — it holds ids, not
   bytes.
4. **If the sweep already ran**, the object is gone and
   cannot be recovered. What survives is the durable
   record: the `jobs` row, the `abuse_reports` row, and
   the audit trail (audit rows live 365 d, app logs 90 d).
   Say so in the notes — "artifact expired, evidence is
   the trail" — and resolve on that basis. Do not claim
   to have seen content you did not.
5. **Inputs are never swept** (no `expires_at`; the ILM
   3-day ceiling is their backstop), so an upload the job
   was made from is still there even after the output
   expired — it is evidence of what was submitted, not of
   what was produced.

## 4. DMCA-style takedown response — STUB

> **Status: stub, not legal advice, no legal review.**
> The compliance doc (`docs/02-REQUIREMENTS/05-compliance-aup.md`)
> requires an abuse contact + intake queue + 72 h response
> SLA on the status page. The intake and the SLA exist; the
> response below is a draft for the owner to verify with
> counsel before it is sent to anyone. Do not present it as
> our legal position, and do not act on a takedown notice
> without the owner (or counsel) in the loop.

A takedown notice must contain, at minimum:

- Identification of the copyrighted work (what it is, who
  owns it).
- Identification of the infringing material on Detag — the
  job id or the output link, specific enough to find it.
- The claimant's contact (name, email).
- A good-faith statement (they believe the use is not
  authorized) and an accuracy statement (the information
  is accurate, under penalty of perjury where applicable).

If any of those is missing, do **not** act on the notice:
reply asking for the missing field, and leave the report
`reviewing` with a note saying what was asked for.

Response template (draft — owner/counsel to approve):

> We received your takedown notice on [date] regarding
> [job/output]. We are reviewing it under our abuse
> process and will respond within 72 hours of receipt.
> If the material violates our acceptable-use policy it
> will be removed and the account actioned; if the notice
> is incomplete we will ask you for the missing
> information first.

On a complete notice: triage per §2 (the reported job is
the evidence — preserve it per §3 first), resolve per the
checklist, and tell the claimant the outcome. The
termination of an account over a takedown is a ladder
decision (suspend first, terminate second), not a
single-step delete — and termination does not cancel the
user's subscription (the P1-071 support flow cancels
billing separately).

## 5. Escalation

- **Legal / owner**: any takedown notice, any threat of
  legal action, any report about a minor, any request
  from law enforcement. Do not answer those yourself.
- **Repeat / ring**: a spike of reports against one user
  or one source (check `abuse.spike_detected` rows) —
  suspend the user, do not reject jobs one by one.
- **The queue itself is broken** (intake 5xx, queue
  5xx, SLA timers wrong): file it as an S1/S2 in
  `docs/09-STATE/ISSUES.md` and work the queue by hand
  from the database until it is fixed. The intake has
  no dead-letter path — a failed filing is a lost report.

## Pointers

- Admin guide (endpoints, capabilities, audit-trail
  rules): `docs/06-DELIVERY/07-admin-guide.md`
- API spec (intake + queue + resolve shapes):
  `docs/02-REQUIREMENTS/01-api-spec.md`
- User lifecycle (suspend/restore/terminate, ladder):
  `docs/03-ARCHITECTURE/06-user-lifecycle.md`
- Compliance + AUP (jurisdiction notes, legal-review
  gate): `docs/02-REQUIREMENTS/05-compliance-aup.md`
- Retention (NFR-40): `docs/02-REQUIREMENTS/03-non-functional-requirements.md`
- The matrix suite (what is audited, proven live):
  `apps/api/tests/audit_abuse_suite.rs` (P1-092)
