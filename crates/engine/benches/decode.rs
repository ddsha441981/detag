//! Decode benches (P2-002): per-format decode of the tier fixtures
//! (png/jpeg/webp clean) through the engine's `decode_image` — the
//! sniffed kind forced, the path the intake will use.
//!
//! P2-007 adds the budgeted benches to the same `decode` group:
//! the same tiers/formats through the budgeted path
//! (`decode_budgeted` with the default 512 MiB budget), so a future
//! regression in the budgeted decode's cost is visible against the
//! plain path. The budgeted rows land in the same `decode.json`
//! baseline (keys `budgeted_<fmt>_<tier>`).
//!
//! Baseline (first run, this ticket): the decode stage did not exist
//! before P2-002, so there is no before-number — these ARE the
//! baseline the next engine ticket compares against. TIFF/AVIF have
//! no committed fixtures yet, so no bench rows for them (their
//! timings come from the matrix test's encode/decode round-trips,
//! not from criterion).

#[path = "support.rs"]
mod support;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use detag_core::media::MediaKind;
use detag_engine::budget::{decode_budgeted, DecodeBudget};
use detag_engine::intake::decode_image;
use std::hint::black_box;

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");
    for (tier, _, _, _) in support::TIERS {
        for (fmt, ext, kind) in [
            ("png", "png", MediaKind::Png),
            ("jpg", "jpg", MediaKind::Jpeg),
            ("webp", "webp", MediaKind::WebP),
        ] {
            let bytes = support::load_fixture(&format!("img-{tier}-{fmt}-clean.{ext}"));
            group.bench_function(format!("{fmt}/{tier}"), |b| {
                b.iter_batched(
                    || bytes.clone(),
                    |bytes| black_box(decode_image(&bytes, kind).expect("fixture decodes")),
                    BatchSize::SmallInput,
                );
            });
        }
    }
    group.finish();
}

fn bench_decode_budgeted(c: &mut Criterion) {
    // Same group as the plain path, with a `budgeted/` prefix on the
    // bench names: criterion lays groups out as top-level directories
    // under target/criterion/ (`decode_budgeted/` would be its own
    // dir), and bench_baseline.py keys baselines by that first
    // component — it only knows the five binary-named groups, so a
    // separate group would silently drop the rows (found the hard way:
    // the first bench run recorded the budgeted timings under
    // target/criterion/decode_budgeted/ and decode.json stayed at 9
    // rows). One group, prefixed names, the script untouched.
    let mut group = c.benchmark_group("decode");
    for (tier, _, _, _) in support::TIERS {
        for (fmt, ext, kind) in [
            ("png", "png", MediaKind::Png),
            ("jpg", "jpg", MediaKind::Jpeg),
            ("webp", "webp", MediaKind::WebP),
        ] {
            let bytes = support::load_fixture(&format!("img-{tier}-{fmt}-clean.{ext}"));
            group.bench_function(format!("budgeted/{fmt}/{tier}"), |b| {
                b.iter_batched(
                    || bytes.clone(),
                    |bytes| {
                        black_box(
                            decode_budgeted(&bytes, kind, DecodeBudget::default())
                                .expect("fixture within budget"),
                        )
                    },
                    BatchSize::SmallInput,
                );
            });
        }
    }
    group.finish();
}

criterion_group!(
    name = benches;
    config = support::criterion();
    targets = bench_decode, bench_decode_budgeted
);
criterion_main!(benches);
