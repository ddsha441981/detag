//! The intake suite — the P2-009 wiring (FR-U-03, FR-U-04, FR-U-05, FR-D-07).
//!
//! [`run_intake`] is the ONE intake path: it wires the P2-003–P2-008
//! parse-only pieces in a fixed order — sniff → guards → budgeted
//! decode → EXIF → ICC → orientation → hashes → merged metadata —
//! and returns everything the caller needs to store the result
//! (`hashes.dhash` for `uploads.phash`, the merged `metadata` object
//! for `uploads.metadata`). No later intake caller may invent a
//! second path: the guard/budget order can never be skipped by
//! going around this function.
//!
//! # Pipeline (each step's failure is a typed [`IntakeError`])
//!
//! 1. **Sniff** — `sniff_media` on the first/last 4096 bytes
//!    (`HEAD_SNIFF_LEN` / `TAIL_SNIFF_LEN`): the format is proved
//!    from the magic bytes, never the extension, and a polyglot
//!    (archive bytes in the tail, missing end marker) fails here
//!    as `AppError::UnsupportedMediaType` (PF-2001, 415).
//! 2. **Guards** — `check_intake_guards`: the plan byte cap, then
//!    the pixel budget from the header alone (PF-2003 / PF-2005,
//!    413). Nothing is decoded yet.
//! 3. **Budgeted decode** — `decode_budgeted`: the decoder's own
//!    `total_bytes()` decides the decoded size before any pixel is
//!    allocated (PF-2007, 413). At or under budget the image decodes
//!    exactly as `decode_image` would (16-bit stays 16-bit).
//! 4. **EXIF** — `parse_exif`. A present-but-malformed segment
//!    (`ExifError::Parse`) degrades to an EMPTY summary — never a
//!    failed intake (the P2-003 contract: the image is still
//!    decodable, hashable, storable; the metadata simply carries
//!    `"exif": {}`). An out-of-range orientation fails loudly
//!    instead: P2-005 would apply it into pixels.
//! 5. **ICC** — `extract_icc`. A missing or corrupt profile is
//!    absent (`bytes: None`), never an error; the only `Err` left
//!    is a decoder refusal, and the budgeted decode already built
//!    the same decoder on the same bytes, so that fails CLOSED.
//! 6. **Orientation** — `normalize_orientation`, applied BEFORE
//!    hashing, so the hashes describe the UPRIGHT pixels (P2-008).
//! 7. **Hashes** — `HashRecord::compute`: sha256 over the raw bytes,
//!    dHash/aHash over the normalized pixels.
//! 8. **Merge** — `merge_metadata_fragments` assembles the four
//!    fragments (`exif` / `icc` / `orientation` / `hashes`) into
//!    the single `uploads.metadata` object; a duplicate key fails
//!    closed instead of silently dropping a fragment.
//!
//! # What this does NOT do
//!
//! No database: the engine carries no pool, so the store half
//! (`set_phash` / `set_metadata`, core `repos::uploads`, both
//! guarded on `status = 'ready'`) belongs to the caller — the
//! job-create handler, later ticket. The live test in
//! `apps/api/tests/intake_suite.rs` proves the store half with a
//! real database. No HTTP endpoint runs this yet (job creation is
//! the 501 stub). No tiling: a huge image inside budget still
//! decodes whole ("tile-aware later" per P2-007).

use detag_core::error::AppError;
use detag_core::media::{sniff_media, MediaKind, HEAD_SNIFF_LEN, TAIL_SNIFF_LEN};
use image::{DynamicImage, GenericImageView};

use crate::budget::{decode_budgeted, DecodeBudget};
use crate::exif::{parse_exif, ExifError, ExifSummary};
use crate::guards::{check_intake_guards, GuardError};
use crate::hash::{merge_metadata_fragments, HashError, HashRecord};
use crate::icc::{extract_icc, IccError, IccProfile};
use crate::intake::DecodeError;
use crate::orientation::{normalize_orientation, OrientationError, OrientationRecord};

