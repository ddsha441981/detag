//! Intake size/dimension guards — the P2-006 decompression-bomb guard (FR-U-04).
//!
//! [`check_intake_guards`] is the gate every image passes BEFORE full
//! decode: the plan byte cap first (pure arithmetic on the byte length),
//! then the pixel budget from the image HEADER alone. A decompression
//! bomb is kilobytes on disk and gigabytes decoded — a solid-colour
//! 20000x20000 PNG compresses to ~1 MB but decodes to ~1.2 GB of
//! pixels — so the guard must never decode to decide: it reads the
//! dimensions with `ImageReader::into_dimensions()`, which constructs
//! the decoder and reads the header only — no pixel data is decoded
//! (verified against the `image` 0.25.10 registry sources: PNG reads
//! IHDR via `reader.info().size()`; the JPEG wrapper calls
//! `decode_headers()` at construction; the TIFF `Decoder::new` reads
//! header + IFD; WebP reads the VP8X chunk at construction).
//!
//! "Header only" has a precise meaning per decoder, and the bomb
//! fixtures encode it: each decoder demands a structural minimum
//! before it reports dims — PNG's `read_info()` requires an IDAT
//! chunk to have begun (an empty zlib stream suffices; the pixels
//! are never read), zune-jpeg's `decode_headers()` loops until SOS,
//! image-webp requires exactly one of VP8/VP8L after VP8X, and the
//! tiff crate reads the IFD tags (a SHORT count packs as u32).
//! The fixtures carry exactly that minimum and no pixel data, so a
//! rejection still proves the verdict came from the header.
//!
//! # The verdict lives in core
//!
//! The caps themselves are NOT re-decided here: the side cap (8000 px)
//! and the pixel budget (64 MP) are `detag_core::media::{MAX_IMAGE_SIDE,
//! MAX_IMAGE_PIXELS, validate_dimensions}` (P0-104, FR-U-04), and this
//! guard delegates the verdict to [`validate_dimensions`] — one source
//! of truth, one place to change the caps. The plan byte cap comes from
//! the caller's plan (`Entitlements.max_bytes`: Free 25 MiB, Pro 1 GiB,
//! Business 4 GiB); the guard takes it as a plain `u64` so it stays
//! usable without a database or a plan lookup.
//!
//! # What this does NOT do
//!
//! No memory budget — that is P2-007 (PF-2007, RSS ceiling). No
//! decoding, no EXIF/ICC handling (P2-003/P2-004), no quality metrics
//! (E2.7). No caller yet: the intake suite (P2-009) wires
//! `check_intake_guards` before `decode_image`, and the job-create
//! handler hooks the same guard — until then this module is the
//! enforcement point with no fake wiring, the P2-003/P2-004/P2-005
//! parse-only shape.
//!
//! # Error semantics (what a caller should do with each outcome)
//!
//! * `Ok(ProbedDimensions)` — within both caps; the probed dims are
//!   returned so the caller records them without re-probing.
//! * `Err(GuardError::ByteCapExceeded)` — the upload is over the plan's
//!   byte cap: PF-2003 (413) at the API layer.
//! * `Err(GuardError::DimensionsExceeded)` — the header claims dims over
//!   the 8000x8000 / 64 MP cap: PF-2005 (413) at the API layer. The
//!   error carries the CLAIMED dims (what the header says), not a
//!   decoded size — the image was never decoded.
//! * `Err(GuardError::FeatureDisabled | NotAnImage | UnreadableDimensions)`
//!   — intake errors, not cap rejections: the caller maps them to its
//!   own decode failure (see [`GuardError::pf_code`], which is `None`
//!   for these). A header that cannot be read fails CLOSED: an image
//!   whose dims cannot be proven within cap is not admitted.
//! * `max_bytes = 0` rejects any non-empty input — fail-closed, the
//!   same reading as `Entitlements::from_value` (missing/mistyped
//!   numerics read as 0).
//!
//! The upstream `Limits` mechanism (`image::io::Limits`: strict
//! `max_image_width`/`max_image_height`, `max_alloc` default 512 MiB)
//! was considered and NOT used for the verdict: it has no pixel-total
//! limit, decoder support is uneven, and a breach surfaces as a generic
//! `ImageError::Limits`, not PF-2005. P2-007 owns the memory side
//! (`max_alloc` / `total_bytes` reserve).

