# PHASE 3 — Pipeline & Video

> **Tickets: 105** · Types: feat 79 · chore 3 · test 14 · docs 9

**Goal:** Production-grade job pipeline: state machine, SSE progress, idempotency, queue lanes/backpressure, resumable uploads, the video pipeline (ffprobe, keyframe detection, mask tracking, CRF<=16 encode, audio stream-copy), VMAF gate, batches, webhooks, GC, resilience drills, perf targets.

**Context (read before starting):**
- Kill-mid-job resilience (NFR-21) is proven with drill scripts, not assumed.
- Memory budgets matter: no full-file-RAM video decode.
- Audio stream-copy bit-exact is a hard gate (FR-D-08); metadata_only mode is the true-lossless path.

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] Job lifecycle e2e green incl. cancel/retry/idempotency semantics; state machine property tests pass.
- [ ] Video fixture pipeline: resolution/fps/duration preserved, audio hash bit-exact, VMAF >= 93 or MS-SSIM fallback >= 0.97.
- [ ] Kill -9 drill: job re-queued and completes without loss (NFR-21 evidence recorded).
- [ ] Fairness: Pro jobs p95 wait <= 60s under Free-lane flood (NFR-11, scaled factor in CI).
- [ ] Image 4K p95 <= 30s and video >= 0.5x realtime on 4 vCPU (NFR-2/3) verified with evidence.

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E3.1 Jobs domain | jobs | 12 |
| E3.2 Queue hardening | queue | 10 |
| E3.3 Uploads & big files | uploads | 10 |
| E3.4 Video pipeline | video | 16 |
| E3.5 Video QC | video/qc | 8 |
| E3.6 Batch & multi | batch | 7 |
| E3.7 Results & delivery | delivery | 9 |
| E3.8 Storage accounting & GC | storage | 6 |
| E3.9 Resilience drills | drills | 6 |
| E3.10 Pipeline observability | observability | 7 |
| E3.11 Performance | perf | 8 |
| E3.12 API polish | api | 6 |

---

## E3.1 — Jobs domain (area: jobs)

- [ ] **P3-001** — Jobs service + state machine
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** States per FR-J-03 with guarded transitions (invalid PF-5002); transition events to job_events; state-machine unit + property tests (all paths).
  - **AC:**
    - [ ] property: no unreachable/stuck states

- [ ] **P3-002** — Job create endpoint
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-001 · **Prio:** P1
  - **Spec:** POST /v1/jobs full contract: upload ready-check, attestation check, params validation, quota pre-check, idempotency-key unique, enqueue; 202 shape.
  - **AC:**
    - [ ] contract test green

- [ ] **P3-003** — Idempotency layer
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-002 · **Prio:** P1
  - **Spec:** idempotency_keys table + middleware for POST /v1/jobs, /v1/uploads, checkout; replay returns stored response; concurrent duplicates single-effect (race test).
  - **AC:**
    - [ ] race test single-effect

- [ ] **P3-004** — Job status endpoint
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-003 · **Prio:** P1
  - **Spec:** GET /v1/jobs/{id}: state, stage, progress, timings, error details; ownership check (IDOR-safe 404).
  - **AC:**
    - [ ] cross-user 404

- [ ] **P3-005** — Job list + filters
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-004 · **Prio:** P1
  - **Spec:** Cursor pagination, filters (status, kind, created_after), created desc; index-backed (EXPLAIN checked in test).
  - **AC:**
    - [ ] EXPLAIN uses index

- [ ] **P3-006** — SSE events endpoint
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-005 · **Prio:** P1
  - **Spec:** GET /v1/jobs/{id}/events: redis pubsub to SSE; heartbeat 15s; reconnect token; latency p95 <= 500ms test (NFR-5).
  - **AC:**
    - [ ] latency test green

- [ ] **P3-007** — Progress protocol
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-006 · **Prio:** P1
  - **Spec:** Stage weights (upload 10, queue 5, detect 25, inpaint 45, encode 10, qc 5) to percent; stage events; UI contract note.
  - **AC:**
    - [ ] weights documented + tested

- [ ] **P3-008** — Cancel semantics
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-007 · **Prio:** P1
  - **Spec:** POST cancel: queued = dequeue; running = cooperative flag at stage boundaries; artifacts cleaned; unused-quota return policy documented + tested.
  - **AC:**
    - [ ] cancel e2e + quota return

