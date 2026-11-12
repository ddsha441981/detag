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

## owner-input-needed queue

| # | Date | Question | Temporary default in use | Owner answer |
| --- | --- | --- | --- | --- |
| 1 | kit start | Product name / brand | PureFrame (working title) | pending |
| 2 | kit start | Final pricing (INR/USD) | Free ₹0 / Pro ₹499 / Business ₹1,999 | pending |
| 3 | kit start | Live payment keys (Stripe/Razorpay) | TEST MODE only | pending |
| 4 | kit start | Domain + email sender | localhost + MailHog dev defaults | pending |
| 5 | kit start | Legal-review gate: provenance_strip (C2PA) | flag OFF; preserve-only shipped | pending |
| 6 | kit start | SynthID scope beyond research | research memo only | pending |

## Research log (Phase 6)

(appended by agent; each entry: date, question, sources with URLs + access dates, finding, recommendation)
