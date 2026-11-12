# Quality Benchmarks & Calibration

## Floors (mirror of quality-policy; single source for CI thresholds file `crates/engine/src/metrics/thresholds.rs`)

| Metric | Floor |
| --- | --- |
| SSIM unmasked (images) | >= 0.98 |
| PSNR unmasked (images) | >= 38 dB |
| PNG untouched pixels | bit-exact |
| VMAF (video, libvmaf default model) | >= 93 |
| MS-SSIM fallback (video) | >= 0.97 sampled |
| Audio stream | bit-exact copy |
| Resolution/fps/duration | identical (duration ±1 frame) |

## How metrics are computed

- SSIM/PSNR: input vs output restricted to mask complement (mask dilated by policy radius for fairness). Implementation validated against reference vectors committed under `fixtures/metric-vectors/` (known inputs with expected SSIM to 4 decimals, cross-checked against an independent implementation during Phase 2 calibration).
- VMAF: ffmpeg with libvmaf (`-lavfi libvmaf`), JSON log parsed. If libvmaf missing at runtime → MS-SSIM fallback + report notes fallback used.
- Audio: decode audio stream, hash PCM; compare input vs output hashes.

## Benchmark harness

`just bench` (criterion) tracks, per resolution tier:
- detect (template pass, text-det pass) ms
- inpaint tiles/s
- encode ms + output size ratio vs input
- full image job p50/p95 (512², 1080p, 4K)
- video processing factor vs realtime (1080p fixture)

Nightly bench job appends to `09-STATE/PROGRESS.md` bench snapshot. Regression > 10% on any benchmark without an ADR = fix ticket next run.

## Calibration procedure (once, Phase 2; repeated on threshold change)

1. Run golden suite with current thresholds; record pass rate (target >= 90% first-try).
2. Compute metric distributions; verify floors sit above p10 of passing jobs by margin (>= 0.02 SSIM, >= 3 dB PSNR, >= 5 VMAF) so gates are meaningful, not vacuous.
3. If floors fail the margin test → ADR proposing floor adjustment with distributions; owner-visible.

## Regression protocol (every engine change)

1. `just bench` before change (main) and after (branch).
2. Golden suite: all envelopes green.
3. Commit body table: metric → before → after → delta; bench: before → after.
4. Threshold movement requires ADR + calibration rerun. No exceptions.

## Model upgrade protocol

New model version = ticket with: license audit, sha256 pin, golden suite before/after, bench before/after, quality report diffs on 5 real-world-ish fixtures, ADR. Rollback = registry pin revert (one commit).
