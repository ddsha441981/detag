//! Encode-stage benches (P0-097).
//!
//! What this measures TODAY: the real encode operation on the tier
//! fixtures — decode, then re-encode to JPEG (quality 95, 4:4:4, per
//! the quality policy's JPEG floor) and to PNG. Time per encode; the
//! output/input size ratio is recorded by the golden suite's
//! envelopes, not by this bench (criterion reports time, not bytes).
//!
//! What replaces it in Phase 2: the engine's own format-preserving
//! encoder (same shapes, same tiers).

#[path = "support.rs"]
mod support;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use image::ImageFormat;
use std::hint::black_box;
use std::io::Cursor;

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode");
    for (tier, file, _, _) in support::TIERS {
        let bytes = support::load_fixture(file);
        group.bench_function(format!("jpeg_q95/{tier}"), |b| {
            b.iter_batched(
                || support::decode_png(&bytes),
                |img| {
                    let mut out = Cursor::new(Vec::new());
                    let mut encoder =
                        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
                    encoder.encode_image(&img).expect("jpeg encodes");
                    black_box(out.into_inner().len());
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function(format!("png/{tier}"), |b| {
            b.iter_batched(
                || support::decode_png(&bytes),
                |img| {
                    let mut out = Cursor::new(Vec::new());
                    img.write_to(&mut out, ImageFormat::Png)
                        .expect("png encodes");
                    black_box(out.into_inner().len());
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(
    name = benches;
    config = support::criterion();
    targets = bench_encode
);
criterion_main!(benches);
