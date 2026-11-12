# PHASE 2 — Removal Engine (images)

> **Tickets: 102** · Types: feat 73 · chore 4 · test 11 · docs 11 · spike 3

**Goal:** The actual watermark removal for images: intake sniffing, mask ops, deterministic template detection + ONNX text detection, tiled LaMa inpainting with an in-house fallback, format-preserving encode, masked SSIM/PSNR quality gates, engine CLI, golden suite calibration.

**Context (read before starting):**
- Every engine ticket ends with a golden-suite run; metrics go in the commit body (04-ENGINEERING/01 §Definition of engine quality).
- Model licenses must be audited before download (E2.10). No scraped watermark assets ever - templates are synthesized (compliance doc).
- Quality gates from 02-REQUIREMENTS/04-quality-policy.md are law; thresholds live in crates/engine thresholds.rs.

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] Golden suite green: all fixture envelopes within floors (SSIM >= 0.98, PSNR >= 38 dB unmasked; PNG untouched pixels bit-exact).
- [ ] Determinism proven: identical input+params+models run twice = bit-identical output.
- [ ] pf-engine CLI processes a fixture end-to-end (mask, result, report).
- [ ] Detection eval: badge IoU >= 0.8 on badge fixtures; zero false positives above threshold on clean fixtures.
- [ ] License audit complete; model registry sha256-pinned; boot refuses corrupt/missing models.

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E2.1 Media intake | engine/intake | 10 |
| E2.2 Mask & region ops | engine/mask | 12 |
| E2.3 Detection: template pack | engine/detect | 10 |
| E2.4 Detection: text models | engine/detect | 12 |
| E2.5 Inpainting | engine/inpaint | 14 |
| E2.6 Output encode & metadata | engine/encode | 12 |
| E2.7 Quality metrics | engine/metrics | 10 |
| E2.8 Engine facade & CLI | engine/core | 9 |
| E2.9 Manual mode & hints | engine/hints | 7 |
| E2.10 Model & asset governance | engine/assets | 6 |

---

## E2.1 — Media intake (area: engine/intake)

- [ ] **P2-001** — Magic-byte sniffing
  - **Type:** feat · **Area:** engine/intake · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Sniffer detects png/jpeg/webp/tiff/mp4/mkv/webm/avif from head+tail bytes; extension ignored (FR-U-03); svg/pdf/unknown rejected PF-2001; table-driven unit tests.
  - **AC:**
    - [ ] extension-vs-magic mismatch: magic wins

- [ ] **P2-002** — Image decoder matrix
  - **Type:** feat · **Area:** engine/intake · **Est:** M · **Deps:** P2-001 · **Prio:** P1
  - **Spec:** image crate: png/jpeg/webp (lossy+lossless) decode; avif/tiff behind cargo features (documented); 16-bit png path preserves bit depth.
  - **AC:**
    - [ ] format matrix green

- [ ] **P2-003** — EXIF parse
  - **Type:** feat · **Area:** engine/intake · **Est:** M · **Deps:** P2-002 · **Prio:** P1
  - **Spec:** kamadak-exif: summary extraction (orientation, camera, software) into jsonb; malformed-EXIF fuzz target (cargo-fuzz); parser never panics.
  - **AC:**
    - [ ] fuzz target runs nightly
    - [ ] no panics

- [ ] **P2-004** — ICC handling
  - **Type:** feat · **Area:** engine/intake · **Est:** S · **Deps:** P2-003 · **Prio:** P1
  - **Spec:** Extract ICC blob + presence flag; embed into output unchanged (FR-D-07); unknown profiles pass through; srgb/p3 fixture tests.
  - **AC:**
    - [ ] ICC byte-identical post-roundtrip

- [ ] **P2-005** — Orientation normalize
  - **Type:** feat · **Area:** engine/intake · **Est:** M · **Deps:** P2-004 · **Prio:** P1
  - **Spec:** Apply EXIF orientation into pixels, record applied transform in metadata (metrics compare correctly); orientation tag stripped after apply.
  - **AC:**
    - [ ] golden pixels match expected orientation

- [ ] **P2-006** — Dimension/size guards
  - **Type:** feat · **Area:** engine/intake · **Est:** S · **Deps:** P2-005 · **Prio:** P1
  - **Spec:** Max 8000x8000 image + plan byte caps at intake; decompression-bomb guard (pixel budget) before full decode (PF-2005).
  - **AC:**
    - [ ] bomb fixture rejected

- [ ] **P2-007** — Decode memory budget
  - **Type:** feat · **Area:** engine/intake · **Est:** M · **Deps:** P2-006 · **Prio:** P1
  - **Spec:** Budgeted decode path for huge images (tile-aware later); RSS ceiling assertion in bench; budget breach errors PF-2007.
  - **AC:**
    - [ ] bench asserts ceiling

