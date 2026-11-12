# Compliance, Acceptable Use & Provenance Policy

This document is a build-blocking spec, not decoration. Real products ship watermark-removal capabilities only inside these rails. The agent implements these as tested features.

## 1. Acceptable Use Policy (AUP) — enforced product behavior

**Allowed:** processing content the user **generated themselves** or otherwise owns/has rights to, including removing marks that their own AI tooling applied (badges, captions, visible stamps, metadata).

**Prohibited (contractual + technically detected where possible):**
- Removing watermarks from stock/agency/photographer content (Getty, Shutterstock, Adobe Stock, individual photographer marks). Detection: template pack includes known stock-agency mark signatures; matches route to `needs_review` status instead of processing.
- Content whose purpose is deception (fake evidence, forged documents, impersonation content).
- Illegal content of any kind; sexual content involving minors (zero tolerance, report preserved).
- Circumventing platform labeling rules for third-party content the user does not own.

**Enforcement matrix:** first violation → warning + job rejected; repeated → suspension; severe (illegal content) → termination + record preserved. All enforcement actions audited and visible in admin panel.

## 2. Ownership attestation (FR-C-01)

Every job creation requires an explicit, logged attestation: *"I own or have rights to this content, and any watermark/mark originates from my own generation or licensed material."* Stored: user id, upload id, attestation text version, timestamp, IP-hashed, client (web/API key). No attestation → job rejected with PF-4001. Attestations are auditable and exportable (admin).

## 3. Provenance marks — the honest engineering position

### C2PA (content credentials / JUMBF manifests)
- **Default: PRESERVE.** PureFrame's default is to keep C2PA manifests intact — provenance preservation is a feature, not a bug.
- **Strip capability (Phase 6, gated):** technically feasible (manifest lives in metadata boxes; rewrite removes it). Gating before it may ever ship:
  1. Owner signs the legal-review gate in this file after reviewing jurisdiction exposure.
  2. Feature flag `provenance_strip` OFF by default; enabling is an admin action, audited.
  3. Per-job attestation PLUS a second explicit consent step ("Remove content credentials? This may affect legal use of the file in some jurisdictions").
  4. Full audit trail per strip (who, what file, when).
- **Known jurisdiction exposure (as of kit writing; owner must verify with counsel):** China's AI-content labeling rules (effective Sept 2025) prohibit malicious deletion/tampering of AI-generated content labels; EU AI Act imposes transparency/marking obligations on the AI side and stripping can undermine compliance chains; platform ToS (e.g., tools that require kept labels) may be violated. Exposure is jurisdiction- and use-specific; the gate exists so a human decides, not the agent.
- Implementation notes for the gated path live in `07-PHASES/PHASE-6-provenance-roadmap.md` (c2pa-rs read/verify first, strip behind the gate, verify-absence tests).

### SynthID (and similar invisible/latent watermarks)
- **Reality check (must be stated in any user-facing doc):** SynthID-class marks are embedded in the generative model's output distribution and are designed to survive common edits (crop, re-encode, mild perturbation). There is **no public detector** for SynthID, and no reliable, verifiable removal method is publicly established. Any vendor claiming guaranteed removal of invisible marks should be treated skeptically.
- **Product stance:** research-only (Phase 6 E6.2). The agent must NOT claim removal capability for invisible marks in any UI/marketing/docs. If, in the future, a detector API becomes legitimately available (e.g., provider offers verification endpoints to content owners), the correct feature is **verify-and-report**, and any strip-related research is confined to content the user provably generated, is ethics-reviewed, and is compliance-gated like C2PA strip.
- Deliverable of the research phase: a feasibility memo with sources, a go/no-go ADR, and honest product copy. That is the entire scope until the owner decides otherwise.

## 4. Data protection commitments

- Privacy policy + ToS pages ship in Phase 4 with text derived from this file (owner reviews before launch).
- Sub-processors list: payment provider (Stripe/Razorpay), email (SMTP provider chosen in config), storage/cloud (owner's infra). No other third-party data sharing.
- GDPR: export endpoint, delete cascade, retention per `03-non-functional-requirements.md` NFR-40..43.
- DMCA-style takedown: abuse contact + intake queue + response SLA 72h documented on the status page.

## 5. Legal-review gate (owner action, recorded in DECISIONS.md)

Before enabling `provenance_strip`:
- [ ] Owner has read this file and jurisdiction notes.
- [ ] Owner confirms target market and legal sign-off (or accepts risk in writing here).
- [ ] AUP + consent flow texts reviewed.
- [ ] Audit/attestation implementation verified by tests (Phase 6 exit gate).
Until all boxes are ticked by the owner in `09-STATE/DECISIONS.md`, the flag stays OFF and Phase 6 E6.1 tickets stop at the preserve/verify stage.

## 6. Marketing honesty rule

No UI copy, landing page, changelog, or report may claim: "removes invisible watermarks", "undetectable", "defeats SynthID", or similar. Copy says: "Removes visible marks from AI media you own, with a verifiable no-quality-loss report." This is a tested requirement: Phase 5 includes a copy-accuracy check ticket.
