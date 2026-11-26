//! EXIF fuzz target (P2-003, FR-U-06): feed arbitrary bytes to
//! [`parse_exif`] as every image kind and prove it never panics —
//! any result (a summary or a typed error) is fine, a panic or an
//! ASan abort is a failure. Runs nightly (`.github/workflows/
//! nightly.yml`, `fuzz` job); the seed corpus lives in
//! `fuzz/corpus/exif_parse/` (a JPEG + a TIFF carrying real EXIF).
//!
//! Run locally with:
//! `cargo +nightly fuzz run exif_parse -- -max_total_time=60`

#![no_main]

use detag_core::media::MediaKind;
use detag_engine::exif::parse_exif;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Every image kind the parser accepts, on the same bytes: the
    // container detection inside the library must agree (or fail
    // typed) for each one. Video kinds are excluded — they fail
    // before the library is reached.
    for kind in [
        MediaKind::Jpeg,
        MediaKind::Png,
        MediaKind::WebP,
        MediaKind::Tiff,
        MediaKind::Avif,
    ] {
        let _ = parse_exif(data, kind);
    }
});
