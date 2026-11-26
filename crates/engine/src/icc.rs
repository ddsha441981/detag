//! ICC profile extraction — the E2.1 intake ICC handler (P2-004, FR-D-07).
//!
//! [`extract_icc`] turns sniffed image bytes into an [`IccProfile`]:
//! the raw ICC blob plus a presence flag — exactly what the spec
//! extracts, nothing else. The mechanism is the `image` crate's
//! `ImageDecoder::icc_profile()` (verified against the 0.25.10 sources,
//! not docs alone): JPEG via zune-jpeg (APP2 `ICC_PROFILE` chunks),
//! PNG via the png crate (`Info.icc_profile`, iCCP), TIFF via tag
//! 34675, WebP via the ICCP chunk, AVIF via the dav1d colour info
//! (feature-gated). No new dependency. One wrinkle, proven by probe:
//! TIFF does NOT go through `ImageReader` — image 0.25.10's
//! `TiffDecoder::set_limits` (called by `into_decoder`) sets the tiff
//! crate's `decoding_buffer_size` to the pixel-buffer size, and the
//! tiff crate reuses that limit to cap IFD value counts, so any real
//! profile reads as absent; constructing `TiffDecoder::new` directly
//! keeps the tiff crate's own defaults and the tag reads fine
//! (ADR-0073).
//!
//! The extractor never inspects the profile bytes — no header, magic,
//! or version check — so an unknown profile passes through verbatim,
//! never dropped or replaced with sRGB (ADR-0073).
//!
//! # Error semantics (what a caller should do with each outcome)
//!
//! * `Ok` with `bytes: None` — the image carries no ICC slot at all,
//!   or the slot is corrupt. That is normal (most images have none),
//!   not a failure: the decoders themselves report corrupt ICC as
//!   absent (zune returns `None` on mismatched chunk counts / seq 0 /
//!   duplicate seq; png swallows iCCP errors; tiff tag errors collapse
//!   to `None`; AVIF maps a corrupt profile to an empty vec, which is
//!   normalized to absent here). A corrupt profile cannot be preserved
//!   by definition, and the pixels are unaffected — metadata must never
//!   fail an intake over it (ADR-0073).
//! * `Err(IccError::Decode(..))` — the container itself failed
//!   (`into_decoder` refused the bytes: truncated, corrupt, or not the
//!   forced format at all).
//! * `Err(IccError::NotAnImage(..))` / `FeatureDisabled(..)` /
//!   `EmptyInput` — the P2-002/P2-003 error vocabulary.
//!
//! The extractor never panics on any input: every malformed shape is a
//! value (`Ok(None)` or a typed error), proven by the truncated /
//! garbage / corrupt-fixture unit tests below.
//!
//! # Where the profile goes
//!
//! [`IccProfile::to_metadata_value`] serializes the profile to the
//! exact `uploads.metadata` jsonb fragment (`{"icc": {"present": …,
//! "sha256": …, "len": …}}` — the `icc` key namespaces it alongside
//! `exif` (ADR-0072 §5), so P2-008 merges without collisions). The
//! fragment carries a SHA-256 fingerprint + length, NOT the blob
//! itself: the blob flows extractor → encode (P2-063 embeds it), it is
//! not duplicated into jsonb. The WRITE itself lands in P2-008, the
//! first ticket that persists intake results onto `uploads` — there is
//! no intake caller yet (job creation is still the 501 stub,
//! `decode_image`'s only caller is its bench), so P2-004 ships
//! parse-only with the shape proven (ADR-0073). No migration:
//! `uploads.metadata jsonb` already exists (migration
//! `20261112064924`, `DEFAULT '{}'::jsonb`).
//!
//! # What this does NOT do
//!
//! No ICC embedding on encode — P2-063 embeds the blob for all
//! supporting formats and binary-compares post-encode. No ICC
//! validation, conversion, or colour management — the blob is opaque
//! bytes to this module; interpreting it is a reader's job, not the
//! intake's.

use detag_core::media::MediaKind;
use image::{ImageDecoder, ImageFormat, ImageReader};
use sha2::{Digest, Sha256};
use std::io::Cursor;

/// The extracted ICC profile: the raw blob, or nothing.
///
/// `bytes: None` means absent OR corrupt (see the module docs — the two
/// are indistinguishable at the decoder APIs, and both mean "no usable
/// profile"). The caller embeds `bytes` verbatim when present (P2-063);
/// it never needs to parse the blob.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IccProfile {
    /// The raw ICC profile bytes, exactly as the container held them.
    /// `None` when the image carries no ICC slot (or the slot is
    /// corrupt — see the module docs).
    pub bytes: Option<Vec<u8>>,
}