- [ ] **P3-009** — Retry semantics
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-008 · **Prio:** P1
  - **Spec:** POST retry on failed/quality_failed: new attempt row, original preserved, quota re-charged; attempts cap 3 (PF-3004).
  - **AC:**
    - [ ] cap enforced

- [ ] **P3-010** — TTL cleanup worker
  - **Type:** feat · **Area:** jobs · **Est:** M · **Deps:** P3-009 · **Prio:** P1
  - **Spec:** Artifacts past expires_at deleted (storage + row); result URL refuses expired (PF-2009); runbook doc; purged-count metric.
  - **AC:**
    - [ ] expired refuses download

- [ ] **P3-011** — Dead-letter inspection
  - **Type:** feat · **Area:** jobs · **Est:** S · **Deps:** P3-010 · **Prio:** P1
  - **Spec:** Admin: list DLQ payloads, requeue/dismiss; links from jobs browser.
  - **AC:**
    - [ ] admin paths green

- [ ] **P3-012** — State machine suite
  - **Type:** test · **Area:** jobs · **Est:** M · **Deps:** P3-011 · **Prio:** P1
  - **Spec:** Property: no unreachable states, timeout watchdog prevents stuck states, cancel/retry combos; idempotency races.
  - **AC:**
    - [ ] properties green

---

## E3.2 — Queue hardening (area: queue)

- [ ] **P3-013** — Lanes + priorities
  - **Type:** feat · **Area:** queue · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Lane per plan (free/pro/business), workers consume weighted (business first); starvation guard (aging promotion, 60s cap per NFR-11).
  - **AC:**
    - [ ] aging promotion tested

- [ ] **P3-014** — Per-user concurrency caps
  - **Type:** feat · **Area:** queue · **Est:** M · **Deps:** P3-013 · **Prio:** P1
  - **Spec:** Slot check at enqueue; per-user running cap; round-robin fairness within lane.
  - **AC:**
    - [ ] cap + fairness tested

- [ ] **P3-015** — Backpressure
  - **Type:** feat · **Area:** queue · **Est:** S · **Deps:** P3-014 · **Prio:** P1
  - **Spec:** Queue depth > threshold: 429 Retry-After on job create (PF-3005) + alert; config per env.
  - **AC:**
    - [ ] backpressure trip tested

- [ ] **P3-016** — Retries with backoff
  - **Type:** feat · **Area:** queue · **Est:** M · **Deps:** P3-015 · **Prio:** P1
  - **Spec:** Jittered exponential per queue; attempt events; final attempt to dead with user-visible reason.
  - **AC:**
    - [ ] schedule tested

- [ ] **P3-017** — Poison-pill quarantine
  - **Type:** feat · **Area:** queue · **Est:** S · **Deps:** P3-016 · **Prio:** P1
  - **Spec:** Malformed/repeatedly-failing envelope to DLQ + payload snapshot + alert; worker loop never crashes.
  - **AC:**
    - [ ] loop survives poison

- [ ] **P3-018** — Visibility timeout tuning
  - **Type:** feat · **Area:** queue · **Est:** S · **Deps:** P3-017 · **Prio:** P1
  - **Spec:** Per-queue visibility vs stage durations; requeue sweep; requeue-count metric (target ~0).
  - **AC:**
    - [ ] tuning documented

- [ ] **P3-019** — Queue metrics per lane
  - **Type:** feat · **Area:** queue · **Est:** M · **Deps:** P3-018 · **Prio:** P1
  - **Spec:** Depth, age p50/p95, throughput per lane; dashboard panels; alert thresholds from runbook.
  - **AC:**
    - [ ] panels wired

- [ ] **P3-020** — Fairness tests
  - **Type:** test · **Area:** queue · **Est:** M · **Deps:** P3-019 · **Prio:** P1
  - **Spec:** Flood free lane with 1000 jobs; pro job p95 wait <= 60s (NFR-11) at CI-scaled factor.
  - **AC:**
    - [ ] fairness holds

- [ ] **P3-021** — Degraded mode (redis down)
  - **Type:** feat · **Area:** queue · **Est:** S · **Deps:** P3-020 · **Prio:** P1
  - **Spec:** Job create 503 + Retry-After; status reads from db; worker survives redis restart (reconnect + resume); chaos test.
  - **AC:**
    - [ ] NFR-13 verified

