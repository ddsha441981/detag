//! Decode benches (P2-002): per-format decode of the tier fixtures
//! (png/jpeg/webp clean) through the engine's `decode_image` — the
//! sniffed kind forced, the path the intake will use.
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

criterion_group!(
    name = benches;
    config = support::criterion();
    targets = bench_decode
);
criterion_main!(benches);
