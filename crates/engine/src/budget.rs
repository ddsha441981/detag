//! Budgeted decode — the P2-007 memory-budget path (FR-U-04).
//!
//! [`decode_budgeted`] is the decode the intake will use once the
//! suite (P2-009) wires it: same bytes-in / pixels-out contract as
//! [`crate::intake::decode_image`], but the decoded size is decided
//! BEFORE any pixel is allocated. The decoder's OWN number is the
//! verdict — [`image::ImageDecoder::total_bytes`] (width × height ×
//! bytes-per-pixel, saturating `u64`, the trait default no decoder
//! overrides; verified against the `image` 0.25.10 registry sources)
//! — so the estimate is what the decoder itself would allocate, not
//! a re-derivation that could drift from it. Over budget, the image
//! fails as [`crate::intake::DecodeError::BudgetExceeded`] (PF-2007,
//! 413) and NOTHING is decoded.
//!
//! # Why not the upstream `Limits` mechanism
//!
//! `image::io::Limits::max_alloc` was considered and REJECTED for the
//! verdict AND as a backstop: it is NON-STRICT ("some decoders may
//! ignore it") and working buffers count toward it, so setting
//! `max_alloc = budget` could make a legitimate at-budget decode fail
//! inside the decoder with a generic `ImageError::Limits` — an exact
//! verdict turned flaky. Same reasoning as P2-006's rejection of
//! `Limits` for the dimension verdict, one step further: this module
//! does the arithmetic itself, on the decoder's own number, and the
//! decoder runs with its defaults.
//!
//! # The numbers (ADR-0076)
//!
//! * [`DecodeBudget::default`] = 512 MiB (536,870,912 B). It admits
//!   every image P2-006 admits at its worst case — 8000×8000 RGBA16
//!   = 512,000,000 B < budget — and it EQUALS the `image` crate's
//!   default `Limits::max_alloc`, so the budget and the upstream
//!   default agree instead of fighting.
//! * [`RSS_CEILING_BYTES`] = 1 GiB (1,073,741,824 B): ~2× the budget,
//!   ~5× the 8k RGB8 pixel buffer of 192 MB. A decode that slipped
//!   past the estimate — e.g. a 20000×20000 RGB8 image at 1.2 GB of
//!   pixels — breaches it, so the ceiling is the line the estimate
//!   must never let a decode cross.
//!
//! # What this does NOT do
//!
//! No caller yet: the intake suite (P2-009) wires `decode_budgeted`
//! after the guards — parse-only shape, no fake wiring, the
//! P2-003–P2-006 precedent. No tiling: a huge image inside budget
//! still decodes whole ("tile-aware later" per the ticket spec).
//! The ceiling is an ESTIMATE-based guard, not a measured-RSS gate:
//! the verdict caps the decoder's pixel buffer, but working buffers
//! beyond it are not counted — see ADR-0076 §5 for the honest limit.

use crate::intake::{DecodeError, DecodedImage};
use detag_core::media::MediaKind;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use std::io::Cursor;

/// The RSS ceiling a decode must never cross: 1 GiB (1,073,741,824 B).
///
/// ~2× the default budget, ~5× the 8k RGB8 pixel buffer (192 MB).
/// It is a constant the tests pin, not a measured value: anything
/// whose ESTIMATED decoded size exceeds it is necessarily over the
/// (smaller) budget too, so it is rejected before any pixel is
/// allocated — no decode can allocate past the ceiling as estimated.
/// Working buffers beyond the pixel buffer are NOT counted; the
/// ceiling is an estimate-based guard, not a measured-RSS gate
/// (ADR-0076 §5).
pub const RSS_CEILING_BYTES: u64 = 1_073_741_824;

/// A decode memory budget: the most decoded bytes a single image may
/// occupy. The estimate is the decoder's own `total_bytes()` —
/// width × height × bytes-per-pixel — so the verdict is exact for
/// the buffer the decoder is about to allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeBudget {
    /// Maximum decoded size in bytes.
    pub max_bytes: u64,
}