- [ ] **P3-022** — Queue design doc
  - **Type:** docs · **Area:** queue · **Est:** S · **Deps:** P3-021 · **Prio:** P1
  - **Spec:** Lane math, backpressure policy, tuning table, failure modes.
  - **AC:**
    - [ ] doc committed

---

## E3.3 — Uploads & big files (area: uploads)

- [ ] **P3-023** — Multipart presign flow
  - **Type:** feat · **Area:** uploads · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** POST /v1/uploads (init) to part URLs (part size 8-64MB config) to complete (verifies parts) + abort; provider-agnostic.
  - **AC:**
    - [ ] multipart e2e green

- [ ] **P3-024** — Resume support
  - **Type:** feat · **Area:** uploads · **Est:** M · **Deps:** P3-023 · **Prio:** P1
  - **Spec:** Client can re-request part URLs for pending uploads; parts listing; idempotent complete; resume across restart tested.
  - **AC:**
    - [ ] resume test green

- [ ] **P3-025** — Chunk size policy
  - **Type:** feat · **Area:** uploads · **Est:** S · **Deps:** P3-024 · **Prio:** P1
  - **Spec:** Per-kind defaults (image single presigned PUT, video multipart); plan caps enforced; docs.
  - **AC:**
    - [ ] caps enforced

- [ ] **P3-026** — Part integrity
  - **Type:** feat · **Area:** uploads · **Est:** M · **Deps:** P3-025 · **Prio:** P1
  - **Spec:** Optional per-part sha256 verify + server recompute on complete (E0.6-6); mismatch PF-2004.
  - **AC:**
    - [ ] mismatch rejected

- [ ] **P3-027** — Upload to job linking
  - **Type:** feat · **Area:** uploads · **Est:** M · **Deps:** P3-026 · **Prio:** P1
  - **Spec:** Only ready uploads create jobs; kind consistency (video upload for video job); upload reuse allowed (quota per job).
  - **AC:**
    - [ ] linking rules tested

- [ ] **P3-028** — Direct small PUT
  - **Type:** feat · **Area:** uploads · **Est:** S · **Deps:** P3-027 · **Prio:** P1
  - **Spec:** Files <= 8MB: single presigned PUT path (fewer roundtrips); same sniff + verify.
  - **AC:**
    - [ ] small path e2e

- [ ] **P3-029** — Orphan GC
  - **Type:** feat · **Area:** uploads · **Est:** S · **Deps:** P3-028 · **Prio:** P1
  - **Spec:** Uploads pending > 24h aborted + objects deleted; count metric; runbook.
  - **AC:**
    - [ ] GC removes orphans only

- [ ] **P3-030** — Concurrent upload caps
  - **Type:** feat · **Area:** uploads · **Est:** S · **Deps:** P3-029 · **Prio:** P1
  - **Spec:** Per-user active multipart sessions cap (plan); 429 otherwise; abuse guard.
  - **AC:**
    - [ ] cap enforced

- [ ] **P3-031** — Upload suite
  - **Type:** test · **Area:** uploads · **Est:** M · **Deps:** P3-030 · **Prio:** P1
  - **Spec:** Resume, part mismatch, expiry, caps, small-PUT path, cross-user IDOR on upload endpoints.
  - **AC:**
    - [ ] suite green

- [ ] **P3-032** — Upload guide
  - **Type:** docs · **Area:** uploads · **Est:** S · **Deps:** P3-031 · **Prio:** P1
  - **Spec:** Client implementation guide (TS + curl examples), size/part tables, error codes.
  - **AC:**
    - [ ] guide committed

---

## E3.4 — Video pipeline (area: video)

- [ ] **P3-033** — ffprobe wrapper
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Subprocess ffprobe -print_format json; serde model (streams, format, side_data); typed parse errors; argv-array exec (never shell).
  - **AC:**
    - [ ] parse tests green

- [ ] **P3-034** — Stream map preservation plan
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-033 · **Prio:** P1
  - **Spec:** From probe: video codec/profile/fps, audio codec/channels/bitrate, subtitle/attachment streams; plan per policy (audio+subs copy; attachments dropped with note).
  - **AC:**
    - [ ] plan matches policy

- [ ] **P3-035** — Scene/keyframe segmentation
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-034 · **Prio:** P1
  - **Spec:** Keyframe-based segmentation (ffprobe -show_frames); segments with pts ranges; deterministic.
  - **AC:**
    - [ ] determinism verified