- [ ] **P2-008** — Hash + perceptual hash
  - **Type:** feat · **Area:** engine/intake · **Est:** M · **Deps:** P2-007 · **Prio:** P1
  - **Spec:** Streaming sha256 + dHash/aHash (imageproc) stored on uploads; feeds dedupe analytics + abuse velocity (FR-U-05).
  - **AC:**
    - [ ] hash stability tested

- [ ] **P2-009** — Intake suite
  - **Type:** test · **Area:** engine/intake · **Est:** M · **Deps:** P2-008 · **Prio:** P1
  - **Spec:** Format matrix incl. polyglot + bomb rejects, orientation correctness, ICC preserved, hash stability.
  - **AC:**
    - [ ] suite green

- [ ] **P2-010** — Intake spec
  - **Type:** docs · **Area:** engine/intake · **Est:** S · **Deps:** P2-009 · **Prio:** P1
  - **Spec:** Supported matrix table, error codes, memory budgets, security notes tied to threat model.
  - **AC:**
    - [ ] spec committed

---

## E2.2 — Mask & region ops (area: engine/mask)

- [ ] **P2-011** — Region types
  - **Type:** feat · **Area:** engine/mask · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Domain types: box, polygon, RLE mask; region {kind: template|text|hint, confidence, source_model}; versioned serde schema.
  - **AC:**
    - [ ] schema versioned + roundtrip tested

- [ ] **P2-012** — RLE codec
  - **Type:** feat · **Area:** engine/mask · **Est:** M · **Deps:** P2-011 · **Prio:** P1
  - **Spec:** Binary RLE mask codec; roundtrip property tests + fuzz (no panic, size bounds); used in job events + reports.
  - **AC:**
    - [ ] fuzz clean
    - [ ] roundtrip 1000 cases

- [ ] **P2-013** — Morphology ops
  - **Type:** feat · **Area:** engine/mask · **Est:** M · **Deps:** P2-012 · **Prio:** P1
  - **Spec:** Dilate/erode/close/open via imageproc; policy radii from quality policy (dilate for fair metrics); config-tunable, changes need ADR.
  - **AC:**
    - [ ] idempotence bounds tested

- [ ] **P2-014** — Feather + blend
  - **Type:** feat · **Area:** engine/mask · **Est:** M · **Deps:** P2-013 · **Prio:** P1
  - **Spec:** Gaussian feather along mask edge + alpha composite paste-back; edge-quality golden cases (halo artifact check).
  - **AC:**
    - [ ] halo cases pass

- [ ] **P2-015** — NMS + region merge
  - **Type:** feat · **Area:** engine/mask · **Est:** S · **Deps:** P2-014 · **Prio:** P1
  - **Spec:** Non-max suppression for detections; overlap merge policy (IoU threshold); user hints win precedence, merge logged.
  - **AC:**
    - [ ] precedence tested

- [ ] **P2-016** — Tile planner
  - **Type:** feat · **Area:** engine/mask · **Est:** M · **Deps:** P2-015 · **Prio:** P1
  - **Spec:** Plan tiles (<=1024) covering mask bboxes with overlap + blend margins; adversarial layouts tested (corner masks, huge masks).
  - **AC:**
    - [ ] tile coverage == full mask

- [ ] **P2-017** — Coordinate spaces
  - **Type:** feat · **Area:** engine/mask · **Est:** S · **Deps:** P2-016 · **Prio:** P1
  - **Spec:** Explicit space types (original, decoded, tile) with From conversions + clamp helpers; prevents off-by-scale bugs.
  - **AC:**
    - [ ] conversion unit tests green

- [ ] **P2-018** — Mask serialization in events
  - **Type:** feat · **Area:** engine/mask · **Est:** M · **Deps:** P2-017 · **Prio:** P1
  - **Spec:** job_events carry RLE mask snapshots per stage (detect, detect_refine) for UI + debug; size-capped.
  - **AC:**
    - [ ] size cap enforced

- [ ] **P2-019** — Mask suite
  - **Type:** test · **Area:** engine/mask · **Est:** M · **Deps:** P2-018 · **Prio:** P1
  - **Spec:** Property: RLE roundtrip; morphology idempotence; feather invariant (outside-mask untouched); tile coverage completeness.
  - **AC:**
    - [ ] properties green

- [ ] **P2-020** — Mask visualizer (dev)
  - **Type:** feat · **Area:** engine/mask · **Est:** S · **Deps:** P2-019 · **Prio:** P1
  - **Spec:** Dev-only CLI overlay of masks on images for triage; used by golden debugging.
  - **AC:**
    - [ ] overlay renders

