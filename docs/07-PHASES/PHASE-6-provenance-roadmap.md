# PHASE 6 — Provenance & Roadmap (gated research)

> **Tickets: 43** · Types: feat 10 · chore 10 · test 2 · docs 7 · spike 14

**Goal:** Provenance marks handled honestly and safely: C2PA read/verify/preserve shipped, strip capability implemented but locked behind the legal gate (flag OFF); SynthID is research-only with an honest feasibility memo; advanced roadmap for post-launch. This phase has 43 research/gated tickets by design - it is not a build-all phase.

**Context (read before starting):**
- 08-PROMPTS/04-RESEARCH.md governs this phase; honesty is a requirement (no removal claims for invisible marks, anywhere).
- 02-REQUIREMENTS/05-compliance-aup.md is law: preserve-by-default; strip needs owner legal sign-off recorded in 09-STATE/DECISIONS.md.
- If the legal gate is not ticked, E6.1 stops after the preserve/verify tickets and E6.2 research completes; nothing else in this phase blocks v1.0.

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] C2PA preserve verified byte-exact on all fixtures (roundtrip leaves manifest untouched).
- [ ] Strip path (if implemented) exists only behind flag OFF with validate-absence tests; audit events proven.
- [ ] SynthID feasibility memo published with sources; go/no-go ADR recorded; no removal claims anywhere in product copy.
- [ ] Roadmap doc + pricing research delivered for owner decisions.

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E6.1 C2PA read & policy | provenance/c2pa | 12 |
| E6.2 SynthID research | provenance/synthid | 8 |
| E6.3 Advanced roadmap | roadmap | 10 |
| E6.4 Post-launch iterate | iterate | 8 |
| E6.5 Research infra | research | 5 |

---

## E6.1 — C2PA read & policy (area: provenance/c2pa)

- [ ] **P6-001** — c2pa-rs spike
  - **Type:** spike · **Area:** provenance/c2pa · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Eval c2pa-rs (license, API stability): read manifests from fixtures (embed sample manifests for tests); deliverable: adopt/not ADR + notes.
  - **AC:**
    - [ ] ADR with findings

- [ ] **P6-002** — Manifest reader service
  - **Type:** feat · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-001 · **Prio:** P1
  - **Spec:** Report C2PA presence + claim summary (no byte-level edits) on upload/metadata_only jobs; store in uploads.metadata.
  - **AC:**
    - [ ] presence reported correctly

- [ ] **P6-003** — Preserve-by-default verify
  - **Type:** feat · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-002 · **Prio:** P1
  - **Spec:** Roundtrip policy: pipeline leaves JUMBF untouched (byte compare of manifest region on fixtures); tests encode this as law (default).
  - **AC:**
    - [ ] byte-exact preserve on fixtures

- [ ] **P6-004** — Policy engine hook
  - **Type:** feat · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-003 · **Prio:** P1
  - **Spec:** provenance param resolution: preserve (default) | strip(gated); policy checks flag + legal gate + attestation v2 + second consent; deny default documented (PF-4101).
  - **AC:**
    - [ ] deny-by-default tested

- [ ] **P6-005** — Strip feasibility spike
  - **Type:** spike · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-004 · **Prio:** P1
  - **Spec:** Timeboxed written analysis: what removal requires technically (box rewrite paths per container); no production strip path leaves the flag.
  - **AC:**
    - [ ] analysis committed

- [ ] **P6-006** — Strip implementation (gated, OFF)
  - **Type:** feat · **Area:** provenance/c2pa · **Est:** S · **Deps:** P6-005 · **Prio:** P1
  - **Spec:** Behind PF_FEATURE_PROVENANCE_STRIP + admin enable: metadata-only rewrite removing manifest; validate-absence tests; UI consent step required; code exists, flag OFF.
  - **AC:**
    - [ ] flag OFF verified
    - [ ] code paths tested

- [ ] **P6-007** — Strip verify-absence tests
  - **Type:** test · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-006 · **Prio:** P1
  - **Spec:** C2PA validation reports absent after strip; other streams untouched (bit compare); deterministic.
  - **AC:**
    - [ ] absence proven

- [ ] **P6-008** — Audit events for provenance
  - **Type:** feat · **Area:** provenance/c2pa · **Est:** S · **Deps:** P6-007 · **Prio:** P1
  - **Spec:** Every preserve/strip decision audited (job, user, policy inputs); admin view field.
  - **AC:**
    - [ ] audit rows complete

- [ ] **P6-009** — Legal gate package
  - **Type:** docs · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-008 · **Prio:** P1
  - **Spec:** One doc for the owner: jurisdiction summary (with sources), consent/attestation texts, enable checklist; ties to compliance doc gate.
  - **AC:**
    - [ ] package ready for owner

- [ ] **P6-010** — AUP update
  - **Type:** docs · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-009 · **Prio:** P1
  - **Spec:** Provenance section finalized (preserve default, gated strip, SynthID stance); version bump flow.
  - **AC:**
    - [ ] AUP versioned