impl IccProfile {
    /// True when the image carried a usable ICC profile.
    pub fn present(&self) -> bool {
        self.bytes.is_some()
    }

    /// Whether the image carries no ICC profile at all.
    /// (Clippy's `len_without_is_empty` requires this next to `len`.)
    pub fn is_empty(&self) -> bool {
        self.bytes.is_none()
    }

    /// Length of the profile in bytes, when present.
    pub fn len(&self) -> Option<usize> {
        self.bytes.as_ref().map(Vec::len)
    }

    /// SHA-256 of the profile bytes as lowercase hex, when present —
    /// the fingerprint the metadata fragment records (and P2-063 can
    /// re-check after the encode roundtrip).
    pub fn sha256_hex(&self) -> Option<String> {
        self.bytes.as_ref().map(|b| {
            let mut hasher = Sha256::new();
            hasher.update(b);
            hex_encode(hasher.finalize().as_slice())
        })
    }

    /// The `uploads.metadata` jsonb fragment this profile becomes:
    /// `{"icc": {"present": true, "sha256": "<hex>", "len": N}}`, or
    /// `{"icc": {"present": false}}` when absent. The blob itself is
    /// NOT in the fragment — it flows to P2-063, not into jsonb. The
    /// write lands in P2-008; this only proves the shape (ADR-0073).
    pub fn to_metadata_value(&self) -> serde_json::Value {
        match &self.bytes {
            Some(bytes) => serde_json::json!({
                "icc": {
                    "present": true,
                    "sha256": self.sha256_hex(),
                    "len": bytes.len(),
                }
            }),
            None => serde_json::json!({ "icc": { "present": false } }),
        }
    }
}