- [ ] **P2-021** — Mask fuzzing
  - **Type:** test · **Area:** engine/mask · **Est:** S · **Deps:** P2-020 · **Prio:** P1
  - **Spec:** cargo-fuzz targets for RLE decode + region serde; crashes become issues with minimized inputs.
  - **AC:**
    - [ ] fuzz clean

- [ ] **P2-022** — Mask ops guide
  - **Type:** docs · **Area:** engine/mask · **Est:** S · **Deps:** P2-021 · **Prio:** P1
  - **Spec:** Spaces diagram, RLE format spec, tuning knobs table + calibration link.
  - **AC:**
    - [ ] guide committed

---

## E2.3 — Detection: template pack (area: engine/detect)

- [ ] **P2-023** — Template pack format
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Pack = dir of {name}.png (RGBA) + manifest.json {name, sha256, scale range, notes}; loader validates checksums; pack version stamped into reports.
  - **AC:**
    - [ ] checksum enforcement tested

- [ ] **P2-024** — NCC multi-scale matcher
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-023 · **Prio:** P1
  - **Spec:** Normalized cross-correlation over scales (config range), top-k matches, score threshold; pure imageproc; 4K < 2s (bench).
  - **AC:**
    - [ ] perf budget met

- [ ] **P2-025** — Position priors
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-024 · **Prio:** P1
  - **Spec:** Corner/bottom-band priors (config) boost likely zones; rationale documented; ablation note in ADR.
  - **AC:**
    - [ ] ablation recorded

- [ ] **P2-026** — Badge template synthesis
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-025 · **Prio:** P1
  - **Spec:** Tool renders known-style badge texts/shapes over varied backgrounds into a pack with sha table; clearly synthetic; no scraped assets.
  - **AC:**
    - [ ] pack generated + committed

- [ ] **P2-027** — Confidence calibration
  - **Type:** feat · **Area:** engine/detect · **Est:** S · **Deps:** P2-026 · **Prio:** P1
  - **Spec:** Score to calibrated confidence (logistic fit on synthetic eval set); threshold policy; reports include raw + calibrated.
  - **AC:**
    - [ ] calibration data committed

- [ ] **P2-028** — Template registry versioning
  - **Type:** feat · **Area:** engine/detect · **Est:** S · **Deps:** P2-027 · **Prio:** P1
  - **Spec:** Pack version pinned in config; upgrade = ticket with detection eval (P/R on synthetic set) before/after.
  - **AC:**
    - [ ] upgrade procedure documented

- [ ] **P2-029** — CPU path decision
  - **Type:** feat · **Area:** engine/detect · **Est:** S · **Deps:** P2-028 · **Prio:** P1
  - **Spec:** Template matching stays CPU-first; rationale documented (NCC is fine on CPU); no GPU ticket here.
  - **AC:**
    - [ ] decision documented

- [ ] **P2-030** — Template golden tests
  - **Type:** test · **Area:** engine/detect · **Est:** M · **Deps:** P2-029 · **Prio:** P1
  - **Spec:** Per badge fixture: detection box IoU >= 0.8; clean fixtures: zero FP above threshold.
  - **AC:**
    - [ ] suite green

- [ ] **P2-031** — Detection perf bench
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-030 · **Prio:** P1
  - **Spec:** NCC + prior pipeline per resolution tier; >10% regression = fix ticket; results into PROGRESS bench table.
  - **AC:**
    - [ ] bench recorded

- [ ] **P2-032** — Template authoring guide
  - **Type:** docs · **Area:** engine/detect · **Est:** S · **Deps:** P2-031 · **Prio:** P1
  - **Spec:** How to add a badge template (render, manifest, eval, ship); legal note (synthesized only).
  - **AC:**
    - [ ] guide committed

---

## E2.4 — Detection: text models (area: engine/detect)

- [ ] **P2-033** — ort runtime bootstrap
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** ort session config (threads, arena), CPU default, CUDA behind gpu flag; session pool; init logs model version.
  - **AC:**
    - [ ] sessions initialize
    - [ ] version logged

- [ ] **P2-034** — Model registry + download
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-033 · **Prio:** P1
  - **Spec:** Registry manifest {model_id, file, sha256, license, source_url}; boot verifies/downloads into PF_ENGINE_MODELS_DIR; corrupt model refuses start + alert.
  - **AC:**
    - [ ] corrupt-sha refusal tested

- [ ] **P2-035** — DBNet text-det integration
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-034 · **Prio:** P1
  - **Spec:** Pre/post: resize, normalize, binarize, boxes to regions; config thresholds; post-processing math unit-tested with tiny tensors.
  - **AC:**
    - [ ] post-processing tests green

