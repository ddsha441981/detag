//! Image decoding — the E2.1 intake decoder matrix (P2-002, FR-U-03, FR-D-07).
//!
//! [`decode_image`] turns sniffed bytes into pixels. The caller passes
//! the [`MediaKind`] the P2-001 sniffer (`detag_core::media`) already
//! proved from the magic bytes — the format is FORCED, never guessed
//! from the bytes again, so a file whose bytes disagree with its
//! sniffed kind fails here instead of decoding as something else
//! (magic wins, end to end).
//!
//! # Format matrix (cargo features)
//!
//! | Format | Always on | Feature | Notes |
//! | ------ | --------- | ------- | ----- |
//! | PNG 8-bit / 16-bit | yes | — | 16-bit path preserves bit depth: a 16-bit PNG decodes to `ImageRgb16`/`ImageRgba16`, never downsampled to 8-bit (FR-D-07 — the output must be able to preserve what the input had). |
//! | JPEG | yes | — | Baseline + progressive, via zune-jpeg. |
//! | WebP lossy + lossless | yes | — | Lossless via VP8L; lossy via the committed Pillow fixtures (the `image` crate's `WebPEncoder` is lossless-only, so no lossy encoder exists in-tree to round-trip against). |
//! | TIFF 8-bit / 16-bit | default | `tiff` | Pure Rust (`tiff` crate); disable with `--no-default-features`. |
//! | AVIF | no | `avif` | Decode needs the `image` crate's `avif-native` feature, which links the SYSTEM libdav1d (>= 1.3.0) — install `libdav1d-dev` before building with it (ADR-0071). Without the feature, AVIF decode fails with [`DecodeError::FeatureDisabled`], never silently. AVIF encode (ravif, pure Rust) rides along with the same feature. |
//!
//! Video kinds (`Mp4`/`Mkv`/`Webm`) are not images: decoding one is
//! [`DecodeError::NotAnImage`]. The intake caller for video is Phase 3.
//!
//! # What this does NOT do
//!
//! No size/dimension guards here — those live in `guards.rs`
//! (P2-006, PF-2005 pixel budget + PF-2003 plan byte cap, checked
//! BEFORE full decode; call `check_intake_guards` before this
//! module's `decode_image`). P2-007 owns the memory budget
//! (PF-2007). No EXIF/ICC handling — P2-003/P2-004. No quality metrics —
//! E2.7. This module only proves: the format matrix decodes, the
//! format is the sniffed one, and 16-bit stays 16-bit.

use detag_core::media::MediaKind;
use image::{ColorType, DynamicImage, ImageFormat, ImageReader};
use std::io::Cursor;

/// A decoded image: the pixels plus the format they were decoded as.
#[derive(Debug)]
pub struct DecodedImage {
    /// The decoded pixels. 16-bit sources stay 16-bit (`ImageRgb16` /
    /// `ImageRgba16`); nothing here downsamples.
    pub image: DynamicImage,
    /// The sniffed kind this was decoded as (== the format forced on
    /// the decoder).
    pub format: MediaKind,
}

impl DecodedImage {
    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// The decoded colour type (`Rgb8`, `Rgb16`, …).
    pub fn color(&self) -> ColorType {
        self.image.color()
    }

    /// Bit depth of one channel: 8, 16, or 32 (float). A 16-bit PNG
    /// reports 16 — the P2-002 AC is that this never reads 8 for a
    /// 16-bit source.
    pub fn bit_depth(&self) -> u8 {
        match self.image.color() {
            ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8 => 8,
            ColorType::L16 | ColorType::La16 | ColorType::Rgb16 | ColorType::Rgba16 => 16,
            ColorType::Rgb32F | ColorType::Rgba32F => 32,
            _ => 8,
        }
    }

    /// True when the source was 16-bit per channel.
    pub fn is_16_bit(&self) -> bool {
        self.bit_depth() == 16
    }
}

