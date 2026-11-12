# Research Prompt (Phase 6: provenance, C2PA, SynthID)

Phase 6 is research-first and compliance-gated. This prompt governs all research tickets. Honesty is a requirement: the memo must state what is NOT possible, not only what is.

```text
Execute research ticket <P6-xxx> per 07-PHASES/PHASE-6-provenance-roadmap.md.

METHOD
1. Scope: read 02-REQUIREMENTS/05-compliance-aup.md first — the gates and the marketing-honesty rule bind research outputs too.
2. Gather: use only public, citable sources (specs, papers, official docs, source repos). Record every claim with a source URL and access date in the research log (09-STATE/DECISIONS.md "research log" section or docs/research/<topic>.md).
3. Build (only where the phase file allows implementation): C2PA manifest READ/VERIFY/PRESERVE is implementable now via c2pa-rs. Any STRIP capability must remain behind the legal gate (feature flag OFF, no UI surface) until the owner ticks the legal-review gate.
4. Synthesize: produce the deliverable named by the ticket — feasibility memo, detector-availability assessment, go/no-go ADR, or evaluation dataset plan. Structure: question → evidence (with sources) → engineering reality → options → recommendation.
5. Honesty checks:
   - Invisible-watermark marks (SynthID-class): state plainly that there is no public detector and no established reliable removal method; robustness-to-edits is the design goal of such marks. Never propose shipping a "removal" claim for invisible marks.
   - If a capability is speculative, label it RESEARCH / NOT SHIPPABLE in its current state.
   - Any perturbation experiments on marks must be confined to content the project itself generates, must be motivated by verification/robustness understanding, and must never become a "defeat provenance" how-to.
6. Log: append the outcome (one-paragraph) to DECISIONS.md; if the conclusion changes the roadmap, file the follow-up tickets.

OUTPUT: the named deliverable + updated DECISIONS.md entry + sources list. If sources are insufficient, say so and stop — do not pad with speculation.
```

## Standing research questions (from open-questions.md)

- RQ-1: Is a legitimate, owner-scoped verification path available for marks like SynthID (provider APIs)? If yes → design a **verify-and-report** feature (product-positive), not a removal feature.
- RQ-2: What does correct C2PA strip-and-attest look like technically (box rewrite, re-sign absence, validate-absence tests), and what does the consent/audit flow need to look like product-wise?
- RQ-3: Which jurisdictions materially constrain provenance-strip features for our target market? (Owner + counsel decide; agent only compiles the factual summary with sources.)
