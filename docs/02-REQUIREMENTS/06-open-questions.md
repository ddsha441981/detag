# Open Questions & Standing Defaults

Requirement-gathering closure: every open question has a **standing default** so the agent never blocks. Owner overrides by editing the Default column (or writing to `09-STATE/DECISIONS.md`); the agent picks overrides up on next run. Agent may append rows; only the owner may change rows marked OWNER.

| # | Question | Standing default (agent proceeds with this) | Owner input? |
| --- | --- | --- | --- |
| 1 | Product name / brand | PureFrame (working title) | OWNER |
| 2 | Final pricing | Free ₹0 / Pro ₹499-mo / Business ₹1,999-mo (USD mirror 0/19/79), changeable in config + Stripe dashboard | OWNER |
| 3 | Payment provider order | Stripe primary; Razorpay adapter behind flag (India-friendly) | OWNER (live keys) |
| 4 | Domain, email sender | `localhost` defaults; transactional email via SMTP env (MailHog in dev) | OWNER |
| 5 | GPU support at launch | No — CPU-first; GPU behind `PF_ENGINE_GPU=1` flag | OWNER |
| 6 | Video codec matrix v1 | H.264 in/out (MP4/MKV); VP9 + HEVC decode-only with H.264 output (ADR if output copy needed) | Agent may ADR |
| 7 | EXIF output policy | Strip all EXIF by default (privacy); apply orientation; keep ICC. Configurable per job | Agent |
| 8 | C2PA default | Preserve manifests; strip gated per `05-compliance-aup.md` | OWNER (gate) |
| 9 | SynthID scope | Research + honest memo only; no removal claims | OWNER (gate) |
| 10 | Result TTL | 6h/24h/72h by plan | OWNER may change |
| 11 | Impersonation feature | Ship (support role, audited, banner) — big ops win, standard SaaS practice | OWNER may veto |
| 12 | Self-host vs cloud | docker-compose single VPS first; scale path documented; no k8s in v1 | Agent |
| 13 | Frontend stack | Next.js 15 + TS + Tailwind + shadcn/ui, upload-centric console | Agent may ADR |
| 14 | Analytics | Self-hosted, cookie-less, no PII event set | Agent |
| 15 | Language of UI copy | English v1; i18n keys structured for hi additions later | OWNER later |
| 16 | Model assets licensing | Only permissively licensed, redistributable models (license audit ticket in Phase 2); no scraped template assets — templates synthesized in-house | Agent must obey |
| 17 | Rate limits | Per plan defaults in config; tuned in Phase 5 | Agent tunes |
| 18 | Data residency | Single region (owner's VPS); documented in privacy page | OWNER |

## Assumption ledger (agent-maintained)

The agent appends here when it builds against an assumption: date, ticket, assumption, chosen default, blast radius. Reviewed at phase boundaries; wrong assumptions become tickets.

| Date | Ticket | Assumption | Default chosen |
| --- | --- | --- | --- |
| (starter row — replace on first use) | P0-001 | none yet | — |
