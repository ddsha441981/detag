# Email matrix (P1-105)

Every mail Detag sends, in one table: what triggers it, which
template it renders, what caps it, and who receives it. The
companion to `05-email-ops.md` (which owns the sender config,
the deliverability checklist, and the troubleshooting playbook)
— this file answers "which mail, why, how often, to whom".

Sources of truth, in order: `crates/mailer/src/rate_limit.rs`
(`template_class`, `templates_for_class`, `RateLimitConfig`),
`crates/mailer/src/templates.rs` (`TEMPLATES`, 21 entries —
pinned by the P1-104 tripwire test
`all_templates_have_committed_snapshots`), the enqueue call
sites in `apps/api/src/services/*` (one constant per template),
and the drain in `crates/mailer/src/drain.rs`. Where this file
and the code disagree, the code wins and this file is stale —
file a docs fix, do not "fix" the code to match the doc.

## The matrix

Cap is per recipient hash + class, counted over the last
hour in the drain (P1-031). "Shared" means the templates in
one class draw from one budget: the 4th mail in the hour is
parked (dropped, not retried) and audited as
`email.rate_limit_tripped`. Transactional is uncapped in the
drain — its limits, where they exist, are the dedup rules in
the Trigger column.

| Trigger | Template | Cap | Audience |
| --- | --- | --- | --- |
| `POST /v1/auth/register`, or `POST /v1/auth/resend-verification` | `verify-email` | Verify, 3/hr, shared with `email-change-verify` | the registering user (the address on the request) |
| `PATCH /v1/me` email change (intent created) | `email-change-verify` | Verify, 3/hr, shared with `verify-email` | the NEW address (not yet the account address) |
| `POST /v1/auth/forgot-password` | `password-reset` | Reset, 3/hr | the account holder |
| `PATCH /v1/me` email change (intent created) | `email-change-alert` | Transactional, uncapped | the OLD address (the account holder's current address) |
| Account lockout (failed-login threshold trips) | `lockout-notice` | Transactional, uncapped; one per lockout episode | the account holder |
| Login from a new device/IP (incl. after a password reset) | `new-login-digest` | Transactional, uncapped | the account holder |
| Refresh-token reuse detected (family revoked) | `refresh-reuse-alert` | Transactional, uncapped | the session owner |
| Account-deletion request | `account-deletion` | Transactional, uncapped | the address on record |
| Subscription moves `trialing → active` | `subscription-welcome` | Transactional, uncapped | the subscriber |
| Subscription moves to `past_due` (from `trialing` or `active`) | `subscription-dunning` | Transactional, uncapped | the subscriber |
| Dunning job, 3 days past due (`past_due_since`) | `subscription-dunning-day3` | Transactional, uncapped; exactly once per step (guarded step bump) | the subscriber |
| Dunning job, 7 days past due (`past_due_since`) | `subscription-dunning-day7` | Transactional, uncapped; exactly once per step (guarded step bump) | the subscriber |
| Subscription moves to `canceled` (from `active` or `past_due`) | `subscription-canceled` | Transactional, uncapped | the subscriber |
| Daily usage crosses 80% of the plan quota | `quota-warning-80` | Transactional, uncapped; once per threshold per day | the account holder |
| Daily usage crosses 100% of the plan quota | `quota-warning-100` | Transactional, uncapped; once per threshold per day | the account holder |
| Abuse report filed | `abuse-report-ack` | Transactional, uncapped | the reporter contact (may not be a user — the reporter is anonymous) |
| Enforcement ladder: admin warns a user | `enforcement-warning` | Transactional, uncapped; only on a real transition (a no-op warn mails nothing) | the enforced user |
| Enforcement ladder: admin suspends a user | `enforcement-suspension` | Transactional, uncapped; only on a real transition | the enforced user |
| Enforcement ladder: admin terminates a user | `enforcement-termination` | Transactional, uncapped; only on a real transition (terminating an already-terminated account is a 200 no-op, no mail) | the enforced user |
| Email verified at register (`verify_email` consumes the token) | `welcome` | Transactional, uncapped; exactly once per account (same transaction as token consume) | the newly verified user |
| Weekly digest job (once per user per week) | `weekly-digest` | Transactional, uncapped; flag `PF_FEATURE_WEEKLY_DIGEST` OFF default + `weekly_digest` opt-out; once per user per week (partial unique index dedup) | the opted-in user |

## Not in the matrix

- `marketing-newsletter` is a class in `rate_limit.rs`
  (`MailClass::Marketing`) but has NO template files, NO
  sender, and NO row above: the marketing cap
  (`PF_MAILER_MARKETING_CAP_PER_HOUR`) is 0, so any
  marketing-class row is parked on sight. Adding a marketing
  template is an owner decision (see `05-email-ops.md`,
  "Stance: transactional-only in v1") — it needs a row here,
  a snapshot, and a class decision first.

## Ops notes

- **The drain is the choke point.** Every queued mail passes
  through `send_one` in the drain: the class cap is checked
  there, per recipient hash + class over the last hour
  (`PF_MAILER_VERIFY_CAP_PER_HOUR` / `PF_MAILER_RESET_CAP_PER_HOUR`
  / `PF_MAILER_MARKETING_CAP_PER_HOUR`, defaults 3 / 3 / 0).
  A capped mail is parked, not retried — its token would be
  stale — and the trip is audited (`email.rate_limit_tripped`,
  hash only, never a raw address, NFR-42).
- **Endpoint limits are the first line, not the drain.**
  Resend-verification is 3/hour per user+template (P1-002);
  forgot-password is 10 burst / 1 per min per account
  (P1-025). A 204 with no mail is by design for unknown
  emails, already-verified users, and over-limit resends (no
  enumeration) — do not "fix" it by sending a mail.
- **Preferences gate two mails only.** `weekly_digest`
  opts a user out of the digest (checked at enqueue by the
  sender); `marketing` opts a user out of marketing mail
  (gated at the last mile in the drain). Transactional,
  verify, and reset mail never consult prefs — an opt-out
  can never silence a password reset or a quota warning
  (P1-103, ADR-0063 / ASSUMPTION-064).
- **Unsubscribe header: marketing only.** Marketing-class
  rows carry `List-Unsubscribe:
  <{base_url}/settings/email-preferences>`; transactional
  mail never carries it. There is no one-click
  `List-Unsubscribe-Post`: one-click needs a single-use
  unsubscribe token + a consume endpoint, a follow-up feat
  (P1-104 proves the existing verify/reset/change tokens
  single-use but builds no unsubscribe token).
- **Exactly-once rules live with the sender, not the drain.**
  The digest dedups on (`template`, `user_id`,
  `week_start`) via a partial unique index (one per week);
  quota warnings fire once per threshold per day; the
  lockout notice fires once per episode; enforcement mails
  fire only on a real ladder transition; `welcome` fires
  exactly once because it enqueues in the same transaction
  that consumes the verify token.
- **Two triggers are not live yet (honesty).** The quota
  warnings have no production caller: `maybe_warn_quota` is
  called only by tests, and the Phase 2 intake calls it after
  each successful `record` / `true_up` (P1-085). The
  dunning day-3 / day-7 reminders exist as a service
  (`run_dunning`, P1-070) but the nightly scheduler (P1-082)
  enqueues `reconciliation` only — dunning stays unenqueued
  until its handler lands. The day-0 dunning mail DOES fire:
  it is sent on the move to `past_due` in the webhook path.
- **Subscription mails are proven via the state machine, not
  live Stripe.** The transition→template mapping is pinned
  by unit test (`mail_mapping` in
  `subscription_state.rs`); the Stripe HTTP legs are
  untested live (no test-mode key on this VM, P1-070).
- **Adding a template?** Add a row here, a snapshot
  (the P1-104 tripwire fails the build until the count and
  the `.snap` set match), a class decision in `rate_limit.rs`,
  and an honesty grep (no "invisible" / "undetectable" /
  "SynthID" anywhere in the copy, P1-029). A marketing
  template additionally needs the owner decision above.

## How to use this file on call

- "The user never got the mail" → find the row, check the
  cap column first (a 4th verify/reset in the hour is
  parked by design), then the outbox row for the recipient
  hash (`05-email-ops.md`, "Ops: what happened to a mail").
- "The user got a mail they should not have" → check the
  trigger column (was the event real?) and the prefs note
  (only the digest and marketing are opt-outable; a
  transactional mail to an opted-out user is correct).
- "Which mail fires when X happens" → the Trigger column is
  the index; the template name is the outbox `template`
  value to query for.