impl DecodeBudget {
    /// The default budget: 512 MiB (536,870,912 B). Admits every
    /// image P2-006 admits at its worst case (8000×8000 RGBA16 =
    /// 512,000,000 B) and equals the `image` crate's default
    /// `Limits::max_alloc`.
    pub const DEFAULT_MAX_BYTES: u64 = 536_870_912;

    /// A budget of `max_bytes` decoded bytes. `0` rejects any
    /// non-empty input — fail-closed, the guards precedent
    /// (`max_bytes = 0` in `check_intake_guards`).
    pub const fn new(max_bytes: u64) -> Self {
        Self { max_bytes }
    }
}

impl Default for DecodeBudget {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_BYTES)
    }
}

/// Decode `bytes` (sniffed as `kind` by P2-001) under `budget`.
///
/// The format is forced on the decoder — the bytes are never
/// re-guessed — so a JPEG passed with `kind == MediaKind::Png`
/// fails here instead of decoding as something else (magic wins,
/// end to end). Empty input fails as [`DecodeError::EmptyInput`];
/// video kinds fail as [`DecodeError::NotAnImage`]; a kind whose
/// cargo feature is off fails as [`DecodeError::FeatureDisabled`];
/// a header that cannot be read fails as [`DecodeError::Image`]
/// (fail closed — the size cannot be proven within budget).
///
/// The budget verdict comes from the decoder's OWN
/// `total_bytes()`, read from the header before decoding: over
/// budget fails as [`DecodeError::BudgetExceeded`] (PF-2007, 413)
/// WITHOUT decoding — the error carries the header's dims, the
/// colour type, the estimate, and the budget, so the caller can
/// report exactly what was refused and why. At or under budget the
/// image decodes exactly as [`crate::intake::decode_image`] would
/// (same `DynamicImage::from_decoder` path, 16-bit stays 16-bit).
pub fn decode_budgeted(
    bytes: &[u8],
    kind: MediaKind,
    budget: DecodeBudget,
) -> Result<DecodedImage, DecodeError> {
    if bytes.is_empty() {
        return Err(DecodeError::EmptyInput);
    }
    let format = match kind {
        MediaKind::Png => ImageFormat::Png,
        MediaKind::Jpeg => ImageFormat::Jpeg,
        MediaKind::WebP => ImageFormat::WebP,
        #[cfg(feature = "tiff")]
        MediaKind::Tiff => ImageFormat::Tiff,
        #[cfg(not(feature = "tiff"))]
        MediaKind::Tiff => return Err(DecodeError::FeatureDisabled(kind, "tiff")),
        #[cfg(feature = "avif")]
        MediaKind::Avif => ImageFormat::Avif,
        #[cfg(not(feature = "avif"))]
        MediaKind::Avif => return Err(DecodeError::FeatureDisabled(kind, "avif")),
        MediaKind::Mp4 | MediaKind::Mkv | MediaKind::Webm => {
            return Err(DecodeError::NotAnImage(kind));
        }
    };
    // Header only so far: into_decoder() constructs the decoder and
    // reads the header — no pixel data is allocated. The estimate is
    // the decoder's own number (width × height × bytes-per-pixel),
    // so the verdict is exact for the buffer about to be allocated.
    let decoder = ImageReader::with_format(Cursor::new(bytes), format).into_decoder()?;
    let (width, height) = decoder.dimensions();
    let color = decoder.color_type();
    let estimated_bytes = decoder.total_bytes();
    if estimated_bytes > budget.max_bytes {
        return Err(DecodeError::BudgetExceeded {
            width,
            height,
            color,
            estimated_bytes,
            budget_bytes: budget.max_bytes,
        });
    }
    let image = DynamicImage::from_decoder(decoder)?;
    Ok(DecodedImage {
        image,
        format: kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intake::decode_image;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    #[cfg(feature = "tiff")]
    use image::codecs::tiff::TiffEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::ColorType;
    use image::{DynamicImage, ImageBuffer, Rgb};

    /// Committed bomb fixtures (`tests/fixtures/bombs/`, generated by
    /// `gen_bomb_fixtures.py` — the P2-006 precedent): real PNG bomb
    /// 20000×20000 (~1.2 GB decoded), one-over 8001×8001 PNG bomb,
    /// header-only PNG bomb (65 bytes, no pixel data), hand-crafted
    /// JPEG / WebP / TIFF 20000×20000 header bombs.
    const BOMB_PNG_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-png-20000x20000.png");
    const BOMB_PNG_8001: &[u8] = include_bytes!("../tests/fixtures/bombs/bomb-png-8001x8001.png");
    const BOMB_PNG_HEADER_ONLY: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-png-header-only-20000x20000.png");
    const BOMB_JPEG_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-jpeg-20000x20000.jpg");
    const BOMB_WEBP_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-webp-20000x20000.webp");
    const BOMB_TIFF_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-tiff-20000x20000.tiff");

    /// The committed 8000×8000 clean PNG (P2-006 boundary fixture):
    /// the largest image the dimension guard admits, 192 MB decoded
    /// as RGB8 — the real "largest admitted image" for the budget.
    const IMG_8K_PNG_CLEAN: &[u8] = include_bytes!("../../../fixtures/images/img-8k-png-clean.png");

    /// Deterministic 8-bit RGB gradient (the intake-test helper):
    /// every pixel differs from its neighbours.
    fn gradient_rgb8(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([
                (x.wrapping_mul(7).wrapping_add(y.wrapping_mul(3)) % 256) as u8,
                (x.wrapping_mul(3).wrapping_add(y.wrapping_mul(11)) % 256) as u8,
                (x.wrapping_add(y).wrapping_mul(5) % 256) as u8,
            ])
        })
    }

    fn encode_png8(img: &DynamicImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_with_encoder(PngEncoder::new(&mut buf))
            .expect("png encode");
        buf
    }

    fn encode_jpeg(img: &DynamicImage, quality: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_with_encoder(JpegEncoder::new_with_quality(&mut buf, quality))
            .expect("jpeg encode");
        buf
    }

    fn encode_webp_lossless(img: &DynamicImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_with_encoder(WebPEncoder::new_lossless(&mut buf))
            .expect("webp lossless encode");
        buf
    }

    #[cfg(feature = "tiff")]
    fn encode_tiff(img: &DynamicImage) -> Vec<u8> {
        // TiffEncoder needs Write + Seek; a Vec alone does not seek.
        let mut cursor = std::io::Cursor::new(Vec::new());
        img.write_with_encoder(TiffEncoder::new(&mut cursor))
            .expect("tiff encode");
        cursor.into_inner()
    }

    #[test]
    fn default_budget_value() {
        assert_eq!(DecodeBudget::default().max_bytes, 536_870_912);
        assert_eq!(DecodeBudget::DEFAULT_MAX_BYTES, 512 * 1024 * 1024);
        // Admits every image P2-006 admits at its worst case:
        // 8000x8000 RGBA16 = 512,000,000 B < budget.
        assert!(8000u64 * 8000 * 8 < DecodeBudget::default().max_bytes);
    }

    #[test]
    fn rss_ceiling_bounds_the_budget() {
        // ~2x the default budget...
        assert_eq!(RSS_CEILING_BYTES, 2 * DecodeBudget::default().max_bytes);
        // ...and ~5x the 8k RGB8 pixel buffer (192 MB), the largest
        // single-channel buffer the dimension guard admits. These two
        // are constant arithmetic, so they are pinned at compile time
        // (clippy::assertions_on_constants) rather than at run time.
        const { assert!(RSS_CEILING_BYTES > 5 * (8000u64 * 8000 * 3)) };
        // The worst case the budget admits (8000x8000 RGBA16) sits
        // under the ceiling too — the ceiling is never the binding
        // limit for a legitimate image, only for a bomb.
        const { assert!(8000u64 * 8000 * 8 < RSS_CEILING_BYTES) };
    }

    #[test]
    fn small_png_decodes_within_budget() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(64, 48));
        let bytes = encode_png8(&img);
        let decoded = decode_budgeted(&bytes, MediaKind::Png, DecodeBudget::default())
            .expect("small png within budget");
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
        assert_eq!(decoded.color(), ColorType::Rgb8);
        // Same pixels decode_image would produce.
        let plain = decode_image(&bytes, MediaKind::Png).expect("plain decode");
        assert_eq!(
            decoded.image.as_rgb8().unwrap().as_raw(),
            plain.image.as_rgb8().unwrap().as_raw()
        );
    }

    #[test]
    fn small_jpeg_decodes_within_budget() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(64, 48));
        let bytes = encode_jpeg(&img, 90);
        let decoded = decode_budgeted(&bytes, MediaKind::Jpeg, DecodeBudget::default())
            .expect("small jpeg within budget");
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }

    #[test]
    fn small_webp_decodes_within_budget() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(64, 48));
        let bytes = encode_webp_lossless(&img);
        let decoded = decode_budgeted(&bytes, MediaKind::WebP, DecodeBudget::default())
            .expect("small webp within budget");
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn small_tiff_decodes_within_budget() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(64, 48));
        let bytes = encode_tiff(&img);
        let decoded = decode_budgeted(&bytes, MediaKind::Tiff, DecodeBudget::default())
            .expect("small tiff within budget");
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }

    #[test]
    fn sixteen_bit_png_stays_sixteen_bit() {
        let buf: ImageBuffer<Rgb<u16>, Vec<u16>> = ImageBuffer::from_fn(32, 24, |x, y| {
            Rgb([
                ((x * 257) % 65536) as u16,
                ((y * 131) % 65536) as u16,
                32768,
            ])
        });
        let img = DynamicImage::ImageRgb16(buf);
        let bytes = encode_png8(&img);
        let decoded = decode_budgeted(&bytes, MediaKind::Png, DecodeBudget::default())
            .expect("small 16-bit png within budget");
        assert!(decoded.is_16_bit());
        assert_eq!(decoded.bit_depth(), 16);
    }

    #[test]
    fn png_bomb_rejected_without_decoding() {
        let err = decode_budgeted(BOMB_PNG_20000, MediaKind::Png, DecodeBudget::default())
            .expect_err("20000x20000 png bomb must breach the budget");
        match err {
            DecodeError::BudgetExceeded {
                width,
                height,
                color,
                estimated_bytes,
                budget_bytes,
            } => {
                assert_eq!((width, height), (20000, 20000));
                assert_eq!(color, ColorType::Rgb8);
                // The decoder's own number: 20000x20000 RGB8.
                assert_eq!(estimated_bytes, 20000u64 * 20000 * 3);
                assert_eq!(budget_bytes, DecodeBudget::default().max_bytes);
                // The ceiling assertion: this bomb's estimate breaches
                // the 1 GiB RSS ceiling too — anything past the ceiling
                // is necessarily over the (smaller) budget, so it is
                // rejected BEFORE any pixel is allocated.
                assert!(estimated_bytes > RSS_CEILING_BYTES);
            }
            other => panic!("expected BudgetExceeded, got {other:?}"),
        }
        assert_eq!(err.pf_code(), Some("PF-2007"));
    }

    #[test]
    fn header_only_bomb_rejected_without_decoding() {
        // 65 bytes, no pixel data at all: the verdict came from the
        // header (the decoder's total_bytes), not from a decode.
        // decode_image on the same bytes fails DIFFERENTLY (no IDAT
        // pixels to read) — the budget path never gets that far.
        let err = decode_budgeted(
            BOMB_PNG_HEADER_ONLY,
            MediaKind::Png,
            DecodeBudget::default(),
        )
        .expect_err("header-only bomb must breach the budget");
        assert!(
            matches!(err, DecodeError::BudgetExceeded { estimated_bytes, .. } if estimated_bytes == 20000u64 * 20000 * 3),
            "expected BudgetExceeded with the 1.2 GB estimate, got {err:?}"
        );
        assert!(matches!(
            decode_image(BOMB_PNG_HEADER_ONLY, MediaKind::Png),
            Err(DecodeError::Image(_))
        ));
    }

    #[test]
    fn jpeg_header_bomb_rejected() {
        let err = decode_budgeted(BOMB_JPEG_20000, MediaKind::Jpeg, DecodeBudget::default())
            .expect_err("20000x20000 jpeg header bomb must breach the budget");
        assert!(
            matches!(err, DecodeError::BudgetExceeded { width: 20000, height: 20000, estimated_bytes, .. } if estimated_bytes == 20000u64 * 20000 * 3),
            "expected BudgetExceeded, got {err:?}"
        );
        assert_eq!(err.pf_code(), Some("PF-2007"));
    }

    #[test]
    fn webp_header_bomb_rejected() {
        let err = decode_budgeted(BOMB_WEBP_20000, MediaKind::WebP, DecodeBudget::default())
            .expect_err("20000x20000 webp header bomb must breach the budget");
        assert!(
            matches!(err, DecodeError::BudgetExceeded { width: 20000, height: 20000, estimated_bytes, .. } if estimated_bytes == 20000u64 * 20000 * 3),
            "expected BudgetExceeded, got {err:?}"
        );
    }

    #[test]
    fn tiff_header_bomb_rejected() {
        // The TIFF header bomb is GRAYSCALE (SamplesPerPixel=1, L8 —
        // see gen_bomb_fixtures.py), so the decoder's own estimate is
        // 20000x20000x1 = 400,000,000 B — UNDER the 512 MiB default
        // budget. Like the 8001 PNG bomb, the guards reject it on dims
        // (PF-2005) but the budget alone admits it. A tight custom
        // budget proves the TIFF verdict path: one byte under the
        // estimate rejects as BudgetExceeded with the exact numbers.
        let tight = DecodeBudget::new(400_000_000 - 1);
        let err = decode_budgeted(BOMB_TIFF_20000, MediaKind::Tiff, tight)
            .expect_err("tiff header bomb must breach a 399,999,999-byte budget");
        assert!(
            matches!(
                err,
                DecodeError::BudgetExceeded {
                    width: 20000,
                    height: 20000,
                    color: ColorType::L8,
                    estimated_bytes: 400_000_000,
                    budget_bytes: 399_999_999
                }
            ),
            "expected BudgetExceeded(L8, 400M/399999999), got {err:?}"
        );
        assert_eq!(err.pf_code(), Some("PF-2007"));
        // And at exactly the estimate it admits — the verdict is `>`
        // on the decoder's own number (the decode itself then fails
        // on the truncated strip, which is the fixture's honesty:
        // one row of strip bytes for a 20000-row image).
        let err = decode_budgeted(
            BOMB_TIFF_20000,
            MediaKind::Tiff,
            DecodeBudget::new(400_000_000),
        )
        .expect_err(
            "at-budget tiff bomb passes the verdict, then fails decoding its truncated strip",
        );
        assert!(
            matches!(err, DecodeError::Image(_)),
            "expected an Image (truncated-strip) failure past the verdict, got {err:?}"
        );
    }

    #[test]
    fn one_over_dims_bomb_admitted_by_budget() {
        // Honest split from P2-006: the 8001x8001 bomb is a DIMENSIONS
        // breach (PF-2005, the guards' verdict) but its decoded size —
        // 8001x8001 RGB8 = 192,048,003 B — is UNDER the 512 MiB memory
        // budget, so the budget path admits it. The guards reject it
        // first in the P2-009 suite; the budget alone must not.
        // A tight custom budget proves the verdict is the estimate:
        // 192,048,003 admits, one byte less rejects.
        let exact = DecodeBudget::new(8001u64 * 8001 * 3);
        let decoded = decode_budgeted(BOMB_PNG_8001, MediaKind::Png, exact)
            .expect("8001 bomb at its exact estimate admits");
        assert_eq!((decoded.width(), decoded.height()), (8001, 8001));
        let one_less = DecodeBudget::new(8001u64 * 8001 * 3 - 1);
        let err = decode_budgeted(BOMB_PNG_8001, MediaKind::Png, one_less)
            .expect_err("one byte under the estimate must reject");
        assert!(
            matches!(err, DecodeError::BudgetExceeded { estimated_bytes, .. } if estimated_bytes == 8001u64 * 8001 * 3),
            "expected BudgetExceeded, got {err:?}"
        );
    }

    #[test]
    fn custom_budget_boundary() {
        // 16x16 RGB8 = 768 bytes decoded. At exactly 768 it admits;
        // at 767 it rejects — the verdict is `>` on the estimate.
        let img = DynamicImage::ImageRgb8(gradient_rgb8(16, 16));
        let bytes = encode_png8(&img);
        decode_budgeted(&bytes, MediaKind::Png, DecodeBudget::new(768))
            .expect("budget == estimate admits");
        let err = decode_budgeted(&bytes, MediaKind::Png, DecodeBudget::new(767))
            .expect_err("budget == estimate - 1 rejects");
        assert!(
            matches!(
                err,
                DecodeError::BudgetExceeded {
                    estimated_bytes: 768,
                    budget_bytes: 767,
                    ..
                }
            ),
            "expected BudgetExceeded(768/767), got {err:?}"
        );
    }

    #[test]
    fn zero_budget_rejects_anything() {
        // Fail-closed, the guards precedent: max_bytes = 0 rejects
        // any non-empty input.
        let img = DynamicImage::ImageRgb8(gradient_rgb8(16, 16));
        let bytes = encode_png8(&img);
        let err = decode_budgeted(&bytes, MediaKind::Png, DecodeBudget::new(0))
            .expect_err("zero budget must reject");
        assert!(matches!(
            err,
            DecodeError::BudgetExceeded {
                budget_bytes: 0,
                ..
            }
        ));
    }

    #[test]
    fn eight_k_fixture_within_default_budget() {
        // The largest image the dimension guard admits (8000x8000
        // RGB8 = 192,000,000 B decoded) decodes under the default
        // budget — the budget never binds a legitimate image.
        let decoded = decode_budgeted(IMG_8K_PNG_CLEAN, MediaKind::Png, DecodeBudget::default())
            .expect("8k clean png within the default budget");
        assert_eq!((decoded.width(), decoded.height()), (8000, 8000));
        assert_eq!(decoded.color(), ColorType::Rgb8);
    }

    #[test]
    fn empty_input_fails() {
        let err = decode_budgeted(&[], MediaKind::Png, DecodeBudget::default())
            .expect_err("empty input must fail");
        assert!(matches!(err, DecodeError::EmptyInput));
        assert_eq!(err.pf_code(), None);
    }

    #[test]
    fn video_kind_is_not_an_image() {
        for kind in [MediaKind::Mp4, MediaKind::Mkv, MediaKind::Webm] {
            let err = decode_budgeted(&[0u8; 16], kind, DecodeBudget::default())
                .expect_err("video kind must fail as NotAnImage");
            assert!(matches!(err, DecodeError::NotAnImage(k) if k == kind));
            assert_eq!(err.pf_code(), None);
        }
    }

    #[cfg(not(feature = "avif"))]
    #[test]
    fn avif_disabled_without_feature() {
        let err = decode_budgeted(&[0u8; 16], MediaKind::Avif, DecodeBudget::default())
            .expect_err("avif without the feature must fail");
        assert!(matches!(
            err,
            DecodeError::FeatureDisabled(MediaKind::Avif, "avif")
        ));
        assert_eq!(err.pf_code(), None);
    }

    #[cfg(feature = "avif")]
    #[test]
    fn avif_small_image_within_budget() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(32, 24));
        let mut bytes = Vec::new();
        img.write_with_encoder(image::codecs::avif::AvifEncoder::new(&mut bytes))
            .expect("avif encode");
        let decoded = decode_budgeted(&bytes, MediaKind::Avif, DecodeBudget::default())
            .expect("small avif within budget");
        assert_eq!((decoded.width(), decoded.height()), (32, 24));
    }

    #[test]
    fn wrong_kind_fails() {
        // Magic wins: PNG bytes forced as JPEG fail instead of
        // decoding as something else — same contract as decode_image.
        let img = DynamicImage::ImageRgb8(gradient_rgb8(16, 16));
        let bytes = encode_png8(&img);
        let err = decode_budgeted(&bytes, MediaKind::Jpeg, DecodeBudget::default())
            .expect_err("png bytes as jpeg must fail");
        assert!(matches!(err, DecodeError::Image(_)));
    }
}
