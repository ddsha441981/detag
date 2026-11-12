//! Inpaint-stage benches (P0-097).
//!
//! What this measures TODAY (scaffold): tile throughput — decode the
//! tier fixture, split it into 512×512 tiles and run a local
//! diffusion-like op (Gaussian blur, sigma 1.5) per tile, reporting
//! tiles/s. The blur is a PROXY for the LaMa inpaint pass: same
//! tile-sized memory access pattern, not the same math.
//!
//! What replaces it in Phase 2: the real tiled LaMa inpaint pass
//! (inpaint tiles/s per tier, per the benchmarks doc).

#[path = "support.rs"]
mod support;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use std::hint::black_box;

fn bench_inpaint_tiles(c: &mut Criterion) {
    let mut group = c.benchmark_group("inpaint");
    for (tier, file, width, height) in support::TIERS {
        let tiles = support::tile_grid(width, height, 512);
        let tile_count = tiles.len() as u64;
        let bytes = support::load_fixture(file);
        group.throughput(Throughput::Elements(tile_count));
        group.bench_function(format!("tiles/{tier}"), |b| {
            b.iter_batched(
                || support::decode_png(&bytes),
                |img| {
                    let rgb = img.to_rgb8();
                    for (x, y, w, h) in &tiles {
                        // Proxy for the inpaint pass: a local
                        // diffusion-like op on the tile's pixels.
                        let tile = image::imageops::crop_imm(&rgb, *x, *y, *w, *h).to_image();
                        let blurred = image::imageops::blur(&tile, 1.5);
                        black_box(blurred);
                    }
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
    targets = bench_inpaint_tiles
);
criterion_main!(benches);