/// The outcome of a successful intake: the upright pixels plus
/// everything the caller needs to store (`hashes.dhash` →
/// `uploads.phash`, `metadata` → `uploads.metadata`).
#[derive(Debug)]
pub struct IntakeOutcome {
    /// The sniffed kind (== the format every step was forced to).
    pub kind: MediaKind,
    /// Upright width in pixels — the width AFTER orientation was
    /// applied (an orientation-6 image stored 4x3 reports 3 here;
    /// the guard's probed width is the stored width instead).
    pub width: u32,
    /// Upright height in pixels — the height AFTER orientation was
    /// applied.
    pub height: u32,
    /// Bit depth of one channel: 8, 16, or 32.
    pub bit_depth: u8,
    /// The pixels after the EXIF orientation was applied
    /// (unchanged when no orientation tag was present). 16-bit
    /// sources stay 16-bit.
    pub image: DynamicImage,
    /// The EXIF summary (empty when the file carries no EXIF, or
    /// when the segment was malformed — see the module docs).
    pub exif: ExifSummary,
    /// The extracted ICC profile (`bytes: None` when absent or
    /// corrupt).
    pub icc: IccProfile,
    /// What orientation normalization did (or that nothing was).
    pub orientation: OrientationRecord,
    /// The hash record: sha256 of the raw bytes + dHash/aHash of
    /// the upright pixels. `dhash` is what `uploads.phash` holds.
    pub hashes: HashRecord,
    /// The single merged metadata object: the exif / icc /
    /// orientation / hashes fragments under their own keys.
    pub metadata: serde_json::Value,
}