- [ ] **P2-036** — EAST alternative behind flag
  - **Type:** feat · **Area:** engine/detect · **Est:** S · **Deps:** P2-035 · **Prio:** P1
  - **Spec:** Same trait; PF_ENGINE_TEXTDET=east|dbnet; ADR documents dbnet default rationale.
  - **AC:**
    - [ ] flag switch works

- [ ] **P2-037** — Overlay-vs-scene classifier
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-036 · **Prio:** P1
  - **Spec:** Heuristics: uniform background, edge density, position priors, contrast to surroundings produce is_overlay score; threshold tunable; ablation in eval.
  - **AC:**
    - [ ] eval table committed

- [ ] **P2-038** — Detection result schema
  - **Type:** feat · **Area:** engine/detect · **Est:** S · **Deps:** P2-037 · **Prio:** P1
  - **Spec:** Unified DetectionResult {regions[], pack_version, model_versions, thresholds, timings}; consumed by pipeline + reports; serde snapshots.
  - **AC:**
    - [ ] schema snapshots locked

- [ ] **P2-039** — Batched inference
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-038 · **Prio:** P1
  - **Spec:** Batch regions across frames (video mode later); measured throughput gain; memory caps respected.
  - **AC:**
    - [ ] gain measured + capped

- [ ] **P2-040** — Threshold tuning harness
  - **Type:** feat · **Area:** engine/detect · **Est:** S · **Deps:** P2-039 · **Prio:** P1
  - **Spec:** Eval CLI: run detector over synthetic set producing P/R/F1 per threshold; outputs under docs/eval/.
  - **AC:**
    - [ ] harness committed

- [ ] **P2-041** — YOLOv8n logo spike
  - **Type:** spike · **Area:** engine/detect · **Est:** M · **Deps:** P2-040 · **Prio:** P1
  - **Spec:** Timeboxed: synthetic logo dataset gen + export path + eval; output = ADR go/no-go for a trained badge detector (Phase 6+); no model ships in v1 unless eval clearly beats heuristics.
  - **AC:**
    - [ ] ADR written with numbers

- [ ] **P2-042** — Model golden tests
  - **Type:** test · **Area:** engine/detect · **Est:** M · **Deps:** P2-041 · **Prio:** P1
  - **Spec:** Text-mark fixtures: detected region IoU >= 0.6; clean fixtures: FP <= 1 low-confidence region.
  - **AC:**
    - [ ] suite green

- [ ] **P2-043** — Detection perf bench
  - **Type:** feat · **Area:** engine/detect · **Est:** M · **Deps:** P2-042 · **Prio:** P1
  - **Spec:** Text-det ms per MP per tier; batch scaling; budget vs NFR-2 math documented.
  - **AC:**
    - [ ] budget math committed

- [ ] **P2-044** — Model cards
  - **Type:** docs · **Area:** engine/detect · **Est:** S · **Deps:** P2-043 · **Prio:** P1
  - **Spec:** docs/models.md per model: source, license, sha256, shapes, thresholds, known limits; capabilities stated plainly.
  - **AC:**
    - [ ] cards committed

---

## E2.5 — Inpainting (area: engine/inpaint)

- [ ] **P2-045** — LaMa ONNX runner
  - **Type:** feat · **Area:** engine/inpaint · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Load big-lama ONNX from registry; input prep (image+mask normalize); output composite; deterministic seeds; fp32 default, fp16 behind flag.
  - **AC:**
    - [ ] fixture result within floors

- [ ] **P2-046** — Tile engine
  - **Type:** feat · **Area:** engine/inpaint · **Est:** M · **Deps:** P2-045 · **Prio:** P1
  - **Spec:** Execute tile plan: pad, infer, overlap blend (cosine feather), no seams (golden checks); memory bounded (peak tracked).
  - **AC:**
    - [ ] no-seam cases pass

- [ ] **P2-047** — In-house diffusion fallback
  - **Type:** feat · **Area:** engine/inpaint · **Est:** M · **Deps:** P2-046 · **Prio:** P1
  - **Spec:** Deterministic push-pull/telea-style inpaint in-crate (pure Rust): runs when LaMa unavailable; property test outside-mask untouched; honest quality floors still enforced.
  - **AC:**
    - [ ] fallback passes floors or honestly quality_fails

- [ ] **P2-048** — Color/illumination match
  - **Type:** feat · **Area:** engine/inpaint · **Est:** M · **Deps:** P2-047 · **Prio:** P1
  - **Spec:** Post-inpaint low-frequency stats match (mean/std in LAB-ish space) to avoid tone patches; golden halo tests.
  - **AC:**
    - [ ] tone-shift cases pass

