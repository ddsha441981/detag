# RENAME.md — PureFrame → Detag rename status

> Decision: ADR-0013 in `DECISIONS.md` (2026-11-12, Phase 0). Owner renamed the
> product; this file tracks what is renamed and what still says PureFrame.

## Renamed (done)

| Area | Before | After |
| --- | --- | --- |
| GitHub repo | — (new) | `ddsha441981/detag` (public since 2026-11-13) |
| Local repo | `~/workspace/pureframe` (kit only) | `~/workspace/detag` |
| Product / brand | PureFrame | Detag |
| Crates | `pureframe-*` (kit) | `detag-api`, `detag-worker`, `detag-core`, `detag-engine`, `detag-storage`, `detag-mailer` |
| Binaries | — | `detag-api`, `detag-worker` |
| CI / changelog / docs (new) | — | Detag throughout |
| Goal | — | "Detag — watermark-removal SaaS" |

## Still PureFrame (by design, ADR-0013)

- **Kit docs under `docs/`** keep the PureFrame name as the spec source until
  the owner asks for a doc rename. A doc-rename ticket may be added later;
  no functional impact.
- The original kit archive: `pureframe-agent-kit.zip` (kept as the import
  source; see `CHANGELOG.md` "import PureFrame agent kit").

## Unchanged by the rename

- Pricing / plan defaults: Free ₹0 / Pro ₹499 / Business ₹1,999.
- Stack, quality gates (SSIM ≥ 0.98, PSNR ≥ 38 dB, VMAF ≥ 93), compliance
  (ownership attestation + AUP, C2PA strip OFF).

## Status

- **2026-11-14:** rename complete in repo, code, brand and CI. Kit docs
  still say PureFrame (deliberate, per ADR-0013). Nothing further to do
  unless the owner requests the doc rename.
