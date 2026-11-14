# Email ops (P1-032)

How Detag sends mail in v1: what ships, how the sender is
configured, what to do before the first real mail leaves, and
how to tell what happened to a mail afterwards.

## Stance: transactional-only in v1

Detag sends account mail only. There is no newsletter, no
marketing mail, and no marketing template ships — the drain's
marketing cap is 0 (`PF_MAILER_MARKETING_CAP_PER_HOUR`), so
any marketing-class row is parked on sight until the operator
deliberately raises the cap (P1-031, ADR-0023). Do not add a
marketing template without an owner decision: it changes the
deliverability posture (bulk-sender rules, unsubscribe handling,
a separate sending subdomain) and the abuse surface.

The 8 templates the api actually enqueues (`crates/mailer`,
`TEMPLATES` — subjects + text + HTML, snapshot-tested, P1-029):

| Template | Sent when | Class / hourly cap per recipient |
| --- | --- | --- |
| `verify-email` | register, resend-verification | verify / 3 |
| `email-change-verify` | email-change intent, to the NEW address | verify / 3 (shares the budget with `verify-email`) |
| `password-reset` | forgot-password | reset / 3 |
| `email-change-alert` | email-change intent, to the OLD address | transactional / never capped |
| `lockout-notice` | account lockout (one per episode) | transactional / never capped |
| `new-login-digest` | login from a new device/IP (incl. after a password reset) | transactional / never capped |
| `refresh-reuse-alert` | refresh-token reuse detected | transactional / never capped |
| `account-deletion` | account-deletion request | transactional / never capped |

Caps are enforced in the drain, the single choke point every
queued mail passes through (P1-031): the drain counts sent
outbox rows for the recipient hash + class templates in the
last hour, parks the 4th mail (dropped, not retried — its
token would be stale) and writes an abuse audit row
(`email.rate_limit_tripped`, hash only, never a raw address,
NFR-42). The endpoint-level limits stay as the first line
(resend-verification 3/hour per user+template, P1-002;
forgot-password 10 burst / 1 per min per account, P1-025).

Copy rule (P1-029): transactional copy makes no product
claims — no "invisible", "undetectable", "SynthID" anywhere
in templates or snapshots. A new template must pass the same
honesty grep before it ships.

## Sender config

| Var | Dev default (`.env.example`) | Prod |
| --- | --- | --- |
| `PF_SMTP_HOST` / `PF_SMTP_PORT` | `localhost` / `1025` (MailHog) | the production relay's host/port |
| `PF_SMTP_USER` / `PF_SMTP_PASS` | empty (no credentials) | the relay's credentials; set only when the relay needs them — the transport sends credentials only when `PF_SMTP_USER` is set (P1-028) |
| `PF_SMTP_TLS` | `false` | `true` for a production relay (STARTTLS) |
| `PF_MAIL_FROM` | `noreply@detag.local` | a real sender on the sending domain (must match the SPF/DKIM domain below, or receiving servers will reject or spam-folder the mail) |
| `PF_PUBLIC_BASE_URL` | `http://localhost:8080` | the public base URL — every token link in every mail is built from it, so a wrong value here breaks all verification/reset links |
| `PF_MAILER_MAX_ATTEMPTS` | `5` | attempts per row before it stops being claimed (terminal state `attempts >= max`, `last_error` kept) |
| `PF_MAILER_BACKOFF_BASE_SECS` | `60` | retry backoff `base * 2^(n-1)` s, capped at 3600 s |
| `PF_MAILER_DRAIN_INTERVAL_SECS` | `30` | worker drain wake interval |
| `PF_MAILER_DRAIN_BATCH` | `25` | max rows one drain pass claims |
| `PF_MAILER_VERIFY_CAP_PER_HOUR` | `3` | see the caps table above |
| `PF_MAILER_RESET_CAP_PER_HOUR` | `3` | see the caps table above |
| `PF_MAILER_MARKETING_CAP_PER_HOUR` | `0` | see the stance above |