use detag_core::media::{validate_dimensions, MediaKind};
use image::{ImageFormat, ImageReader};
use std::io::Cursor;

/// Dimensions probed from the image header by [`check_intake_guards`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbedDimensions {
    /// Width in pixels, as the header claims it.
    pub width: u32,
    /// Height in pixels, as the header claims it.
    pub height: u32,
}

/// Why an intake guard rejected an image. No panics: every failure is
/// a value the caller (intake, Phase 2/3) maps to its own error code.
#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    /// Nothing to guard.
    #[error("empty input")]
    EmptyInput,
    /// The upload is over the caller's plan byte cap (PF-2003, 413).
    #[error("upload is {bytes} bytes, over the plan cap of {max_bytes} bytes (PF-2003)")]
    ByteCapExceeded {
        /// Byte length of the upload.
        bytes: u64,
        /// The plan's `max_bytes` cap the upload was checked against.
        max_bytes: u64,
    },
    /// The header claims dims over the 8000x8000 / 64 MP cap (PF-2005,
    /// 413). Carries the CLAIMED dims — the image was never decoded.
    #[error("image is {width}x{height}, over the 8000x8000 / 64 MP cap (PF-2005)")]
    DimensionsExceeded {
        /// Claimed width in pixels.
        width: u32,
        /// Claimed height in pixels.
        height: u32,
    },
    /// The kind needs a cargo feature this build does not have
    /// (`tiff` / `avif` — see `intake.rs`). The caller should surface
    /// this as "format not enabled in this build", not as a corrupt file.
    #[error("format {0:?} needs cargo feature `{1}` (not enabled in this build)")]
    FeatureDisabled(MediaKind, &'static str),
    /// The kind is a video kind, not an image — the video intake
    /// (Phase 3) owns it.
    #[error("not an image kind: {0:?}")]
    NotAnImage(MediaKind),
    /// The header could not be read (truncated, corrupt, or not the
    /// forced format at all) — fail closed, never admitted.
    #[error("image dimensions unreadable: {0}")]
    UnreadableDimensions(#[from] image::ImageError),
}

impl GuardError {
    /// The PF error code this guard failure maps to at the API layer,
    /// or `None` when the caller maps it to its own decode failure:
    /// only the two cap rejections have PF codes (`ByteCapExceeded` →
    /// PF-2003, `DimensionsExceeded` → PF-2005).
    pub fn pf_code(&self) -> Option<&'static str> {
        match self {
            GuardError::ByteCapExceeded { .. } => Some("PF-2003"),
            GuardError::DimensionsExceeded { .. } => Some("PF-2005"),
            _ => None,
        }
    }
}

