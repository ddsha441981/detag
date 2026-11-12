//! Shared helpers for the engine benches (P0-097). Included with
//! `#[path = "support.rs"]` — bench targets cannot share a library
//! crate without one.
//!
//! Not every bench binary uses every helper (detect uses the pack
//! helpers, inpaint the tile grid, full_job the hasher) — the
//! unused-function warnings would otherwise fire per binary.
//! Unused helpers are intentional.
#![allow(dead_code)]

use image::DynamicImage;
use std::path::PathBuf;
use std::time::Duration;

/// Resolution tiers (benchmarks doc): the fixture each tier benches
/// against (the clean PNG control, so the bench measures the stage,
/// not a mark).
pub const TIERS: [(&str, &str, u32, u32); 3] = [
    ("512", "img-512-png-clean.png", 512, 512),
    ("1080p", "img-1080p-png-clean.png", 1920, 1080),
    ("4k", "img-4k-png-clean.png", 3840, 2160),
];

/// Fast-by-default criterion config: 10 samples, 3 s measurement.
/// A full run (100 samples) is `cargo bench -- --sample-size 100
/// --measurement-time 10`; the scaffold defaults keep `just bench`
/// under a minute so it stays usable in the nightly loop.
pub fn criterion() -> criterion::Criterion {
    criterion::Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(3))
        .warm_up_time(Duration::from_secs(1))
}

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

pub fn fixture_path(name: &str) -> PathBuf {
    repo_root().join("fixtures").join("images").join(name)
}

pub fn load_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).expect("fixture must exist — run `just fixtures-gen`")
}

pub fn decode_png(bytes: &[u8]) -> DynamicImage {
    image::load_from_memory(bytes).expect("fixture PNG decodes")
}

/// Non-overlapping tile grid for a frame: (x, y, w, h), edge tiles
/// clipped. The real tiler (Phase 2) adds overlap + feathering;
/// this is the grid arithmetic the bench measures, not the product
/// tiler.
pub fn tile_grid(width: u32, height: u32, tile: u32) -> Vec<(u32, u32, u32, u32)> {
    let mut tiles = Vec::new();
    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            tiles.push((x, y, tile.min(width - x), tile.min(height - y)));
            x += tile;
        }
        y += tile;
    }
    tiles
}

/// Sha256 of bytes (hex). The engine's `verify_models` path hashes
/// files the same way (streamed); the bench hashes in-memory bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