- [ ] **P6-011** — Admin enable flow
  - **Type:** feat · **Area:** provenance/c2pa · **Est:** S · **Deps:** P6-010 · **Prio:** P1
  - **Spec:** Enable requires owner tick in DECISIONS + flag + per-job consent verified; disable path tested.
  - **AC:**
    - [ ] enable/disable audited

- [ ] **P6-012** — C2PA suite
  - **Type:** test · **Area:** provenance/c2pa · **Est:** M · **Deps:** P6-011 · **Prio:** P1
  - **Spec:** Read/preserve/strip(gated)/absence matrix across fixtures (jpeg/mp4/png/webm where applicable).
  - **AC:**
    - [ ] matrix green

---

## E6.2 — SynthID research (area: provenance/synthid)

- [ ] **P6-013** — Literature + docs memo
  - **Type:** spike · **Area:** provenance/synthid · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Public info: how model-embedded invisible marks work, robustness claims, what is publicly verifiable; sources with URLs + access dates; honest memo docs/research/synthid.md.
  - **AC:**
    - [ ] every claim sourced

- [ ] **P6-014** — Detector availability assessment
  - **Type:** spike · **Area:** provenance/synthid · **Est:** M · **Deps:** P6-013 · **Prio:** P1
  - **Spec:** Is there any legitimate, owner-scoped detection path (provider APIs, partnerships)? Findings + go/no-go for a verify-and-report feature.
  - **AC:**
    - [ ] verdict recorded

- [ ] **P6-015** — Robustness study plan
  - **Type:** spike · **Area:** provenance/synthid · **Est:** M · **Deps:** P6-014 · **Prio:** P1
  - **Spec:** If detector access existed: design of a robustness/verification study (owned content only, ethics constraints); no execution without owner gate.
  - **AC:**
    - [ ] plan committed

- [ ] **P6-016** — Perturbation reality check
  - **Type:** spike · **Area:** provenance/synthid · **Est:** M · **Deps:** P6-015 · **Prio:** P1
  - **Spec:** Honest engineering analysis: why naive edit pipelines do not reliably defeat model-embedded marks; documented as anti-claim evidence.
  - **AC:**
    - [ ] analysis honest + sourced

- [ ] **P6-017** — Feasibility verdict ADR
  - **Type:** docs · **Area:** provenance/synthid · **Est:** M · **Deps:** P6-016 · **Prio:** P1
  - **Spec:** Recommendation (expected): do not offer invisible-mark removal; define what we DO offer (visible removal + provenance preservation/verify).
  - **AC:**
    - [ ] ADR recorded

- [ ] **P6-018** — Verify-and-report design
  - **Type:** feat · **Area:** provenance/synthid · **Est:** S · **Deps:** P6-017 · **Prio:** P1
  - **Spec:** Design only: UX + API for a check-provenance feature using a legit detector path if it lands; no implementation without gate.
  - **AC:**
    - [ ] design doc committed

- [ ] **P6-019** — Ethical review section
  - **Type:** docs · **Area:** provenance/synthid · **Est:** M · **Deps:** P6-018 · **Prio:** P1
  - **Spec:** Misuse analysis (deception/misinfo), mitigations (attestation, AUP, audit), residual risks stated plainly.
  - **AC:**
    - [ ] section committed

- [ ] **P6-020** — Go/no-go artifact
  - **Type:** chore · **Area:** provenance/synthid · **Est:** M · **Deps:** P6-019 · **Prio:** P1
  - **Spec:** DECISIONS.md entry: SynthID scope frozen for next release; owner decision requested.
  - **AC:**
    - [ ] owner asked

---

## E6.3 — Advanced roadmap (area: roadmap)

- [ ] **P6-021** — Detection-only API product
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Spec: mask/detection-only API tier (privacy-safe reuse for researchers); pricing + limits draft.
  - **AC:**
    - [ ] spec committed

- [ ] **P6-022** — Watermark ADDITION service
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** P6-021 · **Prio:** P1
  - **Spec:** Flip-side spec: C2PA signing on export (sign manifest, key mgmt, KMS note) - real differentiator; ADR-level plan.
  - **AC:**
    - [ ] plan committed

- [ ] **P6-023** — Batch API GA
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** P6-022 · **Prio:** P1
  - **Spec:** Graduate batch from Business-only: quotas, pricing, examples.
  - **AC:**
    - [ ] GA plan committed

- [ ] **P6-024** — Desktop CLI product
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** P6-023 · **Prio:** P1
  - **Spec:** pf-engine CLI as product: license model, update channel (models), support scope.
  - **AC:**
    - [ ] spec committed

- [ ] **P6-025** — CMS plugins
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** P6-024 · **Prio:** P1
  - **Spec:** WordPress/Shopify plugin specs (auth via API keys, upload hooks); prioritization.
  - **AC:**
    - [ ] specs committed

- [ ] **P6-026** — Enterprise self-host
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** P6-025 · **Prio:** P1
  - **Spec:** SSO (OIDC), audit export, air-gapped model bundles: requirements draft.
  - **AC:**
    - [ ] requirements committed