/// Why an intake failed. No panics: every failure is a value the
/// caller (job-create handler, Phase 2) maps to its own error code.
#[derive(Debug, thiserror::Error)]
pub enum IntakeError {
    /// Sniffing failed — not a supported media type (PF-2001, 415):
    /// unknown magic, a polyglot, or a truncated file.
    #[error("sniff failed: {0}")]
    Sniff(#[from] AppError),
    /// An intake guard rejected the image (PF-2003 / PF-2005, 413).
    #[error("intake guard rejected the image: {0}")]
    Guard(#[from] GuardError),
    /// Decoding failed (PF-2007, 413, when the budget was breached).
    #[error("decode failed: {0}")]
    Decode(#[from] DecodeError),
    /// EXIF failed — in practice only an out-of-range orientation
    /// (a malformed segment degrades to an empty summary instead).
    #[error("EXIF failed: {0}")]
    Exif(#[from] ExifError),
    /// ICC extraction failed (a decoder refusal after a successful
    /// decode — fails closed; a missing/corrupt profile is absent,
    /// never this error).
    #[error("ICC extraction failed: {0}")]
    Icc(#[from] IccError),
    /// Orientation normalization failed (out-of-range value).
    #[error("orientation failed: {0}")]
    Orientation(#[from] OrientationError),
    /// Hashing or the metadata merge failed.
    #[error("hash failed: {0}")]
    Hash(#[from] HashError),
}

impl IntakeError {
    /// The PF error code this intake failure maps to at the API layer,
    /// or `None` when the caller maps it to its own failure: the
    /// sniff (`Sniff` → PF-2001), the cap rejections (`Guard` →
    /// PF-2003/PF-2005) and the budget breach (`Decode` → PF-2007)
    /// carry codes; the EXIF/ICC/orientation/hash failures do not.
    pub fn pf_code(&self) -> Option<&'static str> {
        match self {
            IntakeError::Sniff(e) => Some(e.code()),
            IntakeError::Guard(e) => e.pf_code(),
            IntakeError::Decode(e) => e.pf_code(),
            _ => None,
        }
    }
}

/// Run the full intake on `bytes` under the plan byte cap `max_bytes`
/// and the decode budget `budget`.
///
/// The pipeline order is fixed (see the module docs): sniff →
/// guards → budgeted decode → EXIF → ICC → orientation → hashes →
/// merged metadata. Empty input fails as [`IntakeError::Sniff`];
/// video kinds fail as [`IntakeError::Guard`] (`NotAnImage`); a kind
/// whose cargo feature is off fails as [`IntakeError::Guard`]
/// (`FeatureDisabled`). The image is NEVER decoded before the guards
/// and the budget have both passed — a bomb is rejected from its
/// header alone.
pub fn run_intake(
    bytes: &[u8],
    max_bytes: u64,
    budget: DecodeBudget,
) -> Result<IntakeOutcome, IntakeError> {
    // 1. Sniff: the first/last 4096 bytes prove the kind; a polyglot
    // fails here, before anything else touches the input.
    let head_len = HEAD_SNIFF_LEN.min(bytes.len());
    let tail_start = bytes.len().saturating_sub(TAIL_SNIFF_LEN);
    let kind = sniff_media(&bytes[..head_len], &bytes[tail_start..], bytes.len() as u64)?;
    // 2. Guards: byte cap, then the pixel budget from the header.
    // The probed dims are the verdict's proof, not returned twice —
    // the outcome carries the UPRIGHT dims (post-normalization),
    // which equal the probed dims exactly when no orientation tag
    // was present; the tests prove that equality on unoriented
    // images rather than trusting it.
    check_intake_guards(bytes, kind, max_bytes)?;
    // 3. Budgeted decode: the decoder's own total_bytes() decides
    // before any pixel is allocated. The bit depth cannot change
    // under a rotation/flip, so it is read here; the dims are read
    // after normalization below.
    let decoded = decode_budgeted(bytes, kind, budget)?;
    let bit_depth = decoded.bit_depth();
    // 4. EXIF: a malformed segment degrades to an empty summary —
    // never a failed intake (the P2-003 contract). Only a
    // present-but-out-of-range orientation fails loudly.
    let exif = match parse_exif(bytes, kind) {
        Ok(summary) => summary,
        Err(ExifError::Parse(_)) => ExifSummary::default(),
        Err(e) => return Err(IntakeError::Exif(e)),
    };
    // 5. ICC: missing/corrupt is absent, never an error.
    let icc = extract_icc(bytes, kind)?;
    // 6. Orientation, BEFORE hashing: the hashes describe the
    // upright pixels. The outcome dims are the upright dims.
    let normalized = normalize_orientation(decoded.image, exif.orientation)?;
    let (width, height) = normalized.image.dimensions();
    // 7. Hashes: sha256 over the raw bytes, perceptual hashes over
    // the normalized pixels.
    let hashes = HashRecord::compute(bytes, &normalized.image)?;
    // 8. Merge: the four fragments into the single metadata object.
    let metadata = merge_metadata_fragments(&[
        exif.to_metadata_value(),
        icc.to_metadata_value(),
        normalized.record.to_metadata_value(),
        hashes.to_metadata_value(),
    ])?;
    Ok(IntakeOutcome {
        kind,
        width,
        height,
        bit_depth,
        image: normalized.image,
        exif,
        icc,
        orientation: normalized.record,
        hashes,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guards::check_intake_guards;
    use crate::hash::{hamming_distance_hex, perceptual_hashes};
    use crate::intake::decode_image;
    use detag_core::checksum::sha256_hex;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    #[cfg(feature = "tiff")]
    use image::codecs::tiff::TiffEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb, RgbImage};

    /// The Free plan's byte cap (seed value, `crates/core/src/plans.rs`):
    /// 25 MiB. A real plan value, not a made-up small number.
    const FREE_MAX_BYTES: u64 = 26_214_400;

    /// Committed bomb fixtures (`tests/fixtures/bombs/`, generated by
    /// `gen_bomb_fixtures.py` — the P2-006 precedent).
    const BOMB_PNG_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-png-20000x20000.png");
    const BOMB_PNG_8001: &[u8] = include_bytes!("../tests/fixtures/bombs/bomb-png-8001x8001.png");
    const BOMB_JPEG_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-jpeg-20000x20000.jpg");
    const BOMB_WEBP_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-webp-20000x20000.webp");
    #[cfg(feature = "tiff")]
    const BOMB_TIFF_20000: &[u8] =
        include_bytes!("../tests/fixtures/bombs/bomb-tiff-20000x20000.tiff");

    /// The committed 8000x8000 clean PNG (P2-006 boundary fixture):
    /// the largest image the dimension guard admits, 192 MB decoded
    /// as RGB8 — the real "largest admitted image" for the budget.
    const IMG_8K_PNG_CLEAN: &[u8] = include_bytes!("../../../fixtures/images/img-8k-png-clean.png");

    /// Committed ICC fixtures (`tests/fixtures/icc/`, P2-004).
    const ICC_PNG_SRGB: &[u8] = include_bytes!("../tests/fixtures/icc/png-srgb.png");
    const ICC_PNG_NO_ICC: &[u8] = include_bytes!("../tests/fixtures/icc/png-no-icc.png");
    const ICC_PNG_CORRUPT: &[u8] = include_bytes!("../tests/fixtures/icc/png-corrupt-icc.png");

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

    /// The ramp plus deterministic ±3 noise (LCG): a photo-like
    /// input for the near-duplicate test (the hash.rs precedent).
    fn gradient_noise_image() -> RgbImage {
        let mut img = RgbImage::new(64, 64);
        let mut s: u64 = 0x1234_5678_9abc_def0;
        for y in 0..64u32 {
            for x in 0..64u32 {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let noise = ((s >> 33) % 7) as u32; // 0..=6
                let v = (x * 4).saturating_add(noise).min(255) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        img
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

    /// A small asymmetric image: every pixel encodes its coordinates,
    /// so a wrong rotation/flip is visible at every pixel, not just at
    /// the edges. 4 wide × 3 high on purpose — non-square, so the
    /// dimension-swapping values (5–8) are exercised (the
    /// orientation.rs precedent).
    fn coord_rgb8(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x * 40) as u8, (y * 40) as u8, ((x + y) * 20) as u8])
        })
    }

    /// Where output pixel (x, y) comes FROM in the source, per the
    /// EXIF spec's row/column definition of each orientation value —
    /// derived from the spec table, INDEPENDENT of the implementation
    /// (the orientation.rs precedent).
    fn expected_source_coords(x: u32, y: u32, w: u32, h: u32, orientation: u16) -> (u32, u32) {
        match orientation {
            1 => (x, y),
            2 => (w - 1 - x, y),
            3 => (w - 1 - x, h - 1 - y),
            4 => (x, h - 1 - y),
            5 => (y, x),
            6 => (y, h - 1 - x),
            7 => (w - 1 - y, h - 1 - x),
            8 => (w - 1 - y, x),
            _ => unreachable!("test only calls this with 1..=8"),
        }
    }

    /// The expected output image for `src` under `orientation`,
    /// built from the spec mapping above.
    fn expected_rgb8(
        src: &ImageBuffer<Rgb<u8>, Vec<u8>>,
        orientation: u16,
    ) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        let (w, h) = src.dimensions();
        let (out_w, out_h) = if orientation >= 5 { (h, w) } else { (w, h) };
        ImageBuffer::from_fn(out_w, out_h, |x, y| {
            let (sx, sy) = expected_source_coords(x, y, w, h, orientation);
            *src.get_pixel(sx, sy)
        })
    }

    /// Minimal TIFF payload carrying ONLY an orientation tag (value 6,
    /// little-endian, one IFD0 entry): the bytes after `Exif\0\0` in a
    /// JPEG APP1 segment, or inside a PNG eXIf chunk (the
    /// orientation.rs precedent).
    fn tiff_payload_orientation_6() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // one IFD0 entry
        out.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        out.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        out.extend_from_slice(&1u32.to_le_bytes()); // count 1
        let mut value = [0u8; 4];
        value[..2].copy_from_slice(&6u16.to_le_bytes());
        out.extend_from_slice(&value);
        out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        out
    }

    /// CRC-32 (IEEE) over `data` — the PNG chunk CRC. Small and
    /// bitwise on purpose: no new dependency for one test helper.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A real PNG (lossless pixels) with an eXIf chunk carrying
    /// orientation 6, spliced in before IEND.
    fn png_with_orientation_6(img: &DynamicImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_with_encoder(PngEncoder::new(&mut buf))
            .expect("png encode");
        let payload = tiff_payload_orientation_6();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        chunk.extend_from_slice(b"eXIf");
        chunk.extend_from_slice(&payload);
        let mut crc_data = b"eXIf".to_vec();
        crc_data.extend_from_slice(&payload);
        chunk.extend_from_slice(&crc32(&crc_data).to_be_bytes());
        let iend = buf
            .windows(4)
            .rposition(|w| w == b"IEND")
            .expect("IEND present")
            - 4;
        let mut out = buf[..iend].to_vec();
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&buf[iend..]);
        out
    }

    /// A real JPEG (lossy pixels) with an APP1 segment carrying
    /// orientation 6, spliced in right after SOI.
    fn jpeg_with_orientation_6(img: &DynamicImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_with_encoder(JpegEncoder::new_with_quality(&mut buf, 90))
            .expect("jpeg encode");
        assert_eq!(&buf[..2], b"\xff\xd8", "JPEG starts with SOI");
        let payload = tiff_payload_orientation_6();
        let mut seg = b"Exif\0\0".to_vec();
        seg.extend_from_slice(&payload);
        let len = (seg.len() + 2) as u16; // length includes itself
        let mut out = buf[..2].to_vec();
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&seg);
        out.extend_from_slice(&buf[2..]);
        out
    }

    /// A real JPEG with an APP1 segment carrying a GARBAGE EXIF
    /// payload ("XX" byte order — no TIFF header can parse it).
    /// The sniffer and the decoder both accept the file (APP1 is
    /// skipped by the decoder); only the EXIF parse fails.
    fn jpeg_with_malformed_exif(img: &DynamicImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_with_encoder(JpegEncoder::new_with_quality(&mut buf, 90))
            .expect("jpeg encode");
        assert_eq!(&buf[..2], b"\xff\xd8", "JPEG starts with SOI");
        let mut seg = b"Exif\0\0".to_vec();
        seg.extend_from_slice(b"XX\x00\x2a\x00\x00\x00\x08\x00garbage-garbage");
        let len = (seg.len() + 2) as u16;
        let mut out = buf[..2].to_vec();
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&seg);
        out.extend_from_slice(&buf[2..]);
        out
    }

