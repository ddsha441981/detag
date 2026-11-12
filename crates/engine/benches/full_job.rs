//! Full-job benches (P0-097).
//!
//! What this measures TODAY (scaffold): the chained stages on the
//! tier fixtures — decode → blur tiles (inpaint proxy) → JPEG
//! re-encode — plus the video input-prep leg (load the 1080p video
//! fixture bytes + sha256). The chain is the shape the real full-job
//! bench will keep; each stage's body is replaced by its Phase 2
//! implementation.
//!
//! What replaces it in Phase 2: detect → inpaint → encode → QC on
//! the fixture media (full image job p50/p95 per tier; video
//! processing factor vs realtime on the 1080p fixture, per the
//! benchmarks doc). The video factor is NOT measured here — it needs
//! the real video pipeline, not a byte hash.

#[path = "support.rs"]
mod support;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use std::hint::black_box;
use std::io::Cursor;

fn bench_full_job(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_job");
    for (tier, file, width, height) in support::TIERS {
        let tiles = support::tile_grid(width, height, 512);
        let bytes = support::load_fixture(file);
        group.bench_function(format!("image/{tier}"), |b| {
            b.iter_batched(
                || support::decode_png(&bytes),
                |img| {
                    let rgb = img.to_rgb8();
                    for (x, y, w, h) in &tiles {
                        let tile = image::imageops::crop_imm(&rgb, *x, *y, *w, *h).to_image();
                        black_box(image::imageops::blur(&tile, 1.5));
                    }
                    let mut out = Cursor::new(Vec::new());
                    let mut encoder =
                        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
                    encoder.encode_image(&img).expect("jpeg encodes");
                    black_box(out.into_inner().len());
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_video_input_prep(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_job");
    group.bench_function("video_input_prep/1080p", |b| {
        b.iter(|| {
            let bytes = std::fs::read(
                support::repo_root()
                    .join("fixtures")
                    .join("videos")
                    .join("vid-1080p-30s-badge.mp4"),
            )
            .expect("video fixture exists");
            black_box(support::sha256_hex(&bytes));
        });
    });
    group.finish();
}

criterion_group!(
    name = benches;
    config = support::criterion();
    targets = bench_full_job, bench_video_input_prep
);
criterion_main!(benches);