- [ ] **P6-027** — GPU autoscale
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** P6-026 · **Prio:** P1
  - **Spec:** Design: warm pool + autoscaled worker fleet; cost model.
  - **AC:**
    - [ ] design committed

- [ ] **P6-028** — Partner/affiliate plan
  - **Type:** spike · **Area:** roadmap · **Est:** M · **Deps:** P6-027 · **Prio:** P1
  - **Spec:** Referral + partner API keys; abuse controls.
  - **AC:**
    - [ ] plan committed

- [ ] **P6-029** — Roadmap doc
  - **Type:** docs · **Area:** roadmap · **Est:** M · **Deps:** P6-028 · **Prio:** P1
  - **Spec:** Consolidated roadmap with effort/impact table; owner decisions requested per item.
  - **AC:**
    - [ ] table complete

- [ ] **P6-030** — Pricing tier research
  - **Type:** chore · **Area:** roadmap · **Est:** M · **Deps:** P6-029 · **Prio:** P1
  - **Spec:** Market scan (public pricing pages) + our cost model to tier recommendations (owner decides).
  - **AC:**
    - [ ] recommendations committed

---

## E6.4 — Post-launch iterate (area: iterate)

- [ ] **P6-031** — KPI review ritual
  - **Type:** chore · **Area:** iterate · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Weekly: KPI dashboard to actions; template + first review; are metrics healthy?
  - **AC:**
    - [ ] first review done

- [ ] **P6-032** — Feedback intake
  - **Type:** feat · **Area:** iterate · **Est:** S · **Deps:** P6-031 · **Prio:** P1
  - **Spec:** In-app feedback form to feedback table + digest email; triage guide.
  - **AC:**
    - [ ] form e2e green

- [ ] **P6-033** — Model upgrade cadence
  - **Type:** chore · **Area:** iterate · **Est:** M · **Deps:** P6-032 · **Prio:** P1
  - **Spec:** Process: new badge templates/models quarterly or on detection-miss reports; eval harness reuse.
  - **AC:**
    - [ ] cadence documented

- [ ] **P6-034** — Threshold recalibration
  - **Type:** feat · **Area:** iterate · **Est:** M · **Deps:** P6-033 · **Prio:** P1
  - **Spec:** Post-launch: recalibrate gates with real distributions (calibration procedure); ADR.
  - **AC:**
    - [ ] ADR with distributions

- [ ] **P6-035** — Incident retro
  - **Type:** chore · **Area:** iterate · **Est:** S · **Deps:** P6-034 · **Prio:** P1
  - **Spec:** First real incident postmortem (template); actions become tickets.
  - **AC:**
    - [ ] retro committed

- [ ] **P6-036** — Tech-debt burn-down
  - **Type:** chore · **Area:** iterate · **Est:** M · **Deps:** P6-035 · **Prio:** P1
  - **Spec:** Debt list (from ADRs/issues) prioritized; sprint scheduled.
  - **AC:**
    - [ ] list + schedule

- [ ] **P6-037** — Dependency upgrade window
  - **Type:** chore · **Area:** iterate · **Est:** M · **Deps:** P6-036 · **Prio:** P1
  - **Spec:** Quarterly: rust toolchain, crates, npm majors with full suites; notes.
  - **AC:**
    - [ ] window executed

- [ ] **P6-038** — Annual compliance review
  - **Type:** chore · **Area:** iterate · **Est:** S · **Deps:** P6-037 · **Prio:** P1
  - **Spec:** Checklist: AUP/legal/retention/privacy-page review cycle; owner calendar invite text.
  - **AC:**
    - [ ] checklist committed

---

## E6.5 — Research infra (area: research)

- [ ] **P6-039** — Research log template
  - **Type:** docs · **Area:** research · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Format + where entries live; citation standards (URL + access date).
  - **AC:**
    - [ ] template committed

- [ ] **P6-040** — Eval dataset curation
  - **Type:** chore · **Area:** research · **Est:** M · **Deps:** P6-039 · **Prio:** P1
  - **Spec:** Licensed dataset plan (own generations + licensed sets) for detector evals; storage + versioning; no scraping.
  - **AC:**
    - [ ] plan committed

- [ ] **P6-041** — Detector metrics harness
  - **Type:** feat · **Area:** research · **Est:** S · **Deps:** P6-040 · **Prio:** P1
  - **Spec:** Standard P/R/IoU harness reused across model candidates; CI-runnable on synthetic set.
  - **AC:**
    - [ ] harness green

- [ ] **P6-042** — Reproducibility pins
  - **Type:** chore · **Area:** research · **Est:** S · **Deps:** P6-041 · **Prio:** P1
  - **Spec:** Seeds, env pins, model hashes for every research artifact (repro-or-marked-nonrepro rule).
  - **AC:**
    - [ ] pins documented

- [ ] **P6-043** — Research PR template
  - **Type:** docs · **Area:** research · **Est:** S · **Deps:** P6-042 · **Prio:** P1
  - **Spec:** Template: question, evidence, recommendation, decision needed; used for all P6 outputs.
  - **AC:**
    - [ ] template committed