    /// Minimal MP4 bytes the sniffer accepts as Mp4 (the core
    /// media.rs precedent: ftyp + mdat, no real frames).
    fn minimal_mp4() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 24];
        v.extend_from_slice(b"ftypisom\x00\x00\x02\x00isomiso2");
        v.extend_from_slice(b"mdat\x00\x01\x02\x03");
        v
    }

    /// Minimal AVIF bytes the sniffer accepts as Avif (the core
    /// media.rs precedent). Only used by the feature-off test.
    #[cfg(not(feature = "avif"))]
    fn minimal_avif() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 24];
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(b"avif");
        v.extend_from_slice(b"\x00\x00\x00\x00avifmif1");
        v.extend_from_slice(b"mdat\x00\x01\x02\x03");
        v
    }

    /// ZIP bytes appended to a valid image: the polyglot shape
    /// (the core media.rs precedent).
    fn zip_tail() -> &'static [u8] {
        b"PK\x03\x04\x14\x00\x00\x00\x08\x00PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"
    }

    /// The four metadata keys every successful intake carries.
    fn assert_metadata_shape(outcome: &IntakeOutcome) {
        let meta = outcome.metadata.as_object().expect("metadata is an object");
        assert_eq!(meta.len(), 4, "four fragments: {meta:?}");
        for key in ["exif", "icc", "orientation", "hashes"] {
            assert!(meta.contains_key(key), "missing fragment {key}");
        }
        // The phash column value: the dHash, 16 lowercase hex chars.
        assert_eq!(outcome.hashes.dhash.len(), 16);
        assert!(outcome.hashes.dhash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            outcome.hashes.dhash,
            outcome.hashes.dhash.to_ascii_lowercase()
        );
    }

    #[test]
    fn format_matrix_png_jpeg_webp() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        for (bytes, kind) in [
            (encode_png8(&src), MediaKind::Png),
            (encode_jpeg(&src, 90), MediaKind::Jpeg),
            (encode_webp_lossless(&src), MediaKind::WebP),
        ] {
            let outcome = run_intake(&bytes, FREE_MAX_BYTES, DecodeBudget::default())
                .unwrap_or_else(|e| panic!("{kind:?} intakes: {e}"));
            assert_eq!(outcome.kind, kind);
            assert_eq!((outcome.width, outcome.height), (48, 32));
            assert_eq!(outcome.bit_depth, 8);
            assert_eq!(outcome.image.dimensions(), (48, 32));
            assert_metadata_shape(&outcome);
            // The decoded dims agree with the guard's probed dims —
            // the two header reads see the same file.
            let probed = check_intake_guards(&bytes, kind, FREE_MAX_BYTES)
                .unwrap_or_else(|e| panic!("{kind:?} guards: {e}"));
            assert_eq!(
                (outcome.width, outcome.height),
                (probed.width, probed.height)
            );
            assert_eq!(outcome.hashes.sha256, sha256_hex(&bytes));
            assert_eq!(outcome.orientation.applied, None);
            assert_eq!(outcome.orientation.transform.name(), "identity");
            assert!(!outcome.orientation.stripped);
        }
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn format_matrix_tiff() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_tiff(&src);
        let outcome =
            run_intake(&bytes, FREE_MAX_BYTES, DecodeBudget::default()).expect("tiff intakes");
        assert_eq!(outcome.kind, MediaKind::Tiff);
        assert_eq!((outcome.width, outcome.height), (48, 32));
        assert_eq!(outcome.bit_depth, 8);
        assert_metadata_shape(&outcome);
    }

    #[cfg(feature = "avif")]
    #[test]
    fn format_matrix_avif() {
        use image::codecs::avif::AvifEncoder;
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let mut buf = Vec::new();
        src.write_with_encoder(AvifEncoder::new_with_speed_quality(&mut buf, 6, 80))
            .expect("avif encode");
        let outcome =
            run_intake(&buf, FREE_MAX_BYTES, DecodeBudget::default()).expect("avif intakes");
        assert_eq!(outcome.kind, MediaKind::Avif);
        assert_eq!((outcome.width, outcome.height), (48, 32));
        assert_eq!(outcome.bit_depth, 8);
        assert_metadata_shape(&outcome);
    }

    #[cfg(not(feature = "avif"))]
    #[test]
    fn avif_disabled_without_feature() {
        let err = run_intake(&minimal_avif(), FREE_MAX_BYTES, DecodeBudget::default())
            .expect_err("avif off");
        assert!(
            matches!(
                err,
                IntakeError::Guard(GuardError::FeatureDisabled(MediaKind::Avif, "avif"))
            ),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), None);
    }

    #[test]
    fn polyglot_png_plus_zip_rejected() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let mut bytes = encode_png8(&src);
        bytes.extend_from_slice(zip_tail());
        let err = run_intake(&bytes, FREE_MAX_BYTES, DecodeBudget::default())
            .expect_err("png+zip rejected");
        assert!(matches!(err, IntakeError::Sniff(_)), "unexpected: {err}");
        assert_eq!(err.pf_code(), Some("PF-2001"));
    }

    #[test]
    fn polyglot_jpeg_plus_zip_rejected() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let mut bytes = encode_jpeg(&src, 90);
        bytes.extend_from_slice(zip_tail());
        let err = run_intake(&bytes, FREE_MAX_BYTES, DecodeBudget::default())
            .expect_err("jpeg+zip rejected");
        assert!(matches!(err, IntakeError::Sniff(_)), "unexpected: {err}");
        assert_eq!(err.pf_code(), Some("PF-2001"));
    }

    #[test]
    fn png_bomb_rejected() {
        assert!(BOMB_PNG_20000.len() < 2_000_000, "fixture is a bomb");
        let err = run_intake(BOMB_PNG_20000, FREE_MAX_BYTES, DecodeBudget::default())
            .expect_err("20000x20000 bomb rejected");
        assert!(
            matches!(
                err,
                IntakeError::Guard(GuardError::DimensionsExceeded {
                    width: 20000,
                    height: 20000
                })
            ),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), Some("PF-2005"));
    }

    #[test]
    fn png_one_over_side_cap_rejected() {
        let err = run_intake(BOMB_PNG_8001, FREE_MAX_BYTES, DecodeBudget::default())
            .expect_err("8001x8001 rejected");
        assert!(
            matches!(
                err,
                IntakeError::Guard(GuardError::DimensionsExceeded {
                    width: 8001,
                    height: 8001
                })
            ),
            "unexpected: {err}"
        );
    }

    #[test]
    fn jpeg_webp_header_bombs_rejected() {
        for (bytes, kind) in [
            (BOMB_JPEG_20000, MediaKind::Jpeg),
            (BOMB_WEBP_20000, MediaKind::WebP),
        ] {
            let err = run_intake(bytes, FREE_MAX_BYTES, DecodeBudget::default()).unwrap_err();
            assert!(
                matches!(
                    err,
                    IntakeError::Guard(GuardError::DimensionsExceeded {
                        width: 20000,
                        height: 20000
                    })
                ),
                "{kind:?}: unexpected: {err}"
            );
            assert_eq!(err.pf_code(), Some("PF-2005"));
        }
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_header_bomb_rejected() {
        let err = run_intake(BOMB_TIFF_20000, FREE_MAX_BYTES, DecodeBudget::default())
            .expect_err("tiff header bomb rejected");
        assert!(
            matches!(
                err,
                IntakeError::Guard(GuardError::DimensionsExceeded {
                    width: 20000,
                    height: 20000
                })
            ),
            "unexpected: {err}"
        );
    }

    #[test]
    fn byte_cap_rejected() {
        let src = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = encode_png8(&src);
        let cap = bytes.len() as u64 - 1;
        let err =
            run_intake(&bytes, cap, DecodeBudget::default()).expect_err("one byte over fails");
        assert!(
            matches!(err, IntakeError::Guard(GuardError::ByteCapExceeded { .. })),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), Some("PF-2003"));
    }

    #[test]
    fn budget_exceeded_rejected() {
        // The 8k fixture passes the guards (8000x8000 is the cap)
        // but its 192 MB decoded size breaches a 1 MiB budget —
        // rejected BEFORE any pixel is decoded.
        let err = run_intake(IMG_8K_PNG_CLEAN, FREE_MAX_BYTES, DecodeBudget::new(1 << 20))
            .expect_err("8k over a 1 MiB budget rejected");
        assert!(
            matches!(
                err,
                IntakeError::Decode(DecodeError::BudgetExceeded {
                    width: 8000,
                    height: 8000,
                    estimated_bytes: 192_000_000,
                    budget_bytes: 1_048_576,
                    ..
                })
            ),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), Some("PF-2007"));
    }

    #[test]
    fn orientation_6_png_exact_pixels() {
        // The AC, pixel-exact through the whole pipeline: the stored
        // 4x3 image carries orientation 6, so the outcome is the
        // upright 3x4 image — and the hashes describe THOSE pixels.
        let stored = DynamicImage::ImageRgb8(coord_rgb8(4, 3));
        let bytes = png_with_orientation_6(&stored);
        let outcome = run_intake(&bytes, FREE_MAX_BYTES, DecodeBudget::default())
            .expect("oriented png intakes");
        assert_eq!(outcome.kind, MediaKind::Png);
        assert_eq!((outcome.width, outcome.height), (3, 4));
        assert_eq!(outcome.orientation.applied, Some(6));
        assert_eq!(outcome.orientation.transform.name(), "rotate-90-cw");
        assert!(outcome.orientation.stripped);
        assert_eq!(outcome.metadata["orientation"]["transform"], "rotate-90-cw");
        assert_eq!(outcome.metadata["orientation"]["applied"], 6);
        assert_eq!(outcome.exif.orientation, Some(6));
        let DynamicImage::ImageRgb8(got) = outcome.image else {
            panic!("outcome is Rgb8");
        };
        let expected = expected_rgb8(&coord_rgb8(4, 3), 6);
        assert_eq!(got, expected, "PNG end-to-end golden pixels");
        // The hashes are on the UPRIGHT pixels: they equal the hashes
        // of the spec-mapped image, and differ from the hashes of the
        // stored (still-rotated) pixels.
        let upright = perceptual_hashes(&DynamicImage::ImageRgb8(expected)).expect("hash upright");
        assert_eq!(outcome.hashes.dhash, upright.dhash);
        assert_eq!(outcome.hashes.ahash, upright.ahash);
        let stored_decoded = decode_image(&bytes, MediaKind::Png).expect("stored decodes");
        let stored_hashes = perceptual_hashes(&stored_decoded.image).expect("hash stored");
        assert_ne!(
            outcome.hashes.dhash, stored_hashes.dhash,
            "hashing the stored pixels would key the wrong content"
        );
    }

    #[test]
    fn orientation_6_jpeg_exact_pixels() {
        // Same path on real JPEG bytes: exact against the spec-mapped
        // stored decode (both sides share the same lossy decode, so
        // exactness still holds — the orientation.rs precedent).
        let stored = DynamicImage::ImageRgb8(coord_rgb8(4, 3));
        let bytes = jpeg_with_orientation_6(&stored);
        let outcome = run_intake(&bytes, FREE_MAX_BYTES, DecodeBudget::default())
            .expect("oriented jpeg intakes");
        assert_eq!((outcome.width, outcome.height), (3, 4));
        assert_eq!(outcome.orientation.applied, Some(6));
        let DynamicImage::ImageRgb8(got) = outcome.image else {
            panic!("outcome is Rgb8");
        };
        let stored_decoded = decode_image(&bytes, MediaKind::Jpeg).expect("stored decodes");
        let DynamicImage::ImageRgb8(stored_pixels) = stored_decoded.image else {
            panic!("stored decode is Rgb8");
        };
        let expected = expected_rgb8(&stored_pixels, 6);
        assert_eq!(got, expected, "JPEG end-to-end golden pixels");
    }

    #[test]
    fn malformed_exif_still_intakes() {
        // A present-but-malformed EXIF segment degrades to an empty
        // summary — never a failed intake (the P2-003 contract).
        let stored = DynamicImage::ImageRgb8(gradient_rgb8(48, 32));
        let bytes = jpeg_with_malformed_exif(&stored);
        assert!(
            matches!(
                crate::exif::parse_exif(&bytes, MediaKind::Jpeg),
                Err(ExifError::Parse(_))
            ),
            "fixture really is malformed EXIF"
        );
        let outcome = run_intake(&bytes, FREE_MAX_BYTES, DecodeBudget::default())
            .expect("malformed EXIF still intakes");
        assert!(outcome.exif.is_empty(), "empty summary: {:?}", outcome.exif);
        assert_eq!(outcome.metadata["exif"], serde_json::json!({}));
        assert_eq!(outcome.orientation.applied, None);
        assert_metadata_shape(&outcome);
    }

    #[test]
    fn icc_preserved_srgb() {
        let outcome = run_intake(ICC_PNG_SRGB, FREE_MAX_BYTES, DecodeBudget::default())
            .expect("srgb png intakes");
        assert!(outcome.icc.present(), "fixture carries sRGB ICC");
        assert_eq!(outcome.metadata["icc"]["present"], true);
        assert_eq!(
            outcome.metadata["icc"]["sha256"],
            serde_json::Value::String(outcome.icc.sha256_hex().expect("profile hashes")),
            "metadata carries the profile fingerprint"
        );
        // The outcome's profile bytes are the extracted ones.
        let extracted =
            crate::icc::extract_icc(ICC_PNG_SRGB, MediaKind::Png).expect("icc extracts");
        assert_eq!(outcome.icc.bytes, extracted.bytes);
    }

    #[test]
    fn icc_absent_and_corrupt_never_fail_intake() {
        for (bytes, name) in [(ICC_PNG_NO_ICC, "no-icc"), (ICC_PNG_CORRUPT, "corrupt-icc")] {
            let outcome = run_intake(bytes, FREE_MAX_BYTES, DecodeBudget::default())
                .unwrap_or_else(|e| panic!("{name} intakes: {e}"));
            assert!(!outcome.icc.present(), "{name}: no profile");
            assert_eq!(outcome.metadata["icc"]["present"], false, "{name}");
            assert_metadata_shape(&outcome);
        }
    }

    #[test]
    fn hash_stability_same_bytes() {
        let first = run_intake(ICC_PNG_SRGB, FREE_MAX_BYTES, DecodeBudget::default())
            .expect("first intake");
        let second = run_intake(ICC_PNG_SRGB, FREE_MAX_BYTES, DecodeBudget::default())
            .expect("second intake");
        assert_eq!(first.hashes, second.hashes, "same bytes, same hashes");
        assert_eq!(first.metadata, second.metadata, "same bytes, same metadata");
        assert_eq!(first.hashes.sha256, sha256_hex(ICC_PNG_SRGB));
        assert_eq!(first.metadata["hashes"]["dhash"], first.hashes.dhash);
        assert_eq!(first.metadata["hashes"]["sha256"], first.hashes.sha256);
    }

    #[test]
    fn hash_stability_jpeg_reencode_close() {
        // The same pixels re-encoded lossy must stay a near-duplicate
        // by Hamming distance — the P2-008 bound (≤ 4), now proven
        // through the whole intake rather than the hash module alone.
        let img = DynamicImage::ImageRgb8(gradient_noise_image());
        let png = encode_png8(&img);
        let jpeg = encode_jpeg(&img, 85);
        let from_png =
            run_intake(&png, FREE_MAX_BYTES, DecodeBudget::default()).expect("png intakes");
        let from_jpeg =
            run_intake(&jpeg, FREE_MAX_BYTES, DecodeBudget::default()).expect("jpeg intakes");
        let d_dhash =
            hamming_distance_hex(&from_png.hashes.dhash, &from_jpeg.hashes.dhash).expect("dist");
        let d_ahash =
            hamming_distance_hex(&from_png.hashes.ahash, &from_jpeg.hashes.ahash).expect("dist");
        assert!(d_dhash <= 4, "dHash distance {d_dhash} > 4");
        assert!(d_ahash <= 4, "aHash distance {d_ahash} > 4");
    }

    #[test]
    fn sixteen_bit_png_stays_sixteen_bit() {
        let mut img16 = image::ImageBuffer::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                let v = (x * 4096) as u16;
                img16.put_pixel(x, y, image::Rgb([v, v, v]));
            }
        }
        let mut buf = Vec::new();
        // PngEncoder expects NATIVE-endian u16 samples (the hash.rs
        // precedent: big-endian bytes come back byte-swapped).
        let raw_ne: Vec<u8> = img16
            .into_raw()
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        use image::ImageEncoder;
        PngEncoder::new(&mut buf)
            .write_image(&raw_ne, 16, 16, image::ExtendedColorType::Rgb16)
            .expect("png16 encode");
        let outcome =
            run_intake(&buf, FREE_MAX_BYTES, DecodeBudget::default()).expect("16-bit png intakes");
        assert_eq!(outcome.bit_depth, 16);
        assert_eq!(outcome.hashes.dhash.len(), 16);
        assert_eq!(
            outcome.hashes.dhash, "ffffffffffffffff",
            "same ramp as 8-bit"
        );
    }

    #[test]
    fn video_kind_rejected() {
        let err = run_intake(&minimal_mp4(), FREE_MAX_BYTES, DecodeBudget::default())
            .expect_err("mp4 rejected");
        assert!(
            matches!(
                err,
                IntakeError::Guard(GuardError::NotAnImage(MediaKind::Mp4))
            ),
            "unexpected: {err}"
        );
        assert_eq!(err.pf_code(), None);
    }

    #[test]
    fn empty_input_rejected() {
        let err =
            run_intake(&[], FREE_MAX_BYTES, DecodeBudget::default()).expect_err("empty rejected");
        assert!(matches!(err, IntakeError::Sniff(_)), "unexpected: {err}");
        assert_eq!(err.pf_code(), Some("PF-2001"));
    }

    #[test]
    fn pf_codes_map() {
        assert_eq!(
            IntakeError::Sniff(AppError::UnsupportedMediaType).pf_code(),
            Some("PF-2001")
        );
        assert_eq!(
            IntakeError::Guard(GuardError::ByteCapExceeded {
                bytes: 11,
                max_bytes: 10
            })
            .pf_code(),
            Some("PF-2003")
        );
        assert_eq!(
            IntakeError::Guard(GuardError::DimensionsExceeded {
                width: 20000,
                height: 20000
            })
            .pf_code(),
            Some("PF-2005")
        );
        assert_eq!(
            IntakeError::Decode(DecodeError::BudgetExceeded {
                width: 8000,
                height: 8000,
                color: image::ColorType::Rgb8,
                estimated_bytes: 192_000_000,
                budget_bytes: 1_048_576,
            })
            .pf_code(),
            Some("PF-2007")
        );
        assert_eq!(
            IntakeError::Exif(ExifError::InvalidOrientation(9)).pf_code(),
            None
        );
        assert_eq!(IntakeError::Icc(IccError::EmptyInput).pf_code(), None);
        assert_eq!(
            IntakeError::Orientation(OrientationError::InvalidOrientation(9)).pf_code(),
            None
        );
        assert_eq!(IntakeError::Hash(HashError::EmptyImage).pf_code(), None);
    }
}