- [ ] **P3-036** — Keyframe detection
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-035 · **Prio:** P1
  - **Spec:** Template + text-det on keyframes with sampling budget (config: every keyframe or Nth for long videos); per-segment detections cached.
  - **AC:**
    - [ ] budget respected

- [ ] **P3-037** — Mask propagation/tracking
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-036 · **Prio:** P1
  - **Spec:** Between keyframes: template-match the detected patch forward + backward refine; occlusion = confidence decay, hold last + shrink; ADR documents tracker choice (no heavy ML in v1).
  - **AC:**
    - [ ] tracker ADR + tests green

- [ ] **P3-038** — Per-frame inpaint scheduling
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-037 · **Prio:** P1
  - **Spec:** Budget-aware: only affected segments processed; tile plan per frame; worker yields progress (frames/total).
  - **AC:**
    - [ ] budget honored

- [ ] **P3-039** — Temporal smoothing
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-038 · **Prio:** P1
  - **Spec:** Blend masks/inpaint across adjacent frames (temporal median on mask edges) to reduce flicker; golden flicker metric (frame-to-frame SSIM delta bound).
  - **AC:**
    - [ ] flicker bound held

- [ ] **P3-040** — Encode profile selection
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-039 · **Prio:** P1
  - **Spec:** libx264 CRF<=16 preset slow yuv420p (H.264 out), same fps/res; hevc/vp9 decode support with H.264 out + note; audio -c:a copy mandatory (FR-D-08).
  - **AC:**
    - [ ] profile tests green

- [ ] **P3-041** — Container hygiene + faststart
  - **Type:** feat · **Area:** video · **Est:** S · **Deps:** P3-040 · **Prio:** P1
  - **Spec:** +faststart; moov placement; udta stripped; C2PA jumbf PRESERVED by default (policy engine hook for Phase 6).
  - **AC:**
    - [ ] atom policy tested

- [ ] **P3-042** — Progress mapping
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-041 · **Prio:** P1
  - **Spec:** frames_done/total to percent via stage weights; SSE updates throttled (max 2/s).
  - **AC:**
    - [ ] throttle verified

- [ ] **P3-043** — Streaming decode
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-042 · **Prio:** P1
  - **Spec:** Pipe ffmpeg frames to worker (no full-file RAM); RSS ceiling test on large fixture; budget breach PF-2007.
  - **AC:**
    - [ ] ceiling test green

- [ ] **P3-044** — Partial-success policy
  - **Type:** feat · **Area:** video · **Est:** S · **Deps:** P3-043 · **Prio:** P1
  - **Spec:** Segment below quality floor: process-but-flag (default) or fail-job (config); per-segment status in report (FR-J-10 analog).
  - **AC:**
    - [ ] policy tested

- [ ] **P3-045** — Video smoke fixture
  - **Type:** chore · **Area:** video · **Est:** M · **Deps:** P3-044 · **Prio:** P1
  - **Spec:** 30s 1080p h264 fixture with badge overlay + audio (synthesized, small) committed + generator script; used across tests.
  - **AC:**
    - [ ] fixture committed + reproducible

- [ ] **P3-046** — Video pipeline tests
  - **Type:** test · **Area:** video · **Est:** M · **Deps:** P3-045 · **Prio:** P1
  - **Spec:** Small-fixture e2e: resolution/fps/duration preserved, audio hash bit-exact, edited-region-only diff sampled, watermark gone (post-edit detection returns none).
  - **AC:**
    - [ ] suite green

- [ ] **P3-047** — Encode QC extraction
  - **Type:** feat · **Area:** video · **Est:** M · **Deps:** P3-046 · **Prio:** P1
  - **Spec:** Encode stats (q values, bitrate) from ffmpeg log into report; VMAF dist/ref alignment handled.
  - **AC:**
    - [ ] stats present in report

- [ ] **P3-048** — Video design doc
  - **Type:** docs · **Area:** video · **Est:** S · **Deps:** P3-047 · **Prio:** P1
  - **Spec:** Pipeline diagram, tracker rationale + limits (honest: fast motion reduces mask accuracy, hint mode mitigates), memory math.
  - **AC:**
    - [ ] doc committed

---

## E3.5 — Video QC (area: video/qc)

- [ ] **P3-049** — VMAF integration
  - **Type:** feat · **Area:** video/qc · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** libvmaf-enabled ffmpeg (ops build note or sidecar); run dist vs ref; JSON parse; fallback path when libvmaf unavailable.
  - **AC:**
    - [ ] VMAF computed on fixture