/// Lowercase hex encoding (no extra dependency for one small helper).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Why an ICC extraction failed. No panics: every failure is a value.
#[derive(Debug, thiserror::Error)]
pub enum IccError {
    /// The kind is a video kind, not an image — the video intake
    /// (Phase 3) owns it.
    #[error("not an image kind: {0:?}")]
    NotAnImage(MediaKind),
    /// Nothing to extract from.
    #[error("empty input")]
    EmptyInput,
    /// The kind needs a cargo feature this build does not have
    /// (`tiff` / `avif` — see the module docs).
    #[error("format {0:?} needs cargo feature `{1}` (not enabled in this build)")]
    FeatureDisabled(MediaKind, &'static str),
    /// The `image` crate refused the bytes (truncated, corrupt, or
    /// not the forced format at all). A corrupt ICC *slot* is NOT
    /// this error — it is `Ok` with `bytes: None` (see module docs).
    #[error("ICC extraction failed: {0}")]
    Decode(String),
}

/// Extract the ICC profile from `bytes` of the sniffed `kind` (P2-001).
///
/// The format is forced on the decoder — the bytes are never
/// re-guessed — so a JPEG passed with `kind == MediaKind::Png` fails
/// instead of extracting as JPEG. Empty input fails as
/// [`IccError::EmptyInput`]; video kinds fail as
/// [`IccError::NotAnImage`]; a kind whose cargo feature is off fails
/// as [`IccError::FeatureDisabled`]; a container the decoder refuses
/// fails as [`IccError::Decode`]. No ICC slot (or a corrupt one) is
/// `Ok` with `bytes: None` — never an error, never a panic.
pub fn extract_icc(bytes: &[u8], kind: MediaKind) -> Result<IccProfile, IccError> {
    if bytes.is_empty() {
        return Err(IccError::EmptyInput);
    }
    let format = match kind {
        MediaKind::Png => ImageFormat::Png,
        MediaKind::Jpeg => ImageFormat::Jpeg,
        MediaKind::WebP => ImageFormat::WebP,
        #[cfg(feature = "tiff")]
        MediaKind::Tiff => {
            // TIFF bypasses ImageReader: image 0.25.10's
            // TiffDecoder::set_limits (called by into_decoder) sets the
            // tiff crate's decoding_buffer_size to the pixel-buffer
            // size, and the tiff crate reuses that limit to cap IFD
            // value counts — any real profile (> ~24 values) then reads
            // as absent (proven by probe: direct TiffDecoder::new reads
            // the 588-byte profile, after set_limits it reads None).
            // Constructing TiffDecoder::new directly keeps the tiff
            // crate's own defaults (256 MiB) and the tag reads fine
            // (ADR-0073).
            let mut decoder = image::codecs::tiff::TiffDecoder::new(Cursor::new(bytes))
                .map_err(|e| IccError::Decode(e.to_string()))?;
            let bytes = decoder
                .icc_profile()
                .ok()
                .flatten()
                .filter(|b| !b.is_empty());
            return Ok(IccProfile { bytes });
        }
        #[cfg(not(feature = "tiff"))]
        MediaKind::Tiff => return Err(IccError::FeatureDisabled(kind, "tiff")),
        #[cfg(feature = "avif")]
        MediaKind::Avif => ImageFormat::Avif,
        #[cfg(not(feature = "avif"))]
        MediaKind::Avif => return Err(IccError::FeatureDisabled(kind, "avif")),
        MediaKind::Mp4 | MediaKind::Mkv | MediaKind::Webm => {
            return Err(IccError::NotAnImage(kind));
        }
    };
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .map_err(|e| IccError::Decode(e.to_string()))?;
    // The trait call itself can fail (WebP's chunk read) — by then the
    // container is structurally valid, so the failure is ICC-slot
    // specific: absent, not a failed intake (see module docs).
    // An empty blob (AVIF's corrupt-profile mapping) is absent too.
    let bytes = decoder
        .icc_profile()
        .ok()
        .flatten()
        .filter(|b| !b.is_empty());
    Ok(IccProfile { bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::media::MediaKind;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    #[cfg(feature = "tiff")]
    use image::codecs::tiff::TiffEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{DynamicImage, ImageBuffer, ImageEncoder, Rgb};

    /// Committed profiles (lcms2-built; see
    /// `tests/fixtures/icc/gen_icc_fixtures.py`).
    const SRGB_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/srgb.icc");
    const P3_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/display-p3.icc");
    const TEST_RGB_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/test-rgb.icc");

    /// Committed Pillow images embedding those profiles (or none).
    const PNG_SRGB: &[u8] = include_bytes!("../tests/fixtures/icc/png-srgb.png");
    const PNG_P3: &[u8] = include_bytes!("../tests/fixtures/icc/png-p3.png");
    const PNG_TEST_RGB: &[u8] = include_bytes!("../tests/fixtures/icc/png-test-rgb.png");
    const PNG_NO_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/png-no-icc.png");
    const PNG_CORRUPT: &[u8] = include_bytes!("../tests/fixtures/icc/png-corrupt-icc.png");
    const JPEG_SRGB: &[u8] = include_bytes!("../tests/fixtures/icc/jpeg-srgb.jpg");
    const JPEG_P3: &[u8] = include_bytes!("../tests/fixtures/icc/jpeg-p3.jpg");
    const JPEG_NO_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/jpeg-no-icc.jpg");
    const JPEG_CORRUPT: &[u8] = include_bytes!("../tests/fixtures/icc/jpeg-corrupt-icc.jpg");
    const WEBP_SRGB: &[u8] = include_bytes!("../tests/fixtures/icc/webp-srgb.webp");
    const WEBP_NO_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/webp-no-icc.webp");
    const TIFF_SRGB: &[u8] = include_bytes!("../tests/fixtures/icc/tiff-srgb.tiff");
    const TIFF_NO_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/tiff-no-icc.tiff");

    /// The sRGB profile's SHA-256, pinned — the fingerprint the
    /// metadata fragment records must match the committed bytes.
    const SRGB_SHA256: &str = "2a406fd5adfe72488c9ec40d150102aef1c91673e0e395fd8969be96af23e8ba";

    /// Deterministic 8-bit RGB gradient (same shape as intake's).
    fn gradient_rgb8(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([
                (x.wrapping_mul(7).wrapping_add(y.wrapping_mul(3)) % 256) as u8,
                (x.wrapping_mul(3).wrapping_add(y.wrapping_mul(11)) % 256) as u8,
                (x.wrapping_add(y).wrapping_mul(5) % 256) as u8,
            ])
        })
    }

    fn extract(bytes: &[u8], kind: MediaKind) -> IccProfile {
        extract_icc(bytes, kind).expect("extraction must not fail on fixtures")
    }

    #[test]
    fn png_srgb_extracts_byte_identical() {
        let profile = extract(PNG_SRGB, MediaKind::Png);
        assert!(profile.present());
        assert_eq!(profile.bytes.as_deref(), Some(SRGB_ICC));
    }

    #[test]
    fn png_p3_extracts_byte_identical() {
        let profile = extract(PNG_P3, MediaKind::Png);
        assert_eq!(profile.bytes.as_deref(), Some(P3_ICC));
        assert_ne!(profile.bytes.as_deref(), Some(SRGB_ICC));
    }

    #[test]
    fn png_unknown_profile_passes_through_verbatim() {
        // TestRGB is no standard profile: it must come back byte-for-byte,
        // never dropped, replaced, or "corrected" to sRGB.
        let profile = extract(PNG_TEST_RGB, MediaKind::Png);
        assert_eq!(profile.bytes.as_deref(), Some(TEST_RGB_ICC));
    }

    #[test]
    fn jpeg_srgb_and_p3_extract_byte_identical() {
        assert_eq!(
            extract(JPEG_SRGB, MediaKind::Jpeg).bytes.as_deref(),
            Some(SRGB_ICC)
        );
        assert_eq!(
            extract(JPEG_P3, MediaKind::Jpeg).bytes.as_deref(),
            Some(P3_ICC)
        );
    }

    #[test]
    fn webp_srgb_extracts_byte_identical() {
        assert_eq!(
            extract(WEBP_SRGB, MediaKind::WebP).bytes.as_deref(),
            Some(SRGB_ICC)
        );
    }

    #[test]
    #[cfg(feature = "tiff")]
    fn tiff_srgb_extracts_byte_identical() {
        assert_eq!(
            extract(TIFF_SRGB, MediaKind::Tiff).bytes.as_deref(),
            Some(SRGB_ICC)
        );
    }

    #[test]
    #[cfg(not(feature = "tiff"))]
    fn tiff_disabled_without_feature() {
        assert!(matches!(
            extract_icc(TIFF_SRGB, MediaKind::Tiff),
            Err(IccError::FeatureDisabled(MediaKind::Tiff, "tiff"))
        ));
    }

    #[test]
    fn no_icc_is_absent_not_an_error() {
        for (bytes, kind) in [
            (PNG_NO_ICC, MediaKind::Png),
            (JPEG_NO_ICC, MediaKind::Jpeg),
            (WEBP_NO_ICC, MediaKind::WebP),
        ] {
            assert_no_icc(bytes, kind);
        }
        #[cfg(feature = "tiff")]
        assert_no_icc(TIFF_NO_ICC, MediaKind::Tiff);
    }

    fn assert_no_icc(bytes: &[u8], kind: MediaKind) {
        let profile = extract(bytes, kind);
        assert!(!profile.present(), "{kind:?} should be absent");
        assert_eq!(profile.bytes, None);
        assert_eq!(profile.len(), None);
        assert_eq!(profile.sha256_hex(), None);
        assert_eq!(
            profile.to_metadata_value(),
            serde_json::json!({ "icc": { "present": false } })
        );
    }

    #[test]
    fn corrupt_icc_is_absent_not_a_panic() {
        // PNG iCCP zlib stream flipped (CRC repaired); JPEG APP2 chunk
        // count 1 -> 2. Both decoders report no ICC — never an error,
        // never a panic (ADR-0073 §2).
        for (bytes, kind) in [
            (PNG_CORRUPT, MediaKind::Png),
            (JPEG_CORRUPT, MediaKind::Jpeg),
        ] {
            let profile = extract(bytes, kind);
            assert!(!profile.present(), "{kind:?} corrupt ICC must be absent");
        }
    }

    #[test]
    fn truncated_and_garbage_never_panic() {
        // Every prefix of a valid file: truncated at any point.
        for cut in [2, 4, 8, 12, 20, 30, 50, PNG_SRGB.len() - 1] {
            let bytes = &PNG_SRGB[..cut];
            // Error or (for a cut that still holds a whole chunk) a
            // profile — never a panic.
            let _ = extract_icc(bytes, MediaKind::Png);
            let _ = extract_icc(bytes, MediaKind::Jpeg);
        }
        // Random garbage, wrong magic, and a PNG whose first chunk lies
        // about its length.
        for garbage in [
            vec![0u8; 64],
            vec![0xffu8; 128],
            b"not an image at all, just text".to_vec(),
            {
                let mut b = PNG_SRGB.to_vec();
                b[8..12].copy_from_slice(&9999u32.to_be_bytes()); // IHDR past EOF
                b
            },
        ] {
            let _ = extract_icc(&garbage, MediaKind::Png);
            let _ = extract_icc(&garbage, MediaKind::Jpeg);
            let _ = extract_icc(&garbage, MediaKind::WebP);
            let _ = extract_icc(&garbage, MediaKind::Tiff);
        }
    }

    #[test]
    fn empty_input_and_video_kinds_fail_typed() {
        assert!(matches!(
            extract_icc(&[], MediaKind::Png),
            Err(IccError::EmptyInput)
        ));
        for kind in [MediaKind::Mp4, MediaKind::Mkv, MediaKind::Webm] {
            assert!(matches!(
                extract_icc(PNG_SRGB, kind),
                Err(IccError::NotAnImage(k))
                if k == kind
            ));
        }
    }

    #[test]
    fn wrong_format_fails_not_extracts() {
        // The format is forced: a PNG passed as JPEG fails instead of
        // extracting as PNG (the P2-002 magic-wins rule, end to end).
        assert!(matches!(
            extract_icc(PNG_SRGB, MediaKind::Jpeg),
            Err(IccError::Decode(_))
        ));
        assert!(matches!(
            extract_icc(JPEG_SRGB, MediaKind::Png),
            Err(IccError::Decode(_))
        ));
    }

    #[test]
    fn metadata_fragment_shape() {
        let profile = extract(PNG_SRGB, MediaKind::Png);
        assert!(profile.present());
        assert_eq!(profile.len(), Some(588));
        assert_eq!(profile.sha256_hex().as_deref(), Some(SRGB_SHA256));
        assert_eq!(
            profile.to_metadata_value(),
            serde_json::json!({
                "icc": { "present": true, "sha256": SRGB_SHA256, "len": 588 }
            })
        );
        // The fragment namespaces cleanly beside the EXIF fragment.
        let merged = serde_json::json!({
            "exif": { "orientation": 6 },
            "icc": profile.to_metadata_value()["icc"],
        });
        assert_eq!(merged["icc"]["sha256"], SRGB_SHA256);
    }

    /// Encode `img` with `profile` via `encoder`, then extract the
    /// profile back — the AC's byte-identical roundtrip, through the
    /// image crate's own encoders (the same ones P2-063 will use).
    fn roundtrip<F>(img: &DynamicImage, profile: &[u8], kind: MediaKind, mut encode: F)
    where
        F: FnMut(&DynamicImage, Vec<u8>) -> Vec<u8>,
    {
        let bytes = encode(img, profile.to_vec());
        let back = extract(&bytes, kind);
        assert_eq!(back.bytes.as_deref(), Some(profile));
    }

    #[test]
    fn png_roundtrip_byte_identical() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(16, 16));
        roundtrip(&img, SRGB_ICC, MediaKind::Png, |img, profile| {
            let mut buf = Vec::new();
            let mut enc = PngEncoder::new(&mut buf);
            enc.set_icc_profile(profile).expect("set icc");
            img.write_with_encoder(enc).expect("png encode");
            buf
        });
    }

    #[test]
    fn jpeg_roundtrip_byte_identical() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(16, 16));
        roundtrip(&img, P3_ICC, MediaKind::Jpeg, |img, profile| {
            let mut buf = Vec::new();
            let mut enc = JpegEncoder::new_with_quality(&mut buf, 95);
            enc.set_icc_profile(profile).expect("set icc");
            img.write_with_encoder(enc).expect("jpeg encode");
            buf
        });
    }

    #[test]
    fn webp_roundtrip_byte_identical() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(16, 16));
        roundtrip(&img, SRGB_ICC, MediaKind::WebP, |img, profile| {
            let mut buf = Vec::new();
            let mut enc = WebPEncoder::new_lossless(&mut buf);
            enc.set_icc_profile(profile).expect("set icc");
            img.write_with_encoder(enc).expect("webp encode");
            buf
        });
    }

    #[test]
    #[cfg(feature = "tiff")]
    fn tiff_roundtrip_byte_identical() {
        let img = DynamicImage::ImageRgb8(gradient_rgb8(16, 16));
        roundtrip(&img, SRGB_ICC, MediaKind::Tiff, |img, profile| {
            // TiffEncoder needs Write + Seek; a Vec alone does not seek.
            let mut cursor = Cursor::new(Vec::new());
            let mut enc = TiffEncoder::new(&mut cursor);
            enc.set_icc_profile(profile).expect("set icc");
            img.write_with_encoder(enc).expect("tiff encode");
            cursor.into_inner()
        });
    }
}