/// Check `bytes` (sniffed as `kind` by P2-001) against the intake caps
/// BEFORE full decode: the plan byte cap `max_bytes` first, then the
/// pixel budget from the header dims.
///
/// The format is forced on the header probe — the bytes are never
/// re-guessed — so a JPEG passed with `kind == MediaKind::Png` fails
/// here instead of probing as something else (magic wins, end to end).
/// Empty input fails as [`GuardError::EmptyInput`]; video kinds fail as
/// [`GuardError::NotAnImage`]; a kind whose cargo feature is off fails
/// as [`GuardError::FeatureDisabled`]; a header that cannot be read
/// fails as [`GuardError::UnreadableDimensions`] (fail closed).
///
/// On success the probed `(width, height)` are returned so the caller
/// records them without re-probing. The image is NEVER decoded by this
/// function — the bomb fixtures prove it: a 20000x20000 PNG is rejected
/// from its ~65-byte header, and the header-only bomb (an IDAT carrying
/// an empty zlib stream — no pixel data at all) is rejected while
/// `decode_image` on the same bytes fails.
pub fn check_intake_guards(
    bytes: &[u8],
    kind: MediaKind,
    max_bytes: u64,
) -> Result<ProbedDimensions, GuardError> {
    if bytes.is_empty() {
        return Err(GuardError::EmptyInput);
    }
    if bytes.len() as u64 > max_bytes {
        return Err(GuardError::ByteCapExceeded {
            bytes: bytes.len() as u64,
            max_bytes,
        });
    }
    let format = match kind {
        MediaKind::Png => ImageFormat::Png,
        MediaKind::Jpeg => ImageFormat::Jpeg,
        MediaKind::WebP => ImageFormat::WebP,
        #[cfg(feature = "tiff")]
        MediaKind::Tiff => ImageFormat::Tiff,
        #[cfg(not(feature = "tiff"))]
        MediaKind::Tiff => return Err(GuardError::FeatureDisabled(kind, "tiff")),
        #[cfg(feature = "avif")]
        MediaKind::Avif => ImageFormat::Avif,
        #[cfg(not(feature = "avif"))]
        MediaKind::Avif => return Err(GuardError::FeatureDisabled(kind, "avif")),
        MediaKind::Mp4 | MediaKind::Mkv | MediaKind::Webm => {
            return Err(GuardError::NotAnImage(kind));
        }
    };
    // Header-only probe: into_dimensions() constructs the decoder and
    // reads the dims — no pixel data is touched, so a bomb's claimed
    // dims are what gets rejected, before any allocation.
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
    // The verdict is core's (single source of truth). A claimed dim
    // past i32::MAX cannot convert, so it reads as missing — and
    // missing dims fail closed in validate_dimensions.
    let dims = (i32::try_from(width).ok(), i32::try_from(height).ok());
    validate_dimensions(dims.0, dims.1)
        .map_err(|_| GuardError::DimensionsExceeded { width, height })?;
    Ok(ProbedDimensions { width, height })
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
    use image::{DynamicImage, ImageBuffer, Rgb};

    /// The Free plan's byte cap (seed value, `crates/core/src/plans.rs`):
    /// 25 MiB. A real plan value, not a made-up small number.
    const FREE_MAX_BYTES: u64 = 26_214_400;

    /// Committed bomb fixtures (`tests/fixtures/bombs/`, generated by
    /// `gen_bomb_fixtures.py` — see its header for what each file is).
    const BOMB_PNG_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-png-20000x20000.png");
    const BOMB_PNG_8001: &[u8] = include_bytes!("../tests/fixtures/bombs/bomb-png-8001x8001.png");
    const BOMB_PNG_HEADER_ONLY: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-png-header-only-20000x20000.png");
    const BOMB_JPEG_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-jpeg-20000x20000.jpg");
    const BOMB_WEBP_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-webp-20000x20000.webp");
    #[cfg(feature = "tiff")]
    const BOMB_TIFF_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-tiff-20000x20000.tiff");

    /// The committed 8000x8000 fixture (P2-002, `fixtures/images/`):
    /// the cap itself, admitted by the guard — probed, never decoded.
    const AT_CAP_PNG: &[u8] = include_bytes!("../../../fixtures/images/img-8k-png-clean.png");

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
        let mut cursor = std::io::Cursor::new(Vec::new());
        img.write_with_encoder(TiffEncoder::new(&mut cursor))
            .expect("tiff encode");
        cursor.into_inner()
    }

    #[test]
    fn byte_cap_rejects_over_cap() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_png8(&src);
        let cap = bytes.len() as u64 - 1;
        let err =
            check_intake_guards(&bytes, MediaKind::Png, cap).expect_err("one byte over fails");
        assert!(
            matches!(err, GuardError::ByteCapExceeded { bytes: b, max_bytes: m }
                if b as usize == bytes.len() && m == cap),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), Some("PF-2003"));
    }

    #[test]
    fn byte_cap_admits_at_cap() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_png8(&src);
        let dims = check_intake_guards(&bytes, MediaKind::Png, bytes.len() as u64)
            .expect("exactly at cap passes");
        assert_eq!((dims.width, dims.height), (48, 32));
    }

    #[test]
    fn byte_cap_zero_rejects_everything() {
        // Fail-closed: a plan whose max_bytes reads 0 (missing/mistyped
        // entitlements) admits no upload at all.
        let src = DynamicImage::ImageRgb8(gradient_rgb8(8, 8));
        let bytes = encode_png8(&src);
        let err = check_intake_guards(&bytes, MediaKind::Png, 0).expect_err("cap 0 rejects");
        assert!(
            matches!(err, GuardError::ByteCapExceeded { max_bytes: 0, .. }),
            "unexpected: {err}"
        );
    }

    #[test]
    fn free_plan_cap_admits_small_upload() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_png8(&src);
        assert!(bytes.len() as u64 <= FREE_MAX_BYTES);
        let dims = check_intake_guards(&bytes, MediaKind::Png, FREE_MAX_BYTES)
            .expect("small upload under the Free 25 MiB cap passes");
        assert_eq!((dims.width, dims.height), (48, 32));
    }

    #[test]
    fn small_images_probe_dims_per_format() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        for (bytes, kind) in [
            (encode_png8(&src), MediaKind::Png),
            (encode_jpeg(&src, 90), MediaKind::Jpeg),
            (encode_webp_lossless(&src), MediaKind::WebP),
        ] {
            let dims = check_intake_guards(&bytes, kind, FREE_MAX_BYTES)
                .unwrap_or_else(|e| panic!("{kind:?} probes: {e}"));
            assert_eq!((dims.width, dims.height), (48, 32), "{kind:?}");
        }
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn small_tiff_probes_dims() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_tiff(&src);
        let dims =
            check_intake_guards(&bytes, MediaKind::Tiff, FREE_MAX_BYTES).expect("tiff probes");
        assert_eq!((dims.width, dims.height), (48, 32));
    }

    #[test]
    fn boundary_8000x8000_admitted() {
        // The cap itself, from the committed 8k fixture — the guard
        // probes the header only, it never decodes the 64 MP image.
        let dims = check_intake_guards(AT_CAP_PNG, MediaKind::Png, FREE_MAX_BYTES)
            .expect("8000x8000 is admitted");
        assert_eq!((dims.width, dims.height), (8000, 8000));
    }

    #[test]
    fn png_bomb_rejected() {
        // The AC: a real decompression bomb — 1,166,632 bytes on disk,
        // ~1.2 GB decoded — rejected from the header alone.
        assert!(BOMB_PNG_20000.len() < 2_000_000, "fixture is a bomb");
        let err = check_intake_guards(BOMB_PNG_20000, MediaKind::Png, FREE_MAX_BYTES)
            .expect_err("20000x20000 bomb rejected");
        assert!(
            matches!(
                err,
                GuardError::DimensionsExceeded {
                    width: 20000,
                    height: 20000
                }
            ),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), Some("PF-2005"));
    }

    #[test]
    fn png_one_over_side_cap_rejected() {
        let err = check_intake_guards(BOMB_PNG_8001, MediaKind::Png, FREE_MAX_BYTES)
            .expect_err("8001x8001 rejected");
        assert!(
            matches!(
                err,
                GuardError::DimensionsExceeded {
                    width: 8001,
                    height: 8001
                }
            ),
            "unexpected: {err}"
        );
    }

    #[test]
    fn png_header_only_bomb_rejected_without_decoding() {
        // 65 bytes: signature + IHDR + an IDAT carrying an empty zlib
        // stream + IEND — no pixel data at all. The guard rejects it
        // from the header — while decode_image on the SAME bytes fails
        // with an Image error. The two verdicts differ, so the guard's
        // verdict cannot have come from a decode attempt.
        let err = check_intake_guards(BOMB_PNG_HEADER_ONLY, MediaKind::Png, FREE_MAX_BYTES)
            .expect_err("header-only bomb rejected");
        assert!(
            matches!(
                err,
                GuardError::DimensionsExceeded {
                    width: 20000,
                    height: 20000
                }
            ),
            "unexpected: {err}"
        );
        let dec_err = decode_image(BOMB_PNG_HEADER_ONLY, MediaKind::Png)
            .expect_err("no pixel data, decode fails");
        assert!(
            matches!(dec_err, crate::intake::DecodeError::Image(_)),
            "unexpected decode error: {dec_err}"
        );
    }

    #[test]
    fn jpeg_header_bomb_rejected() {
        let err = check_intake_guards(BOMB_JPEG_20000, MediaKind::Jpeg, FREE_MAX_BYTES)
            .expect_err("jpeg header bomb rejected");
        assert!(
            matches!(
                err,
                GuardError::DimensionsExceeded {
                    width: 20000,
                    height: 20000
                }
            ),
            "unexpected: {err}"
        );
    }

    #[test]
    fn webp_header_bomb_rejected() {
        let err = check_intake_guards(BOMB_WEBP_20000, MediaKind::WebP, FREE_MAX_BYTES)
            .expect_err("webp header bomb rejected");
        assert!(
            matches!(
                err,
                GuardError::DimensionsExceeded {
                    width: 20000,
                    height: 20000
                }
            ),
            "unexpected: {err}"
        );
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_header_bomb_rejected() {
        let err = check_intake_guards(BOMB_TIFF_20000, MediaKind::Tiff, FREE_MAX_BYTES)
            .expect_err("tiff header bomb rejected");
        assert!(
            matches!(
                err,
                GuardError::DimensionsExceeded {
                    width: 20000,
                    height: 20000
                }
            ),
            "unexpected: {err}"
        );
    }

    #[cfg(not(feature = "avif"))]
    #[test]
    fn avif_disabled_without_feature() {
        let err =
            check_intake_guards(&[0u8; 64], MediaKind::Avif, FREE_MAX_BYTES).expect_err("avif off");
        assert!(
            matches!(err, GuardError::FeatureDisabled(MediaKind::Avif, "avif")),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), None);
    }

    #[cfg(feature = "avif")]
    #[test]
    fn avif_small_image_probes_dims() {
        use image::codecs::avif::AvifEncoder;
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let mut buf = Vec::new();
        src.write_with_encoder(AvifEncoder::new_with_speed_quality(&mut buf, 6, 80))
            .expect("avif encode");
        let dims =
            check_intake_guards(&buf, MediaKind::Avif, FREE_MAX_BYTES).expect("small avif probes");
        assert_eq!((dims.width, dims.height), (48, 32));
    }

    #[test]
    fn video_kinds_are_not_images() {
        for kind in [MediaKind::Mp4, MediaKind::Mkv, MediaKind::Webm] {
            let err =
                check_intake_guards(&[0u8; 64], kind, FREE_MAX_BYTES).expect_err("video fails");
            assert!(
                matches!(err, GuardError::NotAnImage(k) if k == kind),
                "unexpected: {err}"
            );
            assert_eq!(err.pf_code(), None);
        }
    }

    #[test]
    fn empty_input_fails() {
        let err =
            check_intake_guards(&[], MediaKind::Png, FREE_MAX_BYTES).expect_err("empty fails");
        assert!(matches!(err, GuardError::EmptyInput));
    }

    #[test]
    fn corrupt_header_fails_closed() {
        // Garbage forced as PNG: the header cannot be read, so the
        // image is not admitted — an unprovable image fails closed.
        let err = check_intake_guards(&[0u8; 64], MediaKind::Png, FREE_MAX_BYTES)
            .expect_err("garbage fails");
        assert!(
            matches!(err, GuardError::UnreadableDimensions(_)),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), None);
        // A header-only real PNG (8 signature bytes) is unreadable too.
        let err = check_intake_guards(
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
            MediaKind::Png,
            FREE_MAX_BYTES,
        )
        .expect_err("signature-only fails");
        assert!(
            matches!(err, GuardError::UnreadableDimensions(_)),
            "unexpected: {err}"
        );
    }

    #[test]
    fn pf_codes_map_only_for_cap_rejections() {
        assert_eq!(
            GuardError::ByteCapExceeded {
                bytes: 11,
                max_bytes: 10
            }
            .pf_code(),
            Some("PF-2003")
        );
        assert_eq!(
            GuardError::DimensionsExceeded {
                width: 20000,
                height: 20000
            }
            .pf_code(),
            Some("PF-2005")
        );
        assert_eq!(GuardError::EmptyInput.pf_code(), None);
        assert_eq!(
            GuardError::FeatureDisabled(MediaKind::Avif, "avif").pf_code(),
            None
        );
        assert_eq!(GuardError::NotAnImage(MediaKind::Mp4).pf_code(), None);
    }
}