- [ ] **P3-050** — MS-SSIM fallback
  - **Type:** feat · **Area:** video/qc · **Est:** M · **Deps:** P3-049 · **Prio:** P1
  - **Spec:** Sampled frames (keyframes + every 30th), windowed MS-SSIM per frame aggregated (min/mean); floor 0.97; fallback flagged in report.
  - **AC:**
    - [ ] fallback clearly flagged

- [ ] **P3-051** — Property assertions
  - **Type:** feat · **Area:** video/qc · **Est:** M · **Deps:** P3-050 · **Prio:** P1
  - **Spec:** Post-encode ffprobe: resolution/fps/duration (+/-1 frame); mismatch PF-4007.
  - **AC:**
    - [ ] assertions tested

- [ ] **P3-052** — Audio bit-exact verify
  - **Type:** feat · **Area:** video/qc · **Est:** M · **Deps:** P3-051 · **Prio:** P1
  - **Spec:** Hash decoded PCM input vs output (codec copy) - mismatch is a pipeline bug; test asserts equality.
  - **AC:**
    - [ ] hash equality proven

- [ ] **P3-053** — Video report schema
  - **Type:** feat · **Area:** video/qc · **Est:** M · **Deps:** P3-052 · **Prio:** P1
  - **Spec:** Extends image report: vmaf, audio_hash_match, duration_frames_delta, per-segment table; snapshot tests.
  - **AC:**
    - [ ] schema snapshots locked

- [ ] **P3-054** — Gate + retry (video)
  - **Type:** feat · **Area:** video/qc · **Est:** M · **Deps:** P3-053 · **Prio:** P1
  - **Spec:** Fail triggers retry with slower preset/lower CRF + full sampled VMAF; second fail = quality_failed; per quality policy.
  - **AC:**
    - [ ] retry path tested

- [ ] **P3-055** — QC suite
  - **Type:** test · **Area:** video/qc · **Est:** M · **Deps:** P3-054 · **Prio:** P1
  - **Spec:** Synthetic degraded encodes (crf 40, resize) MUST FAIL gates; passing encodes pass deterministically.
  - **AC:**
    - [ ] gates bite

- [ ] **P3-056** — Video QC doc
  - **Type:** docs · **Area:** video/qc · **Est:** S · **Deps:** P3-055 · **Prio:** P1
  - **Spec:** Metric configs, fallback semantics, thresholds table, calibration link.
  - **AC:**
    - [ ] doc committed

---

## E3.6 — Batch & multi (area: batch)

- [ ] **P3-057** — Batch job type
  - **Type:** feat · **Area:** batch · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** ZIP in: safe extraction (bomb guards, entry caps), per-file child jobs; manifest json emitted; ZIP out artifact; kind=batch.
  - **AC:**
    - [ ] bomb guard tested

- [ ] **P3-058** — Per-file status
  - **Type:** feat · **Area:** batch · **Est:** M · **Deps:** P3-057 · **Prio:** P1
  - **Spec:** Children tracked under parent (job_events); partial-success semantics (FR-J-10): succeeded count + failed list with reasons.
  - **AC:**
    - [ ] partial semantics tested

- [ ] **P3-059** — Batch concurrency accounting
  - **Type:** feat · **Area:** batch · **Est:** S · **Deps:** P3-058 · **Prio:** P1
  - **Spec:** Batch counts as N jobs against quota at enqueue (refund on child cancel); documented.
  - **AC:**
    - [ ] refund path tested

- [ ] **P3-060** — Batch API
  - **Type:** feat · **Area:** batch · **Est:** S · **Deps:** P3-059 · **Prio:** P1
  - **Spec:** Create (upload zip), status (children inline), result (zip url); docs.
  - **AC:**
    - [ ] API contract tested

- [ ] **P3-061** — Batch suite
  - **Type:** test · **Area:** batch · **Est:** M · **Deps:** P3-060 · **Prio:** P1
  - **Spec:** Mixed-success zip (some corrupt files), caps, quota refund, manifest correctness.
  - **AC:**
    - [ ] suite green

- [ ] **P3-062** — Batch plan gate
  - **Type:** feat · **Area:** batch · **Est:** S · **Deps:** P3-061 · **Prio:** P1
  - **Spec:** features.batch check (Business default); PF-3003 otherwise.
  - **AC:**
    - [ ] gate enforced

