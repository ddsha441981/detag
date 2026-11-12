# Golden fixtures

Deterministic, synthesized test media for the engine golden suite
(`05-QUALITY/01-test-strategy.md`). Generated, never hand-edited:
`just fixtures-gen` regenerates everything; `just fixtures-check`
fails on any drift from the committed files.

## Layout

| Path | Contents |
| --- | --- |
| `images/` | 36 images: 12 base scenes × 3 variants (clean / badge / text) |
| `videos/` | 3 videos (MP4, H.264 + AAC audio) |
| `expected/` | One expected-metrics envelope per fixture (`<stem>.json`) |
| `SHA256SUMS` | sha256sum table over every file above |

## The image matrix — "24 images" means 24 marked inputs

The strategy lists the matrix as {png, jpeg, webp} × {512², 1920×1080,
3840×2160, 8000×8000} × {badge mark, text mark, clean control} but
labels it "24 images". 3 × 4 × 3 = 36, not 24. The reading used here
(see ISSUES.md, ASSUMPTION-013):

- **24 marked images** = 3 formats × 4 sizes × 2 marks (badge, text).
  These are the test inputs.
- **12 clean controls** = the same 12 scenes unmarked. They are the
  SSIM/PSNR reference the golden suite compares the engine's output
  against, so they are fixtures too — just not counted in the "24".

File names: `img-<size>-<format>-<mark>.<ext>`, e.g.
`img-4k-jpg-text.jpg`, `img-8k-png-clean.png`.

## Videos

| File | Contents |
| --- | --- |
| `vid-1080p-30s-badge.mp4` | 1920×1080, 30 s, 30 fps, badge + 440 Hz sine audio |
| `vid-720p-10s-clean.mp4` | 1280×720, 10 s, 30 fps, clean + 880 Hz sine audio |
| `vid-vertical-4s-badge.mp4` | 1080×1920, 4 s, 30 fps, badge + 440 Hz sine audio |

Source is ffmpeg's deterministic `testsrc2`; the badge is a
drawbox + drawtext overlay. All three carry audio so the
audio-stream-copy gate is testable on every video.

## Envelopes (`expected/*.json`)

Each envelope records the fixture's identity (`file`, `format`,
dimensions, `mark`, `seed`, `source: "synthesized"`), its `sha256`
and `bytes`, the toolchain versions that produced it, and the
quality-policy floors the **engine's output** must meet on it
(SSIM ≥ 0.98 / PSNR ≥ 38 dB unmasked for images; VMAF ≥ 93 for
video — `02-REQUIREMENTS/04-quality-policy.md`). `measured` is
`null` until the Phase 2 golden suite (`crates/engine/tests/golden/`)
fills it with the engine's actual metrics.

## Reproducibility contract

`just fixtures-gen` run twice on the same toolchain produces
bit-identical files (AC: "gen twice = same shas"). What makes that
true, and what breaks it:

- **Fixed seeds** — scenes derive from `BASE_SEED` in
  `scripts/fixtures_gen.py`; numpy PCG64 and Python's Mersenne
  Twister are both stream-stable across versions.
- **Fixed encoder settings** — Pillow quality/subsampling/method;
  libx264 `-threads 1` (frame threading can vary output) and a
  pinned MP4 `creation_time` (ffmpeg otherwise stamps wall-clock
  time into the file).
- **Same toolchain versions** — recorded in every envelope and in
  the `SHA256SUMS` header. Pillow and numpy are pip-pinnable, so
  `just fixtures-check` fails loudly when they differ (regenerate
  and commit the new shas instead of overriding the check).
- **ffmpeg differs → video bytes differ.** libx264 output is
  encoder-version-dependent, and CI runners ship a different
  ffmpeg than this VM. In that case `just fixtures-check` skips the
  video byte compare and verifies stream properties via ffprobe
  (codec, resolution, fps, duration, audio present) instead; image
  bytes are still compared exactly. Full byte-reproducibility for
  videos requires the recorded ffmpeg version.

## Regenerating

```sh
just fixtures-gen    # regenerate in place (updates SHA256SUMS)
just fixtures-check  # regenerate to temp, compare, exit 1 on drift
```

The 8000×8000 bases are the slow leg (numpy scene build + PNG
deflate); the 30 s video is the other slow leg (single-threaded
x264 by design). A full regen takes a few minutes.