- [ ] **P2-049** — Edge-aware composite
  - **Type:** feat · **Area:** engine/inpaint · **Est:** M · **Deps:** P2-048 · **Prio:** P1
  - **Spec:** Paste-back with feather + optional Poisson-lite guidance; artifact checklist cases (gradient backgrounds, textured regions).
  - **AC:**
    - [ ] artifact cases pass

- [ ] **P2-050** — Hi-res mode for large masks
  - **Type:** feat · **Area:** engine/inpaint · **Est:** S · **Deps:** P2-049 · **Prio:** P1
  - **Spec:** Mask area > threshold triggers hi-res tiled pass + multi-scale blend; auto-retry policy uses this (quality gate flow).
  - **AC:**
    - [ ] retry uses hi-res

- [ ] **P2-051** — Determinism tests
  - **Type:** test · **Area:** engine/inpaint · **Est:** M · **Deps:** P2-050 · **Prio:** P1
  - **Spec:** Same input+params+models twice = bit-identical output (FR-D-06); explicit RNG seeds.
  - **AC:**
    - [ ] hash equality on fixtures

- [ ] **P2-052** — Backend selection ADR + policy
  - **Type:** feat · **Area:** engine/inpaint · **Est:** S · **Deps:** P2-051 · **Prio:** P1
  - **Spec:** ADR: LaMa primary, diffusion fallback, reasons + expected delta; choose_backend(mask, resolution, flags) policy function.
  - **AC:**
    - [ ] policy unit-tested

- [ ] **P2-053** — Inpaint perf bench
  - **Type:** feat · **Area:** engine/inpaint · **Est:** M · **Deps:** P2-052 · **Prio:** P1
  - **Spec:** criterion: tiles/s by tile size, peak RSS, full-image 1080p/4K times; baseline recorded; 10% regression gate.
  - **AC:**
    - [ ] baseline recorded

- [ ] **P2-054** — Memory caps
  - **Type:** feat · **Area:** engine/inpaint · **Est:** S · **Deps:** P2-053 · **Prio:** P1
  - **Spec:** Hard RSS budget per job (config); breach fails fast PF-4005 (never OOM-kill); constrained-alloc test.
  - **AC:**
    - [ ] breach fails fast

- [ ] **P2-055** — GPU feature flag
  - **Type:** feat · **Area:** engine/inpaint · **Est:** S · **Deps:** P2-054 · **Prio:** P1
  - **Spec:** CUDA provider + warm pool behind PF_ENGINE_GPU; CPU remains default supported path; flag-gated smoke; docs state when GPU helps.
  - **AC:**
    - [ ] flag-gated smoke green (if GPU present)

- [ ] **P2-056** — INT8 quantized spike
  - **Type:** spike · **Area:** engine/inpaint · **Est:** S · **Deps:** P2-055 · **Prio:** P1
  - **Spec:** Quantized LaMa eval: speed vs SSIM delta; ADR go/no-go; never ship below-floor output.
  - **AC:**
    - [ ] ADR with numbers

- [ ] **P2-057** — Inpaint golden suite
  - **Type:** test · **Area:** engine/inpaint · **Est:** M · **Deps:** P2-056 · **Prio:** P1
  - **Spec:** Per fixture: floors pass; artifact cases (halo, seam, tone shift) absent via metrics + perceptual distance.
  - **AC:**
    - [ ] suite green

- [ ] **P2-058** — Inpainting guide
  - **Type:** docs · **Area:** engine/inpaint · **Est:** S · **Deps:** P2-057 · **Prio:** P1
  - **Spec:** Pipeline diagram, knob table, backend selection, determinism statement, memory math.
  - **AC:**
    - [ ] guide committed

---

## E2.6 — Output encode & metadata (area: engine/encode)

- [ ] **P2-059** — Encode policy engine
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** same-as-source policy per format; settings from quality policy (JPEG q>=95 4:4:4, PNG lossless, WebP q>=90); policy unit-tested against FR-Q-03.
  - **AC:**
    - [ ] policy table test green

- [ ] **P2-060** — JPEG encoder path
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** P2-059 · **Prio:** P1
  - **Spec:** Re-encode with quality ladder + 4:4:4; estimate source quality (q-table distance) to choose target; settings stored in report.
  - **AC:**
    - [ ] estimated-source-q logic tested

- [ ] **P2-061** — PNG lossless verify
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** P2-060 · **Prio:** P1
  - **Spec:** Untouched pixels bit-exact after roundtrip (hash unmasked region); failure = pipeline bug PF-4006, never user-visible degradation.
  - **AC:**
    - [ ] bit-exact verified on fixtures