/// Why a decode failed. No panics: every failure is a value the
/// caller (intake, Phase 2/3) maps to its own error code.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The kind is a video kind, not an image — the video intake
    /// (Phase 3) owns it.
    #[error("not an image kind: {0:?}")]
    NotAnImage(MediaKind),
    /// The kind needs a cargo feature this build does not have
    /// (`tiff` / `avif` — see the module docs). The caller should
    /// surface this as "format not enabled in this build", not as a
    /// corrupt file.
    #[error("format {0:?} needs cargo feature `{1}` (not enabled in this build)")]
    FeatureDisabled(MediaKind, &'static str),
    /// Nothing to decode.
    #[error("empty input")]
    EmptyInput,
    /// The `image` crate refused the bytes (truncated, corrupt, or
    /// not the forced format at all).
    #[error("image decode failed: {0}")]
    Image(#[from] image::ImageError),
}

/// Decode `bytes` as the sniffed `kind` (P2-001).
///
/// The format is forced on the decoder — the bytes are never
/// re-guessed — so a JPEG passed with `kind == MediaKind::Png`
/// fails instead of decoding as JPEG. Empty input fails as
/// [`DecodeError::EmptyInput`]; video kinds fail as
/// [`DecodeError::NotAnImage`]; a kind whose cargo feature is off
/// fails as [`DecodeError::FeatureDisabled`].
pub fn decode_image(bytes: &[u8], kind: MediaKind) -> Result<DecodedImage, DecodeError> {
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
    let image = ImageReader::with_format(Cursor::new(bytes), format).decode()?;
    Ok(DecodedImage {
        image,
        format: kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::media::{sniff_media, HEAD_SNIFF_LEN, TAIL_SNIFF_LEN};
    #[cfg(feature = "avif")]
    use image::codecs::avif::AvifEncoder;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    #[cfg(feature = "tiff")]
    use image::codecs::tiff::TiffEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{DynamicImage, ImageBuffer, Rgb};
    use std::path::PathBuf;

    /// Deterministic 8-bit RGB gradient: every pixel differs from its
    /// neighbours, so a lossy round-trip that smears values shows up
    /// in the max-diff assertion.
    fn gradient_rgb8(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([
                (x.wrapping_mul(7).wrapping_add(y.wrapping_mul(3)) % 256) as u8,
                (x.wrapping_mul(3).wrapping_add(y.wrapping_mul(11)) % 256) as u8,
                (x.wrapping_add(y).wrapping_mul(5) % 256) as u8,
            ])
        })
    }

    /// Deterministic 16-bit RGB gradient spanning the full 0..=65535
    /// range — a decoder that downsamples to 8-bit loses the low
    /// byte, which the exact-pixel assertion catches.
    fn gradient_rgb16(width: u32, height: u32) -> ImageBuffer<Rgb<u16>, Vec<u16>> {
        ImageBuffer::from_fn(width, height, |x, y| {
            let v = |a: u32, b: u32| -> u16 { ((x * a + y * b) % 65536) as u16 };
            Rgb([v(257, 131), v(131, 257), v(389, 61)])
        })
    }

    /// Max absolute per-channel difference between two 8-bit RGB
    /// images of the same size, plus the mean absolute difference.
    fn rgb8_diff(
        a: &ImageBuffer<Rgb<u8>, Vec<u8>>,
        b: &ImageBuffer<Rgb<u8>, Vec<u8>>,
    ) -> (u8, f64) {
        assert_eq!(a.dimensions(), b.dimensions());
        let mut max = 0u8;
        let mut sum: u64 = 0;
        for (pa, pb) in a.pixels().zip(b.pixels()) {
            for c in 0..3 {
                let d = pa[c].abs_diff(pb[c]);
                max = max.max(d);
                sum += u64::from(d);
            }
        }
        let n = u64::from(a.width() * a.height() * 3);
        (max, sum as f64 / n as f64)
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

    #[cfg(feature = "avif")]
    fn encode_avif(img: &DynamicImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_with_encoder(AvifEncoder::new_with_speed_quality(&mut buf, 6, 80))
            .expect("avif encode");
        buf
    }

    #[test]
    fn png_8bit_roundtrip_exact() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_png8(&src);
        let dec = decode_image(&bytes, MediaKind::Png).expect("png decodes");
        assert_eq!(dec.format, MediaKind::Png);
        assert_eq!((dec.width(), dec.height()), (48, 32));
        assert_eq!(dec.color(), ColorType::Rgb8);
        assert_eq!(dec.bit_depth(), 8);
        assert!(!dec.is_16_bit());
        assert_eq!(dec.image.to_rgb8(), src.to_rgb8(), "png is lossless");
    }

    #[test]
    fn png_16bit_path_preserves_bit_depth() {
        let src = DynamicImage::ImageRgb16(gradient_rgb16(48, 32));
        let bytes = encode_png8(&src);
        let dec = decode_image(&bytes, MediaKind::Png).expect("16-bit png decodes");
        assert_eq!(dec.color(), ColorType::Rgb16, "16-bit stays 16-bit");
        assert_eq!(dec.bit_depth(), 16);
        assert!(dec.is_16_bit());
        assert_eq!(dec.image.to_rgb16(), src.to_rgb16(), "16-bit pixels exact");
    }

    #[test]
    fn jpeg_roundtrip_bounded_error() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_jpeg(&src, 90);
        let dec = decode_image(&bytes, MediaKind::Jpeg).expect("jpeg decodes");
        assert_eq!((dec.width(), dec.height()), (48, 32));
        assert_eq!(dec.color(), ColorType::Rgb8);
        assert_eq!(dec.bit_depth(), 8);
        let (max, mean) = rgb8_diff(&src.to_rgb8(), &dec.image.to_rgb8());
        assert!(max <= 64, "jpeg q90 max channel diff {max} (bound 64)");
        assert!(mean < 10.0, "jpeg q90 mean diff {mean:.2} (bound 10)");
    }

    #[test]
    fn webp_lossless_roundtrip_exact() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_webp_lossless(&src);
        let dec = decode_image(&bytes, MediaKind::WebP).expect("webp lossless decodes");
        assert_eq!((dec.width(), dec.height()), (48, 32));
        assert_eq!(dec.color(), ColorType::Rgb8);
        assert_eq!(dec.image.to_rgb8(), src.to_rgb8(), "webp lossless is exact");
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_8bit_roundtrip_exact() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_tiff(&src);
        let dec = decode_image(&bytes, MediaKind::Tiff).expect("tiff decodes");
        assert_eq!((dec.width(), dec.height()), (48, 32));
        assert_eq!(dec.color(), ColorType::Rgb8);
        assert_eq!(dec.bit_depth(), 8);
        assert_eq!(dec.image.to_rgb8(), src.to_rgb8(), "tiff is lossless");
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_16bit_path_preserves_bit_depth() {
        let src = DynamicImage::ImageRgb16(gradient_rgb16(48, 32));
        let bytes = encode_tiff(&src);
        let dec = decode_image(&bytes, MediaKind::Tiff).expect("16-bit tiff decodes");
        assert_eq!(dec.color(), ColorType::Rgb16, "16-bit stays 16-bit");
        assert_eq!(dec.bit_depth(), 16);
        assert_eq!(dec.image.to_rgb16(), src.to_rgb16(), "16-bit pixels exact");
    }

    #[cfg(feature = "avif")]
    #[test]
    fn avif_roundtrip_bounded_error() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_avif(&src);
        let dec = decode_image(&bytes, MediaKind::Avif).expect("avif decodes");
        assert_eq!((dec.width(), dec.height()), (48, 32));
        assert_eq!(dec.bit_depth(), 8);
        let (max, mean) = rgb8_diff(&src.to_rgb8(), &dec.image.to_rgb8());
        assert!(max <= 64, "avif q80 max channel diff {max} (bound 64)");
        assert!(mean < 10.0, "avif q80 mean diff {mean:.2} (bound 10)");
    }

    #[cfg(not(feature = "avif"))]
    #[test]
    fn avif_disabled_without_feature() {
        // Any non-empty input takes the feature-gate branch first.
        let err = decode_image(&[0u8; 64], MediaKind::Avif).expect_err("avif off");
        assert!(
            matches!(err, DecodeError::FeatureDisabled(MediaKind::Avif, "avif")),
            "unexpected error: {err}"
        );
    }

    #[cfg(not(feature = "tiff"))]
    #[test]
    fn tiff_disabled_without_feature() {
        let err = decode_image(&[0u8; 64], MediaKind::Tiff).expect_err("tiff off");
        assert!(
            matches!(err, DecodeError::FeatureDisabled(MediaKind::Tiff, "tiff")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_input_fails() {
        let err = decode_image(&[], MediaKind::Png).expect_err("empty fails");
        assert!(matches!(err, DecodeError::EmptyInput));
    }

    #[test]
    fn truncated_png_fails_not_panics() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_png8(&src);
        let cut = &bytes[..bytes.len() / 2];
        assert!(
            decode_image(cut, MediaKind::Png).is_err(),
            "truncated png fails"
        );
        assert!(
            decode_image(&bytes[..8], MediaKind::Png).is_err(),
            "header-only fails"
        );
    }

    #[test]
    fn format_is_forced_not_guessed() {
        // JPEG bytes passed as PNG must fail: the decoder is forced
        // to the sniffed kind, it never re-guesses (magic wins).
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let jpeg = encode_jpeg(&src, 90);
        let err = decode_image(&jpeg, MediaKind::Png).expect_err("jpeg-as-png fails");
        assert!(matches!(err, DecodeError::Image(_)), "unexpected: {err}");
    }

    #[test]
    fn video_kinds_are_not_images() {
        for kind in [MediaKind::Mp4, MediaKind::Mkv, MediaKind::Webm] {
            let err = decode_image(&[0u8; 64], kind).expect_err("video fails");
            assert!(
                matches!(err, DecodeError::NotAnImage(k) if k == kind),
                "unexpected: {err}"
            );
        }
    }

    /// The committed fixtures (Pillow-produced: PNG lossless, JPEG q90
    /// 4:4:4, WebP lossy q90 — real-encoder output, not this crate's
    /// own encoders) all decode, with the dims the filename tier
    /// promises, and the sniffed kind is the kind decoded.
    #[test]
    fn committed_fixtures_decode() {
        let dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("images");
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("fixtures/images exists")
            .map(|e| {
                e.expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        entries.sort();
        assert_eq!(entries.len(), 36, "fixture count: {entries:?}");
        let mut decoded = 0;
        for name in &entries {
            let parts: Vec<&str> = name.split(['-', '.']).collect();
            // img-<tier>-<fmt>-<mark>.<ext>
            assert_eq!(parts.len(), 5, "fixture name shape: {name}");
            let (tier, fmt) = (parts[1], parts[2]);
            let (want_w, want_h) = match tier {
                "512" => (512, 512),
                "1080p" => (1920, 1080),
                "4k" => (3840, 2160),
                // fixtures_gen SIZES: 8k is capped square 8000x8000.
                "8k" => (8000, 8000),
                _ => panic!("unknown tier in {name}"),
            };
            let bytes = std::fs::read(dir.join(name)).expect("fixture reads");
            let head = &bytes[..HEAD_SNIFF_LEN.min(bytes.len())];
            let tail = &bytes[bytes.len() - TAIL_SNIFF_LEN.min(bytes.len())..];
            let kind = sniff_media(head, tail, bytes.len() as u64)
                .unwrap_or_else(|e| panic!("{name} sniffs: {e}"));
            let want_kind = match fmt {
                "png" => MediaKind::Png,
                "jpg" => MediaKind::Jpeg,
                "webp" => MediaKind::WebP,
                _ => panic!("unknown fmt in {name}"),
            };
            assert_eq!(kind, want_kind, "{name}: sniffed kind");
            let dec = decode_image(&bytes, kind).unwrap_or_else(|e| panic!("{name} decodes: {e}"));
            assert_eq!(
                (dec.width(), dec.height()),
                (want_w, want_h),
                "{name}: dims"
            );
            assert_eq!(dec.color(), ColorType::Rgb8, "{name}: colour");
            assert_eq!(dec.bit_depth(), 8, "{name}: bit depth");
            decoded += 1;
        }
        assert_eq!(decoded, 36);
    }

    /// Lossy fixtures stay close to the lossless control of the same
    /// tier: JPEG q90 + WebP q90 against the PNG clean image. This is
    /// the lossy-decode sanity check — a decoder that garbles lossy
    /// data shows up here, not in a self-encoder round-trip.
    #[test]
    fn lossy_fixtures_close_to_png_control() {
        let dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("images");
        for tier in ["512", "1080p", "4k"] {
            let png = std::fs::read(dir.join(format!("img-{tier}-png-clean.png"))).unwrap();
            let png = decode_image(&png, MediaKind::Png).unwrap().image.to_rgb8();
            for (fmt, kind) in [("jpg", MediaKind::Jpeg), ("webp", MediaKind::WebP)] {
                let bytes =
                    std::fs::read(dir.join(format!("img-{tier}-{fmt}-clean.{fmt}"))).unwrap();
                let dec = decode_image(&bytes, kind).unwrap().image.to_rgb8();
                let (max, mean) = rgb8_diff(&png, &dec);
                // Measured 2026-11-26 (P2-002): 512/jpg max 43 mean
                // 2.45, 512/webp max 171 mean 6.79. The base scene
                // carries per-pixel noise, so a lossy max is dominated
                // by single noisy pixels — the mean is the sanity
                // signal; the max bound is a gross-corruption
                // tripwire only, not a quality floor (floors are E2.7).
                assert!(max <= 192, "{tier}/{fmt} max diff {max} (bound 192)");
                assert!(mean < 12.0, "{tier}/{fmt} mean diff {mean:.2} (bound 12)");
            }
        }
    }
}
