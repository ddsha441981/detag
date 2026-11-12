# Engine benches

Criterion benches for the engine pipeline (P0-097), per resolution
tier (512², 1080p, 4K). Run: `just bench`.

## What is benched

| Bench file | Measures today |
| --- | --- |
| `detect.rs` | Detect prologue: decode tier fixture + resize to the 512-wide match scale; template-pack load + sha256-verify. |
| `inpaint.rs` | Tile throughput (tiles/s): decode + 512×512 tiles + Gaussian blur per tile. |
| `encode.rs` | Re-encode per tier: JPEG q95 (4:4:4) + PNG, time per encode. |
| `full_job.rs` | Chained stages per tier (decode → blur tiles → JPEG) + video input prep (load 1080p fixture + sha256). |

## Scaffold status — read before quoting numbers

The engine is a Phase 2 placeholder, so these benches measure the
pipeline's **scaffold operations**, not the product stages:

- **Detect:** the NCC template-match kernel is NOT benched — the
  naive kernel is too slow to run at full res, and faking it would
  produce meaningless numbers. The bench measures the prologue the
  real matcher will keep. The kernel bench lands with the Phase 2
  matcher.
- **Inpaint:** the blur is a **proxy** for the LaMa pass (same
  tile-sized memory pattern, not the same math). Tiles/s here is a
  harness number, not an inpainting speed.
- **Encode:** real operation (decode → re-encode on the real
  fixtures). The Phase 2 encoder replaces the `image`-crate call
  with the engine's format-preserving encoder; shapes stay the same.
- **Full job:** the chained shape is real; each stage's body is
  replaced by its Phase 2 implementation. The video processing
  factor (vs realtime) is NOT measured here — it needs the real
  video pipeline.

## Baselines

`just bench` saves a `main` baseline per bench (criterion's own
storage under `target/criterion/`, gitignored). The committed
baselines live in `baselines/` as one `estimates.json` summary per
bench, copied there by `just bench` via `scripts/bench_baseline.py`
after the run — a committed baseline is the record the next run
compares against, so it is committed with the code that produced it
like any other generated artifact. The script only copies the
estimate summaries (mean/median), never raw samples.

## Running

```sh
just bench            # fast defaults: 10 samples, 3 s measurement per bench
cargo bench -p detag-engine -- --sample-size 100 --measurement-time 10   # full run
cargo bench -p detag-engine --bench detect   # one stage
```

## Regression rule (benchmarks doc)

Regression > 10% on any benchmark without an ADR = fix ticket next
run. Compare with `cargo bench -- --baseline main` (criterion
compares against the saved `main` baseline).