- [ ] **P2-062** — WebP/AVIF policies
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** P2-061 · **Prio:** P1
  - **Spec:** WebP encode q>=90 (lossless if source lossless); AVIF behind feature flag with ADR (encode speed); format matrix doc.
  - **AC:**
    - [ ] policies tested

- [ ] **P2-063** — ICC preservation
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** P2-062 · **Prio:** P1
  - **Spec:** Embed original ICC blob on encode for all supporting formats; binary compare post-encode.
  - **AC:**
    - [ ] byte-identical ICC

- [ ] **P2-064** — EXIF strip policy
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** P2-063 · **Prio:** P1
  - **Spec:** Default: strip EXIF/XMP (privacy), orientation already applied (E2.1-5); per-job override documented as risky; NFR-43 aligned.
  - **AC:**
    - [ ] strip default verified

- [ ] **P2-065** — DPI/aspect metadata
  - **Type:** feat · **Area:** engine/encode · **Est:** S · **Deps:** P2-064 · **Prio:** P1
  - **Spec:** Preserve DPI where supported; aspect never changes; 72/300dpi fixture tests.
  - **AC:**
    - [ ] DPI preserved

- [ ] **P2-066** — Output naming/keys
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** P2-065 · **Prio:** P1
  - **Spec:** Artifact rows + storage keys per ADR-0011; content-type set correctly; filename mirrors input stem + suffix.
  - **AC:**
    - [ ] keys + naming tested

- [ ] **P2-067** — Encode bench
  - **Type:** feat · **Area:** engine/encode · **Est:** M · **Deps:** P2-066 · **Prio:** P1
  - **Spec:** Encode ms + size ratio per format/tier; size-ratio report (no bloat); regression gate.
  - **AC:**
    - [ ] bench recorded

- [ ] **P2-068** — Encode suite
  - **Type:** test · **Area:** engine/encode · **Est:** M · **Deps:** P2-067 · **Prio:** P1
  - **Spec:** Roundtrip: format, dims, ICC, DPI; quality settings asserted; decode-back verify (parses + dims match).
  - **AC:**
    - [ ] suite green

- [ ] **P2-069** — Corrupted-output guard
  - **Type:** feat · **Area:** engine/encode · **Est:** S · **Deps:** P2-068 · **Prio:** P1
  - **Spec:** Post-encode decode-back before success; mismatch PF-4006 + job fail (no bad file delivery).
  - **AC:**
    - [ ] corrupt output never delivered

- [ ] **P2-070** — Format matrix doc
  - **Type:** docs · **Area:** engine/encode · **Est:** S · **Deps:** P2-069 · **Prio:** P1
  - **Spec:** Per format: decode, encode, metadata policy, caveats; linked from quality policy.
  - **AC:**
    - [ ] matrix committed

---

## E2.7 — Quality metrics (area: engine/metrics)

- [ ] **P2-071** — SSIM implementation
  - **Type:** feat · **Area:** engine/metrics · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Windowed SSIM (11x11 gaussian) on luminance restricted to mask complement; validated against committed reference vectors (4-decimal match).
  - **AC:**
    - [ ] vector match exact

- [ ] **P2-072** — PSNR implementation
  - **Type:** feat · **Area:** engine/metrics · **Est:** M · **Deps:** P2-071 · **Prio:** P1
  - **Spec:** Masked PSNR (MSE over unmasked only); vector-validated; dB floor from thresholds.rs.
  - **AC:**
    - [ ] vector match exact

- [ ] **P2-073** — Masked metric service
  - **Type:** feat · **Area:** engine/metrics · **Est:** M · **Deps:** P2-072 · **Prio:** P1
  - **Spec:** Composes input/output/mask into metrics struct; non-finite and empty-mask guards (fully-masked skips pixel metrics with note).
  - **AC:**
    - [ ] edge cases handled

- [ ] **P2-074** — LPIPS optional spike
  - **Type:** spike · **Area:** engine/metrics · **Est:** S · **Deps:** P2-073 · **Prio:** P1
  - **Spec:** ONNX LPIPS behind flag for a future perceptual metric; ADR with eval vs SSIM disagreements; off by default.
  - **AC:**
    - [ ] ADR committed

- [ ] **P2-075** — Report JSON schema
  - **Type:** feat · **Area:** engine/metrics · **Est:** M · **Deps:** P2-074 · **Prio:** P1
  - **Spec:** Quality report struct per quality-policy section 5 (serde, versioned); snapshot tests; consumer contract for web + API.
  - **AC:**
    - [ ] schema snapshots locked

