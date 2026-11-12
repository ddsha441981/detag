//! Detect-stage benches (P0-097).
//!
//! What this measures TODAY (scaffold): the detect prologue — decode
//! the tier fixture + resize it to the 512-wide match scale (the
//! multi-scale matcher works downscaled first), and load + sha256
//! -verify the production template pack (the matcher verifies the
//! pack before matching; FR-D-01).
//!
//! What replaces it in Phase 2: the NCC template-match kernel itself
//! (template pass + text-det pass, ms per tier, per the benchmarks
//! doc). The naive NCC kernel is too slow to bench at full res with
//! the current `imageproc` implementation, so it is NOT faked here.

#[path = "support.rs"]
mod support;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use image::imageops::FilterType;
use serde_json::Value;
use std::hint::black_box;

fn bench_detect_prologue(c: &mut Criterion) {
    let mut group = c.benchmark_group("detect");
    for (tier, file, _, _) in support::TIERS {
        let bytes = support::load_fixture(file);
        group.bench_function(format!("prologue/{tier}"), |b| {
            b.iter_batched(
                || bytes.clone(),
                |bytes| {
                    let img = support::decode_png(&bytes);
                    // Multi-scale prologue: downscale to the 512-wide
                    // match scale (the matcher works up from here).
                    let small = image::imageops::resize(
                        &img,
                        512,
                        (512 * img.height()) / img.width(),
                        FilterType::Triangle,
                    );
                    black_box(small);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_pack_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("detect");
    group.bench_function("pack_verify", |b| {
        b.iter(|| {
            let manifest_path = support::repo_root()
                .join("templates")
                .join("prod")
                .join("manifest.json");
            let manifest: Value = serde_json::from_str(
                &std::fs::read_to_string(manifest_path).expect("prod manifest exists"),
            )
            .expect("manifest parses");
            let templates = manifest["templates"].as_array().expect("templates array");
            let mut verified = 0;
            for entry in templates {
                let file = entry["file"].as_str().expect("file name");
                let bytes = std::fs::read(
                    support::repo_root()
                        .join("templates")
                        .join("prod")
                        .join(file),
                )
                .expect("template exists");
                assert_eq!(
                    support::sha256_hex(&bytes),
                    entry["sha256"].as_str().unwrap()
                );
                verified += 1;
            }
            black_box(verified);
        });
    });
    group.finish();
}

criterion_group!(
    name = benches;
    config = support::criterion();
    targets = bench_detect_prologue, bench_pack_verify
);
criterion_main!(benches);