Boot behaviour (P1-028): a bad `PF_MAIL_FROM` fails boot —
the api refuses to start rather than send from an address it
cannot parse. The drain runs inside the worker (`apps/worker`,
one pass at startup, then per interval); if the worker is
down, mail queues in `email_outbox` and sends when it returns
— nothing is lost, only delayed.

## Deliverability checklist (do before the first real mail)

Status: **placeholders only**. The domain and the sender are
owner decision #4 in `docs/09-STATE/DECISIONS.md` (pending):
until the owner picks the domain and the relay, no record
below can be filled in with a real value, and no real mail
should be sent. The shapes are recorded here so the deploy
runbook's pre-launch step is a fill-in, not a redesign.

- [ ] **Domain chosen** (owner decision #4) and `PF_DOMAIN` /
  `PF_PUBLIC_BASE_URL` / `PF_MAIL_FROM` set to it everywhere.
  The dev values (`detag.local`, `localhost:8080`) must never
  reach prod: `detag.local` does not resolve in public DNS, so
  SPF/DKIM cannot validate for it and most receivers will
  reject the mail outright.
- [ ] **SPF** — a TXT record on the sending domain authorising
  the relay:
  `v=spf1 include:<relay SPF include> -all`
  (replace `<relay SPF include>` with the relay's published
  include; `-all`, not `~all`, once sending is confirmed).
- [ ] **DKIM** — the relay signs with the domain's key; publish
  the relay's DKIM TXT record at the selector it gives
  (`<selector>._domainkey.<domain>`). Verify a real mail's
  `DKIM-Signature` validates before launch.
- [ ] **DMARC** — a TXT record at `_dmarc.<domain>`,
  starting at `v=DMARC1; p=none; rua=mailto:<dmarc reports>`,
  then `p=quarantine` and finally `p=reject` once the reports
  show SPF+DKIM passing. Do not start at `p=reject`: a
  misconfigured SPF/DKIM would silently drop every
  verification mail, and users could not register at all.
- [ ] **Sending subdomain** (recommended, not required in v1):
  send from `mail.<domain>` / `noreply@mail.<domain>` so the
  domain's main SPF/DKIM stays untouched and a deliverability
  problem with transactional mail cannot stain the root
  domain's reputation.
- [ ] **Reverse DNS / PTR** — the relay's sending IP resolves
  back to the sending hostname (the relay provider usually
  owns this; confirm it, do not assume it).
- [ ] **Link domain matches** — token links use
  `PF_PUBLIC_BASE_URL`; the link host must be the domain users
  expect, served over HTTPS in prod (an `http://` link host in
  a password-reset mail is a phishing tell).
- [ ] **Seed + warm-up** — send the 8 templates to a seed
  inbox (Gmail + Outlook at minimum), confirm inbox placement
  (not spam), and check the token links end-to-end before
  opening registration. No bulk warm-up is needed: v1 sends
  transactional mail only, at human scale.
- [ ] **Abuse monitoring** — the `email.rate_limit_tripped`
  audit rows (see below) are the tripwire: a sudden run of
  them means someone is hammering registration/forgot against
  one address, and the per-account buckets + lockout are the
  response, not a mail change.

## Test addresses

- **Dev:** any address works — mail sent by the api lands in
  MailHog, never in a real inbox (SMTP :1025, UI :8025,
  `wget --spider :8025/api/v2/messages` is the dev health
  check in the runbook). Use obviously-fake addresses so a
  dev mail can never be mistaken for a real one.
- **Tests:** the suites use `@example.com` addresses (RFC
  2606 — reserved, never deliverable) and a fake SMTP relay on
  `127.0.0.1` (the same minimal protocol subset in the mailer,
  worker and api suites). No real SMTP in CI, ever: the
  `apps/api/tests/email_flow.rs` e2e tests (P1-030) register
  and reset against the fake relay and read the token out of
  the DELIVERED mail. Test addresses in the suite follow the
  `<name>@example.com` pattern (`fran@`, `carol@`, `erin@`, …).
- **Never use** a real user's address in a test, a seed, or a
  manual check — the outbox stores only the sha256 of the
  recipient (`to_hash`), and the payload carries no raw address
  (NFR-42); keep it that way.

## Ops: what happened to a mail

All state lives in `email_outbox` (`to_hash`, `template`,
`payload`, `scheduled_at`, `sent_at`, `attempts`,
`last_error`) plus `audit_logs`. Useful queries (psql against
the `pf` database; `to_hash` is a sha256 hex — compute it
from the address, never store the address in the query log):

```sql
-- What is queued / stuck for one recipient (hash only).
SELECT id, template, scheduled_at, sent_at, attempts, last_error
FROM email_outbox WHERE to_hash = '<sha256 hex>'
ORDER BY created_at DESC LIMIT 20;

-- Parked rows: exhausted retries or rate-limit trips.
SELECT id, template, to_hash, attempts, last_error
FROM email_outbox WHERE attempts >= 5 AND sent_at IS NULL
ORDER BY created_at DESC LIMIT 20;

-- Rate-limit trips (abuse audit): who, which class, how many.
SELECT created_at, after->>'to_hash' AS to_hash,
       after->>'template' AS template, after->>'class' AS class,
       after->>'cap_per_hour' AS cap, after->>'sent_in_window' AS sent
FROM audit_logs WHERE action = 'email.rate_limit_tripped'
ORDER BY created_at DESC LIMIT 20;
```

Reading the result:

- `sent_at` set → the mail left the relay. If the user says
  they never got it, the problem is downstream (spam folder,
  relay blocklist, wrong `PF_MAIL_FROM` domain) — check the
  relay's logs, not the outbox.
- `sent_at` NULL, `attempts` climbing, `last_error` set → a
  transient relay failure; the drain retries with backoff
  (`base * 2^(n-1)` s, cap 3600 s). If `last_error` names the
  relay (connection refused, 5xx), fix the relay, not the
  code — the rows will drain on their own.
- `sent_at` NULL, `attempts >= 5` → parked, never claimed
  again. `last_error` says why: an unknown template, an
  unresolvable recipient, a `to_hash` mismatch — or `email
  rate limit: …` for a cap trip. A parked verification/reset
  mail is dropped by design (its token would be stale); the
  user requests a fresh mail instead of anyone re-sending the
  parked one.
- A row with `scheduled_at` in the future is a retry waiting
  out its backoff, not a stuck row.

## Troubleshooting

- **"The user never got the verification mail"** — (1) check
  the outbox row for the recipient hash: `sent_at` set means
  it left us; (2) check MailHog in dev (`:8025`) — if it is
  there, the api side is fine; (3) check the rate-limit trips
  — a 4th verification mail in an hour is parked, not sent,
  and the user must wait out the hour (or an admin must not
  hand-send a link: the token in the parked mail is the live
  one only until a newer mint replaces it); (4) check the
  `PF_MAIL_FROM` domain and the SPF/DKIM/DMARC checklist above.
- **"The link in the mail does not work"** — (1) confirm
  `PF_PUBLIC_BASE_URL` is the public URL, not `localhost`;
  (2) note the lettre transport sends both parts
  quoted-printable (P1-030): the `=` in the link's query
  string arrives as `=3D` on the wire, which is correct —
  a mail client decodes it; anyone reading the raw message
  must QP-decode first, or the token reads 2 chars shifted
  and the endpoint answers 401 `PF-1010`.
- **"Register / forgot answered 204 but no mail arrived"** —
  by design, for unknown emails, already-verified users and
  over-limit resends (no enumeration, P1-002/P1-025): the
  endpoint answers 204 with no mail and audits only when a
  mail is actually queued. Do not "fix" this by sending a
  mail — it would leak which addresses have accounts.
- **"The worker is not draining"** — the drain lives in the
  worker process; check the worker is up and its logs for the
  drain pass lines. A claim error is logged and retried next
  interval (P1-028); a bad `PF_MAIL_FROM` fails boot loudly
  instead of draining silently.
- **"A template renders wrong"** — the snapshots are the
  contract: `just snapshots-check` (P1-029) fails on any
  drift. Edit the `.tera` file, update the snapshot
  deliberately (`INSTA_UPDATE=always`), and re-run the
  honesty grep — no "invisible"/"undetectable"/"SynthID"
  anywhere in the copy.