- [ ] **P2-076** — Gate evaluation + thresholds
  - **Type:** feat · **Area:** engine/metrics · **Est:** M · **Deps:** P2-075 · **Prio:** P1
  - **Spec:** thresholds.rs single source; gate() returns pass/fail + reasons; threshold changes = ADR + calibration rerun.
  - **AC:**
    - [ ] gate tested incl. failure reasons

- [ ] **P2-077** — Auto-retry policy
  - **Type:** feat · **Area:** engine/metrics · **Est:** M · **Deps:** P2-076 · **Prio:** P1
  - **Spec:** Gate fail triggers one stronger retry (hi-res tiled + wider feather + color match); retried flag in report; second fail = quality_failed.
  - **AC:**
    - [ ] retry path e2e on fixture

- [ ] **P2-078** — Report persistence
  - **Type:** feat · **Area:** engine/metrics · **Est:** S · **Deps:** P2-077 · **Prio:** P1
  - **Spec:** quality_reports row per job (data-model); report endpoint contract; retention per plan TTL.
  - **AC:**
    - [ ] persistence + retention tested

- [ ] **P2-079** — Metric validation suite
  - **Type:** test · **Area:** engine/metrics · **Est:** M · **Deps:** P2-078 · **Prio:** P1
  - **Spec:** Vector tests + integration: synthetic degraded outputs (blur, resize attempts) MUST fail gates - proves gates bite.
  - **AC:**
    - [ ] degraded outputs fail

- [ ] **P2-080** — Metrics doc
  - **Type:** docs · **Area:** engine/metrics · **Est:** S · **Deps:** P2-079 · **Prio:** P1
  - **Spec:** Math definitions, masked-region rationale, calibration link, anti-gaming notes (why mask complement).
  - **AC:**
    - [ ] doc committed

---

## E2.8 — Engine facade & CLI (area: engine/core)

- [ ] **P2-081** — Engine trait + pipeline
  - **Type:** feat · **Area:** engine/core · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** crates/engine: Engine::process(input, params) returns {output, mask, report}; stage traits (Detect, Plan, Inpaint, Encode, QC) composable; params validated.
  - **AC:**
    - [ ] pipeline e2e on fixture

- [ ] **P2-082** — Dry-run mode
  - **Type:** feat · **Area:** engine/core · **Est:** S · **Deps:** P2-081 · **Prio:** P1
  - **Spec:** mode=detect_only: mask + detections only, no pixel edit (FR-D-04); artifacts mask.png + detections.json.
  - **AC:**
    - [ ] no pixel change verified

- [ ] **P2-083** — metadata_only mode
  - **Type:** feat · **Area:** engine/core · **Est:** M · **Deps:** P2-082 · **Prio:** P1
  - **Spec:** Metadata rewrite path (EXIF strip, C2PA preserve) with true stream copy for video (Phase 3 wires); images: strip only; untouched pixels bit-exact.
  - **AC:**
    - [ ] pixels bit-exact

- [ ] **P2-084** — pf-engine CLI
  - **Type:** feat · **Area:** engine/core · **Est:** M · **Deps:** P2-083 · **Prio:** P1
  - **Spec:** bin: inspect an image, flags for backend/tiles/dry-run, outputs mask/report/result; used for debugging + golden triage.
  - **AC:**
    - [ ] CLI processes fixture

- [ ] **P2-085** — CLI tests
  - **Type:** test · **Area:** engine/core · **Est:** M · **Deps:** P2-084 · **Prio:** P1
  - **Spec:** Exit codes, outputs written, report schema, determinism (two runs same hash).
  - **AC:**
    - [ ] determinism hash equal

- [ ] **P2-086** — Golden regression runner
  - **Type:** chore · **Area:** engine/core · **Est:** M · **Deps:** P2-085 · **Prio:** P1
  - **Spec:** `just golden`: full fixture suite with per-fixture report + summary table; CI job on engine changes; failures annotate the tripped floor.
  - **AC:**
    - [ ] runner in CI

- [ ] **P2-087** — Engine version stamping
  - **Type:** feat · **Area:** engine/core · **Est:** S · **Deps:** P2-086 · **Prio:** P1
  - **Spec:** engine_version + model versions embedded in every report; provenance for support/debug.
  - **AC:**
    - [ ] versions present in reports

- [ ] **P2-088** — Crash-safe temp management
  - **Type:** feat · **Area:** engine/core · **Est:** S · **Deps:** P2-087 · **Prio:** P1
  - **Spec:** Per-job temp dir (uuid), cleanup on drop + boot sweep of orphans; no cross-job contamination; disk budget guard.
  - **AC:**
    - [ ] orphan sweep tested