- [ ] **P3-063** — Batch guide
  - **Type:** docs · **Area:** batch · **Est:** S · **Deps:** P3-062 · **Prio:** P1
  - **Spec:** Limits (entries, size), semantics, examples.
  - **AC:**
    - [ ] guide committed

---

## E3.7 — Results & delivery (area: delivery)

- [ ] **P3-064** — Signed result URLs
  - **Type:** feat · **Area:** delivery · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** GET /v1/jobs/{id}/result returns presigned GET (plan TTL) + artifacts list (output, mask, previews, report); single-use where supported.
  - **AC:**
    - [ ] TTL per plan tested

- [ ] **P3-065** — Result TTL cleanup
  - **Type:** feat · **Area:** delivery · **Est:** M · **Deps:** P3-064 · **Prio:** P1
  - **Spec:** Worker deletes expired artifacts (completes E3.1-10 loop); URL generation refuses expired (PF-2009).
  - **AC:**
    - [ ] cleanup loop green

- [ ] **P3-066** — Preview assets
  - **Type:** feat · **Area:** delivery · **Est:** M · **Deps:** P3-065 · **Prio:** P1
  - **Spec:** before/after webp thumbs + slider pair generated at QC stage; sizes 240/720; stored as artifacts.
  - **AC:**
    - [ ] preview sizes correct

- [ ] **P3-067** — Report endpoint
  - **Type:** feat · **Area:** delivery · **Est:** S · **Deps:** P3-066 · **Prio:** P1
  - **Spec:** GET report standalone (data-model already has it); cache headers set.
  - **AC:**
    - [ ] cache headers verified

- [ ] **P3-068** — Completion webhooks
  - **Type:** feat · **Area:** delivery · **Est:** M · **Deps:** P3-067 · **Prio:** P1
  - **Spec:** job.completed/failed to user endpoints: HMAC PF-Signature + timestamp header, retries 1/5/60min, delivery log rows; SSRF guard (public HTTPS only, NFR-35).
  - **AC:**
    - [ ] signature + retries + SSRF tests

- [ ] **P3-069** — Webhook delivery viewer API
  - **Type:** feat · **Area:** delivery · **Est:** S · **Deps:** P3-068 · **Prio:** P1
  - **Spec:** Deliveries list per endpoint (admin + owner self-view); owner replay POST for debugging.
  - **AC:**
    - [ ] replay works

- [ ] **P3-070** — SSE finalization
  - **Type:** feat · **Area:** delivery · **Est:** S · **Deps:** P3-069 · **Prio:** P1
  - **Spec:** done/failed terminal events include result/report URLs; connection cleanup; metrics.
  - **AC:**
    - [ ] terminal events correct

- [ ] **P3-071** — Download accounting
  - **Type:** feat · **Area:** delivery · **Est:** S · **Deps:** P3-070 · **Prio:** P1
  - **Spec:** Signed-URL download events counted per artifact; analytics only (no PII).
  - **AC:**
    - [ ] counts recorded

- [ ] **P3-072** — Delivery suite
  - **Type:** test · **Area:** delivery · **Est:** M · **Deps:** P3-071 · **Prio:** P1
  - **Spec:** HMAC tamper/replay/expire, SSRF denial (private CIDR), retry schedule simulation, cleanup invalidation.
  - **AC:**
    - [ ] suite green

---

## E3.8 — Storage accounting & GC (area: storage)

- [ ] **P3-073** — Per-user bytes used
  - **Type:** feat · **Area:** storage · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Aggregate artifact bytes per user (incremental on create/delete); admin view + quota hook (storage cap per plan).
  - **AC:**
    - [ ] aggregation correct

- [ ] **P3-074** — Storage quota enforcement
  - **Type:** feat · **Area:** storage · **Est:** S · **Deps:** P3-073 · **Prio:** P1
  - **Spec:** Job create checks user storage headroom (plan config); 429 PF-3006; tested.
  - **AC:**
    - [ ] quota trip tested

- [ ] **P3-075** — GC strategies
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** P3-074 · **Prio:** P1
  - **Spec:** Orphan object sweep (prefix scan vs artifacts table), TTL lifecycle verify (E0.6-8), report counts.
  - **AC:**
    - [ ] GC removes only orphans

