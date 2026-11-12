# Quality Policy — the no-degradation doctrine

This is the most important product requirement. "Quality decrease nahi honi chahiye" is implemented as tested, machine-verifiable gates — not a hope.

## 1. Doctrine

1. Only pixels inside the removal mask may change. Everything else must survive bit-exact or within the metric floors below.
2. Resolution is never changed. Upscaling, downscaling, cropping to "help" removal are forbidden.
3. Format is preserved. JPEG stays JPEG, PNG stays PNG, MP4 stays MP4. Container/codec swaps only when the source codec is unsupported, and then at the highest-quality settings (logged in report).
4. Every job carries a quality report. A job that cannot pass gates fails as `quality_failed` with the report attached — the user sees the truth; we never silently degrade.
5. Speed can be traded for quality, never the reverse. Any "fast mode" is a separate, off-by-default flag with its own (still strict) floors.

## 2. Image gates

| Metric | Floor | Measured on |
| --- | --- | --- |
| SSIM (windowed, luminance) | >= 0.98 | unmasked pixels only (mask complement) |
| PSNR | >= 38 dB | unmasked pixels only |
| Resolution | identical | whole frame |
| ICC profile | preserved | binary compare of embedded profile |
| Untouched-pixel check (PNG) | bit-exact | hash of unmasked region after roundtrip |
| JPEG re-encode quality | q >= 95, 4:4:4 chroma | encoder settings + report |
| WebP re-encode quality | q >= 90 | encoder settings + report |

Metric implementation notes: compute SSIM/PSNR between input and output restricted to the mask complement. Reference implementations are validated against known-good vectors in the golden suite (`05-QUALITY/02-quality-benchmarks.md`).

## 3. Video gates

| Metric | Floor | Measured on |
| --- | --- | --- |
| VMAF (libvmaf, default model) | >= 93 | full stream, or sampled per scene when runtime-bound |
| MS-SSIM fallback (if libvmaf unavailable) | >= 0.97 | sampled frames (keyframes + every 30th) |
| Resolution / fps | identical | container + stream properties |
| Duration | within 1 frame | stream properties |
| Audio | stream-copy (`-c:a copy`), bit-exact | hash compare of audio stream |
| Video codec | H.264 (same as source) or ADR-approved alternative | ffprobe |
| Encode settings | CRF <= 16, preset slow, yuv420p | encoder log in report |
| Edited-region check | only mask regions differ | per-segment diff sampled frames |

`metadata_only` mode: full A/V stream copy, container metadata rewritten only — this mode is truly lossless and is the answer for "sirf metadata hatana hai".

## 4. Gate flow

```
job stage: qc
  ├─ metrics computed on output vs input (masked)
  ├─ pass  → succeeded (+ report)
  ├─ fail  → auto-retry once with stronger plan:
  │           hi-res tiled LaMa + wider feather + color-match pass
  │           (video: slower preset + lower CRF, sampled VMAF full)
  │    ├─ pass → succeeded (+ report, note "retried")
  │    └─ fail → quality_failed (+ report, user-facing reason, no output download by default;
  │              admin toggle can allow "accept anyway" per job, decision audited)
```

## 5. Quality report schema (v1)

```json
{
  "engine_version": "engine-0.3.1",
  "models": { "detector": "dbnet-det-v2", "inpainter": "lama-fp16-v1" },
  "input": { "format": "jpeg", "width": 4032, "height": 3024, "icc": true },
  "output": { "format": "jpeg", "width": 4032, "height": 3024, "jpeg_quality": 95 },
  "metrics": { "ssim_unmasked": 0.991, "psnr_unmasked_db": 41.2, "gate": "pass" },
  "encode": { "color_mode": "444", "estimated_source_q": 92 },
  "regions": [ { "id": 1, "type": "badge_template", "confidence": 0.94, "pixels": 18234 } ],
  "retried": false,
  "determinism_seed": "a1b2c3"
}
```

Video reports add `vmaf`, `audio_hash_match`, `duration_frames_delta`, per-segment table.

## 6. Golden media suite (regression floor)

The suite in `05-QUALITY/02-quality-benchmarks.md` must run on every engine change. Any metric drop beyond thresholds requires either a fix or an ADR accepting the regression with numbers. This suite is the definition of "quality did not decrease" over time.

## 7. Defaults the agent may tune (with ADR)

- Mask feather width, dilation radius, tile size, blend strategy — tunable, but any change must keep the golden suite green.
- Model versions are pinned; upgrades are tickets with before/after benchmark reports.

## 8. What this policy forbids

- Shipping output that failed gates (unless admin-approved override, audited).
- Burying metrics: report must always be visible in UI and API.
- Metric fudging: thresholds live in config, changing them requires an ADR and golden-suite evidence.