- [ ] **P2-089** — Engine architecture doc
  - **Type:** docs · **Area:** engine/core · **Est:** S · **Deps:** P2-088 · **Prio:** P1
  - **Spec:** Stage graph, extension points, performance notes, determinism statement; links architecture diagrams.
  - **AC:**
    - [ ] doc committed

---

## E2.9 — Manual mode & hints (area: engine/hints)

- [ ] **P2-090** — Hints API schema
  - **Type:** feat · **Area:** engine/hints · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** hints [{type: box|polygon, coords[], label?}] validated (bounds, max 50); PF-2008 on invalid; documented in api-spec.
  - **AC:**
    - [ ] validation tested

- [ ] **P2-091** — Hint to mask conversion
  - **Type:** feat · **Area:** engine/hints · **Est:** M · **Deps:** P2-090 · **Prio:** P1
  - **Spec:** Rasterize boxes/polygons to mask with policy dilation; coordinate-space conversions enforced (E2.2-7).
  - **AC:**
    - [ ] rasterization golden pixels

- [ ] **P2-092** — Auto + manual merge
  - **Type:** feat · **Area:** engine/hints · **Est:** M · **Deps:** P2-091 · **Prio:** P1
  - **Spec:** manual = hints only; auto = detections only; union mode with hints precedence; mode mapping documented.
  - **AC:**
    - [ ] merge precedence tested

- [ ] **P2-093** — Hint validation UX contract
  - **Type:** feat · **Area:** engine/hints · **Est:** S · **Deps:** P2-092 · **Prio:** P1
  - **Spec:** Field-level error shapes for web; max sizes; api-spec section complete.
  - **AC:**
    - [ ] error shapes documented

- [ ] **P2-094** — Hint suite
  - **Type:** test · **Area:** engine/hints · **Est:** M · **Deps:** P2-093 · **Prio:** P1
  - **Spec:** Rasterization correctness, merge precedence, invalid inputs rejected.
  - **AC:**
    - [ ] suite green

- [ ] **P2-095** — detect_only wiring
  - **Type:** feat · **Area:** engine/hints · **Est:** S · **Deps:** P2-094 · **Prio:** P1
  - **Spec:** Job kind image + mode detect_only end-to-end via worker (no inpaint stage); artifacts + minimal report; counts as image job (documented).
  - **AC:**
    - [ ] e2e via worker green

- [ ] **P2-096** — Manual mode guide
  - **Type:** docs · **Area:** engine/hints · **Est:** S · **Deps:** P2-095 · **Prio:** P1
  - **Spec:** When to use, expected quality behavior, screenshot flow for Phase 4.
  - **AC:**
    - [ ] guide committed

---

## E2.10 — Model & asset governance (area: engine/assets)

- [ ] **P2-097** — License audit
  - **Type:** chore · **Area:** engine/assets · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Audit + record license of every model/asset (lama, dbnet, eval sets): redistribution allowed? attribution needed? docs/models.md table; nothing non-compliant ships.
  - **AC:**
    - [ ] audit table complete

- [ ] **P2-098** — ONNX export scripts
  - **Type:** chore · **Area:** engine/assets · **Est:** M · **Deps:** P2-097 · **Prio:** P1
  - **Spec:** Scripts + instructions reproducing ONNX files from upstream (torch to onnx) with fixed opset; outputs sha256-matched to registry.
  - **AC:**
    - [ ] reproduction documented

- [ ] **P2-099** — Registry pinning table
  - **Type:** feat · **Area:** engine/assets · **Est:** S · **Deps:** P2-098 · **Prio:** P1
  - **Spec:** models.toml: id, version, sha256, license, source; boot verifies; drift refuses start (tested).
  - **AC:**
    - [ ] drift refusal tested

- [ ] **P2-100** — Asset integrity CI
  - **Type:** chore · **Area:** engine/assets · **Est:** S · **Deps:** P2-099 · **Prio:** P1
  - **Spec:** CI checks fixture/template/model manifests (sha tables) consistent; prevents silent asset drift.
  - **AC:**
    - [ ] integrity job green

- [ ] **P2-101** — Redistribution compliance
  - **Type:** docs · **Area:** engine/assets · **Est:** S · **Deps:** P2-100 · **Prio:** P1
  - **Spec:** Statement of what is redistributed (models, synthesized templates), attribution file, takedown contact.
  - **AC:**
    - [ ] statement committed

- [ ] **P2-102** — Governance ADR
  - **Type:** docs · **Area:** engine/assets · **Est:** S · **Deps:** P2-101 · **Prio:** P1
  - **Spec:** ADR: only synthesized templates + permissively licensed models; no scraped watermark assets ever (compliance doc).
  - **AC:**
    - [ ] ADR committed