- [ ] **P3-076** — Cold storage note
  - **Type:** feat · **Area:** storage · **Est:** S · **Deps:** P3-075 · **Prio:** P1
  - **Spec:** Doc: R2/S3 lifecycle to infrequent-access after 7d (optional, owner infra); no code v1.
  - **AC:**
    - [ ] doc committed

- [ ] **P3-077** — Storage accounting tests
  - **Type:** test · **Area:** storage · **Est:** M · **Deps:** P3-076 · **Prio:** P1
  - **Spec:** Byte math on create/delete/cancel; quota trip; GC behavior.
  - **AC:**
    - [ ] suite green

- [ ] **P3-078** — Storage ops doc update
  - **Type:** docs · **Area:** storage · **Est:** S · **Deps:** P3-077 · **Prio:** P1
  - **Spec:** Accounting semantics, GC cadence, cost model input.
  - **AC:**
    - [ ] doc updated

---

## E3.9 — Resilience drills (area: drills)

- [ ] **P3-079** — Kill-mid-job drill
  - **Type:** test · **Area:** drills · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Script: worker processing fixture video killed -9 mid-inpaint; job re-queued via visibility timeout and completes; artifact set consistent (NFR-21 evidence).
  - **AC:**
    - [ ] no lost jobs

- [ ] **P3-080** — Idempotency fuzz
  - **Type:** test · **Area:** drills · **Est:** M · **Deps:** P3-079 · **Prio:** P1
  - **Spec:** Fuzz duplicate deliveries (webhook + queue) simulating a day of events compressed; zero duplicate effects.
  - **AC:**
    - [ ] zero duplicates

- [ ] **P3-081** — Redis flush drill
  - **Type:** test · **Area:** drills · **Est:** S · **Deps:** P3-080 · **Prio:** P1
  - **Spec:** Flush redis under load: degraded mode per NFR-13; worker resumes; alerts fired; runbook updated.
  - **AC:**
    - [ ] degraded mode correct

- [ ] **P3-082** — Storage outage drill
  - **Type:** test · **Area:** drills · **Est:** S · **Deps:** P3-081 · **Prio:** P1
  - **Spec:** MinIO down mid-pipeline: jobs hold (storage_hold), resume after; no lost artifacts; error codes correct.
  - **AC:**
    - [ ] holds + resume green

- [ ] **P3-083** — DB restart drill
  - **Type:** test · **Area:** drills · **Est:** S · **Deps:** P3-082 · **Prio:** P1
  - **Spec:** Postgres restart: api reconnect, in-flight tx fail fast PF-5001, no corruption; worker retries.
  - **AC:**
    - [ ] recovery green

- [ ] **P3-084** — Drill runbook
  - **Type:** docs · **Area:** drills · **Est:** S · **Deps:** P3-083 · **Prio:** P1
  - **Spec:** How to run each drill (ops/drills/ scripts), expected outcomes, evidence to record.
  - **AC:**
    - [ ] runbook committed

---

## E3.10 — Pipeline observability (area: observability)

- [ ] **P3-085** — Stage histograms
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Per-stage duration histograms (detect/inpaint/encode/qc) by kind+tier; panels wired.
  - **AC:**
    - [ ] panels live

- [ ] **P3-086** — Queue depth gauge
  - **Type:** feat · **Area:** observability · **Est:** S · **Deps:** P3-085 · **Prio:** P1
  - **Spec:** Gauge per lane refreshed; alert rule wired.
  - **AC:**
    - [ ] gauge + alert green

- [ ] **P3-087** — Job failure alerts
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** P3-086 · **Prio:** P1
  - **Spec:** By stage + kind; runbook thresholds; test alert fires in staging sim.
  - **AC:**
    - [ ] alert fires

- [ ] **P3-088** — SSE connection gauges
  - **Type:** feat · **Area:** observability · **Est:** S · **Deps:** P3-087 · **Prio:** P1
  - **Spec:** Active conns, event lag; connection-leak detection (age histogram).
  - **AC:**
    - [ ] leak detectable

- [ ] **P3-089** — Pipeline trace exemplars
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** P3-088 · **Prio:** P1
  - **Spec:** OTel spans per stage with job_id exemplars; sampling config; trace-to-logs correlation doc.
  - **AC:**
    - [ ] correlation documented

- [ ] **P3-090** — Dashboards v2
  - **Type:** chore · **Area:** observability · **Est:** S · **Deps:** P3-089 · **Prio:** P1
  - **Spec:** Update grafana json: lanes, stages, video metrics, GC; provisioning reload documented.
  - **AC:**
    - [ ] dashboards updated

- [ ] **P3-091** — Observability runbook update
  - **Type:** docs · **Area:** observability · **Est:** S · **Deps:** P3-090 · **Prio:** P1
  - **Spec:** New panels/alerts documented in runbook observability section.
  - **AC:**
    - [ ] runbook updated

---

## E3.11 — Performance (area: perf)

- [ ] **P3-092** — Image perf targets
  - **Type:** feat · **Area:** perf · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Bench: 4K image job p95 <= 30s on 4 vCPU (NFR-2) verified; profile + fix ticket if missed.
  - **AC:**
    - [ ] target met with evidence

- [ ] **P3-093** — Video throughput target
  - **Type:** feat · **Area:** perf · **Est:** M · **Deps:** P3-092 · **Prio:** P1
  - **Spec:** >= 0.5x realtime 1080p CPU (NFR-3); profile hot path (likely per-frame detection - reduce via budget).
  - **AC:**
    - [ ] target met with evidence

- [ ] **P3-094** — Worker scale test
  - **Type:** feat · **Area:** perf · **Est:** M · **Deps:** P3-093 · **Prio:** P1
  - **Spec:** 3 replicas: throughput scales near-linearly, no queue storms; slots/lanes hold; evidence table.
  - **AC:**
    - [ ] scaling evidence recorded

- [ ] **P3-095** — DB under load
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P3-094 · **Prio:** P1
  - **Spec:** k6 burst: job create/status poll 30 min; slow-query log clean (< 50ms) per NFR-12; index fixes if needed.
  - **AC:**
    - [ ] slow log clean

- [ ] **P3-096** — Redis under load
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P3-095 · **Prio:** P1
  - **Spec:** SSE fanout + counters at simulated 10k conns (synthetic publisher); latency p99 < 5ms; capacity note.
  - **AC:**
    - [ ] capacity note recorded

- [ ] **P3-097** — Cache tuning
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P3-096 · **Prio:** P1
  - **Spec:** Entitlement/plan/flag caches hit-rate check; TTL adjust; singleflight verified under burst.
  - **AC:**
    - [ ] hit-rate improved

- [ ] **P3-098** — Cost-per-job model
  - **Type:** feat · **Area:** perf · **Est:** S · **Deps:** P3-097 · **Prio:** P1
  - **Spec:** CPU-seconds + storage-days + egress per job kind measured from bench; model doc (NFR-70); pricing sanity note.
  - **AC:**
    - [ ] model committed

- [ ] **P3-099** — Capacity plan
  - **Type:** docs · **Area:** perf · **Est:** S · **Deps:** P3-098 · **Prio:** P1
  - **Spec:** Jobs/day capacity at 1/2/3 workers, storage growth, redis sizing; scale decision table.
  - **AC:**
    - [ ] plan committed

---

## E3.12 — API polish (area: api)

- [ ] **P3-100** — Cursor pagination audit
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** All list endpoints cursor-based (Link header), no offset; tamper tests.
  - **AC:**
    - [ ] audit green

- [ ] **P3-101** — Filters + sort contracts
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P3-100 · **Prio:** P1
  - **Spec:** Documented filter sets per endpoint; precise validation errors; OpenAPI sync.
  - **AC:**
    - [ ] contracts tested

- [ ] **P3-102** — Rate limit tiers final
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P3-101 · **Prio:** P1
  - **Spec:** Per-plan/per-key/per-route table configured; headers verified; docs page.
  - **AC:**
    - [ ] table verified

- [ ] **P3-103** — Status polling coalescing
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P3-102 · **Prio:** P1
  - **Spec:** GET status cache 1s (redis) absorbing UI polling bursts; invalidation on events.
  - **AC:**
    - [ ] burst absorbed

- [ ] **P3-104** — OpenAPI sync check
  - **Type:** chore · **Area:** api · **Est:** S · **Deps:** P3-103 · **Prio:** P1
  - **Spec:** CI diff green; examples updated; API changelog since Phase 2.
  - **AC:**
    - [ ] diff green

- [ ] **P3-105** — API regression suite
  - **Type:** test · **Area:** api · **Est:** M · **Deps:** P3-104 · **Prio:** P1
  - **Spec:** Contract tests for every endpoint (status codes, shapes, headers) as living documentation.
  - **AC:**
    - [ ] suite green
