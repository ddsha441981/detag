//! EXIF summary extraction — the E2.1 intake EXIF parser (P2-003, FR-U-06, FR-D-07).
//!
//! [`parse_exif`] turns sniffed image bytes into an [`ExifSummary`]:
//! orientation, camera (make + model), and software — exactly the
//! fields the spec extracts, nothing else. GPS and every other tag
//! are never read and never stored (privacy — ADR-0072).
//!
//! The parser is [`kamadak-exif`] (`exif::Reader::read_from_container`),
//! pure Rust, no system deps. It reads the EXIF segment out of the
//! container itself (JPEG APP1, PNG eXIf, WebP EXIF chunk, TIFF IFD,
//! AVIF/HEIF meta box), so the caller passes the P2-001-sniffed
//! [`MediaKind`] only to reject video kinds up front — the format is
//! never re-guessed from the bytes.
//!
//! # Error semantics (what a caller should do with each outcome)
//!
//! * `Ok` with an empty summary — the image carries no EXIF segment
//!   at all. That is normal (most images have none), not a failure.
//! * `Err(ExifError::Parse(..))` — the EXIF segment exists but is
//!   malformed. The pixels are still fine; the caller should treat
//!   this as "no usable EXIF" and continue without it, never fail
//!   the intake over metadata.
//! * `Err(ExifError::InvalidOrientation(..))` — the orientation tag
//!   is present but outside 1..=8. P2-005 applies orientation into
//!   pixels, so a bogus value must fail loudly here rather than
//!   corrupt the output later.
//! * String fields (make / model / software) are informational: a
//!   wrong-typed or empty value is skipped (`None`), it never fails
//!   the summary.
//!
//! The parser never panics on any input: every malformed shape is a
//! typed error, proven by the `exif_parse` fuzz target
//! (`crates/engine/fuzz/`, runs nightly — ADR-0072) and by the
//! truncated/garbage unit tests below.
//!
//! # Where the summary goes
//!
//! [`ExifSummary::to_metadata_value`] serializes the summary to the
//! exact `uploads.metadata` jsonb fragment (`{"exif": {...}}` — the
//! `exif` key namespaces it so later intake fields, ICC in P2-004
//! and hashes in P2-008, share the object without collisions). The
//! WRITE itself lands in P2-008, the first ticket that persists
//! intake results onto `uploads` — there is no intake caller yet
//! (job creation is still the 501 stub, `decode_image`'s only caller
//! is its bench), so P2-003 ships parse-only with the shape proven
//! (ADR-0072). No migration: `uploads.metadata jsonb` already exists
//! (migration `20261112064924`, `DEFAULT '{}'::jsonb`).
//!
//! # What this does NOT do
//!
//! No orientation application — P2-005 applies the orientation into
//! pixels and records the transform. No EXIF stripping — the output
//! strip is E2.6 (ADR-0006: EXIF stripped by default, orientation
//! applied, ICC preserved). No ICC handling — P2-004.

use detag_core::media::MediaKind;
use exif::{In, Reader, Tag, Value};
use serde::Serialize;
use std::io::Cursor;

/// The EXIF summary: orientation, camera, software — and nothing else.
///
/// All fields are `Option`: absent in the file means absent here.
/// Serializes with absent fields omitted, so a no-EXIF image is
/// `{"exif": {}}` once wrapped by [`ExifSummary::to_metadata_value`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExifSummary {
    /// EXIF orientation, 1..=8 (1 = normal). `None` when the tag is
    /// absent. Present-but-out-of-range fails the parse instead —
    /// see [`parse_exif`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u16>,
    /// Camera manufacturer (`Tag::Make`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    /// Camera model (`Tag::Model`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Software that wrote the file (`Tag::Software`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
}

impl ExifSummary {
    /// True when no EXIF field was present at all.
    pub fn is_empty(&self) -> bool {
        self.orientation.is_none()
            && self.make.is_none()
            && self.model.is_none()
            && self.software.is_none()
    }

    /// The `uploads.metadata` jsonb fragment this summary becomes:
    /// `{"exif": {"orientation": 6, "make": …, "model": …,
    /// "software": …}}` with absent fields omitted. The write lands
    /// in P2-008; this only proves the shape (ADR-0072).
    pub fn to_metadata_value(&self) -> serde_json::Value {
        serde_json::json!({ "exif": self })
    }
}

/// Why an EXIF parse failed. No panics: every failure is a value.
#[derive(Debug, thiserror::Error)]
pub enum ExifError {
    /// The kind is a video kind, not an image — the video intake
    /// (Phase 3) owns it.
    #[error("not an image kind: {0:?}")]
    NotAnImage(MediaKind),
    /// Nothing to parse.
    #[error("empty input")]
    EmptyInput,
    /// The EXIF segment exists but is malformed (truncated, bad
    /// offsets, unknown container). The caller should treat this as
    /// "no usable EXIF", never as a failed intake.
    #[error("EXIF parse failed: {0}")]
    Parse(String),
    /// The orientation tag is present but outside the EXIF range
    /// 1..=8. Fails loudly on purpose: P2-005 applies this value
    /// into pixels, so a bogus value must never reach it.
    #[error("EXIF orientation {0} is out of range (1..=8)")]
    InvalidOrientation(u16),
}

/// Parse the EXIF summary out of `bytes` of the sniffed `kind` (P2-001).
///
/// Empty input fails as [`ExifError::EmptyInput`]; video kinds fail
/// as [`ExifError::NotAnImage`]; a container with no EXIF segment
/// returns an empty summary (`Ok`, all `None`) — that is normal, not
/// an error. A present-but-malformed segment fails as
/// [`ExifError::Parse`]; an out-of-range orientation fails as
/// [`ExifError::InvalidOrientation`]. Never panics on any input.
pub fn parse_exif(bytes: &[u8], kind: MediaKind) -> Result<ExifSummary, ExifError> {
    if bytes.is_empty() {
        return Err(ExifError::EmptyInput);
    }
    match kind {
        MediaKind::Mp4 | MediaKind::Mkv | MediaKind::Webm => {
            return Err(ExifError::NotAnImage(kind));
        }
        MediaKind::Png | MediaKind::Jpeg | MediaKind::WebP | MediaKind::Tiff | MediaKind::Avif => {}
    }
    // The library re-detects the container from the bytes itself
    // (JPEG APP1 / PNG eXIf / WebP EXIF / TIFF IFD / AVIF meta box);
    // the sniffed kind only gates video out above. A container the
    // library cannot recognise is malformed input, not a missing EXIF.
    let mut cursor = Cursor::new(bytes);
    let exif = match Reader::new().read_from_container(&mut cursor) {
        Ok(exif) => exif,
        // No EXIF segment in the container: a normal image, empty summary.
        Err(exif::Error::NotFound(_)) => return Ok(ExifSummary::default()),
        Err(err) => return Err(ExifError::Parse(err.to_string())),
    };
    let mut summary = ExifSummary::default();
    if let Some(field) = exif.get_field(Tag::Orientation, In::PRIMARY) {
        let value = match &field.value {
            Value::Short(values) => values.first().copied(),
            _ => return Err(ExifError::Parse("orientation is not a SHORT".to_string())),
        };
        let value = value.ok_or_else(|| ExifError::Parse("orientation is empty".to_string()))?;
        if !(1..=8).contains(&value) {
            return Err(ExifError::InvalidOrientation(value));
        }
        summary.orientation = Some(value);
    }
    summary.make = ascii_field(&exif, Tag::Make);
    summary.model = ascii_field(&exif, Tag::Model);
    summary.software = ascii_field(&exif, Tag::Software);
    Ok(summary)
}

/// One ASCII EXIF field as a string, or `None` when absent, empty,
/// or stored under an unexpected type (informational fields never
/// fail the summary — only orientation is strict).
fn ascii_field(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    let Value::Ascii(values) = &field.value else {
        return None;
    };
    let raw = values.first()?;
    let text = String::from_utf8_lossy(raw).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::media::MediaKind;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::{DynamicImage, ImageBuffer, Rgb};

    /// Build a minimal TIFF/EXIF payload (the bytes after `Exif\0\0`
    /// in a JPEG APP1 segment, or inside a PNG eXIf chunk, or a raw
    /// `.tif` file): header + one IFD0 with the four summary fields.
    /// `little_endian` selects the byte order; the offsets are
    /// computed from the layout, never hardcoded.
    fn tiff_payload(little_endian: bool, orientation: u16) -> Vec<u8> {
        let u16 = |v: u16| -> [u8; 2] {
            if little_endian {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let u32 = |v: u32| -> [u8; 4] {
            if little_endian {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let mut out = Vec::new();
        // TIFF header: byte order + 42 + IFD0 offset (8).
        out.extend_from_slice(if little_endian { b"II" } else { b"MM" });
        out.extend_from_slice(&u16(42));
        out.extend_from_slice(&u32(8));
        // IFD0: 4 entries; the strings live after the IFD.
        let ifd_len = 2 + 4 * 12 + 4;
        let strings_at = 8 + ifd_len as u32;
        let make = b"Acme\0";
        let model = b"X100V\0";
        let software = b"Detag 1.0\0";
        out.extend_from_slice(&u16(4));
        let entry = |tag: u16, typ: u16, count: u32, value: [u8; 4]| {
            let mut e = Vec::with_capacity(12);
            e.extend_from_slice(&u16(tag));
            e.extend_from_slice(&u16(typ));
            e.extend_from_slice(&u32(count));
            e.extend_from_slice(&value);
            e
        };
        // Orientation: SHORT, count 1, value inline (low 2 bytes).
        let mut orient_value = [0u8; 4];
        orient_value[..2].copy_from_slice(&u16(orientation));
        out.extend_from_slice(&entry(0x0112, 3, 1, orient_value));
        // Make / Model / Software: ASCII, values by offset.
        let mut off = strings_at;
        for (tag, bytes) in [
            (0x010f_u16, make.as_slice()),
            (0x0110_u16, model.as_slice()),
            (0x0131_u16, software.as_slice()),
        ] {
            out.extend_from_slice(&entry(tag, 2, bytes.len() as u32, u32(off)));
            off += bytes.len() as u32;
        }
        out.extend_from_slice(&u32(0)); // no next IFD
        out.extend_from_slice(make);
        out.extend_from_slice(model);
        out.extend_from_slice(software);
        out
    }

    /// The TIFF payload wrapped as a JPEG APP1 segment: SOI + APP1 +
    /// EOI. The parser scans markers only — no pixel data is needed.
    fn jpeg_with_exif(little_endian: bool, orientation: u16) -> Vec<u8> {
        let payload = tiff_payload(little_endian, orientation);
        let mut seg = b"Exif\0\0".to_vec();
        seg.extend_from_slice(&payload);
        let len = (seg.len() + 2) as u16; // length includes itself
        let mut out = vec![0xff, 0xd8, 0xff, 0xe1];
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&seg);
        out.extend_from_slice(&[0xff, 0xd9]);
        out
    }

    /// The TIFF payload wrapped as a PNG eXIf chunk: signature +
    /// IHDR (empty, CRC zero — the parser never verifies CRCs) +
    /// eXIf + IEND.
    fn png_with_exif(little_endian: bool, orientation: u16) -> Vec<u8> {
        let payload = tiff_payload(little_endian, orientation);
        let mut out = b"\x89PNG\x0d\x0a\x1a\x0a".to_vec();
        let chunk = |typ: &[u8; 4], data: &[u8]| {
            let mut c = Vec::new();
            c.extend_from_slice(&(data.len() as u32).to_be_bytes());
            c.extend_from_slice(typ);
            c.extend_from_slice(data);
            c.extend_from_slice(&[0, 0, 0, 0]); // CRC: unchecked by the parser
            c
        };
        // Minimal IHDR: 13 bytes of zeros (the parser skips it).
        out.extend_from_slice(&chunk(b"IHDR", &[0u8; 13]));
        out.extend_from_slice(&chunk(b"eXIf", &payload));
        out.extend_from_slice(&chunk(b"IEND", &[]));
        out
    }

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

    fn expected_summary() -> ExifSummary {
        ExifSummary {
            orientation: Some(6),
            make: Some("Acme".to_string()),
            model: Some("X100V".to_string()),
            software: Some("Detag 1.0".to_string()),
        }
    }

    #[test]
    fn jpeg_exif_little_endian_exact() {
        let bytes = jpeg_with_exif(true, 6);
        let summary = parse_exif(&bytes, MediaKind::Jpeg).expect("jpeg exif parses");
        assert_eq!(summary, expected_summary());
    }

    #[test]
    fn jpeg_exif_big_endian_exact() {
        // Same fields, Motorola byte order — the parser must not
        // assume little-endian.
        let bytes = jpeg_with_exif(false, 6);
        let summary = parse_exif(&bytes, MediaKind::Jpeg).expect("jpeg exif parses");
        assert_eq!(summary, expected_summary());
    }

    #[test]
    fn png_exif_chunk_exact() {
        let bytes = png_with_exif(true, 6);
        let summary = parse_exif(&bytes, MediaKind::Png).expect("png exif parses");
        assert_eq!(summary, expected_summary());
    }

    #[test]
    fn tiff_native_exact() {
        // A raw .tif file IS the TIFF payload — no container to unwrap.
        let bytes = tiff_payload(true, 6);
        let summary = parse_exif(&bytes, MediaKind::Tiff).expect("tiff exif parses");
        assert_eq!(summary, expected_summary());
    }

    #[test]
    fn orientation_values_all_accepted() {
        // Every legal orientation 1..=8 parses; the value is carried verbatim.
        for v in 1..=8u16 {
            let bytes = jpeg_with_exif(true, v);
            let summary = parse_exif(&bytes, MediaKind::Jpeg).expect("orientation parses");
            assert_eq!(summary.orientation, Some(v), "orientation {v}");
        }
    }

    #[test]
    fn orientation_out_of_range_fails() {
        for v in [0u16, 9, 42] {
            let bytes = jpeg_with_exif(true, v);
            let err = parse_exif(&bytes, MediaKind::Jpeg).expect_err("bad orientation fails");
            assert!(
                matches!(err, ExifError::InvalidOrientation(got) if got == v),
                "unexpected: {err}"
            );
        }
    }

    #[test]
    fn no_exif_is_an_empty_summary_not_an_error() {
        // Real encoders write no EXIF: a plain PNG/JPEG parses to empty.
        let src = DynamicImage::ImageRgb8(gradient_rgb8(32, 24));
        let png = encode_png8(&src);
        let summary = parse_exif(&png, MediaKind::Png).expect("png without exif");
        assert!(summary.is_empty());
        assert_eq!(summary, ExifSummary::default());
        let jpeg = encode_jpeg(&src, 90);
        let summary = parse_exif(&jpeg, MediaKind::Jpeg).expect("jpeg without exif");
        assert!(summary.is_empty());
    }

    #[test]
    fn truncated_and_garbage_never_panic() {
        let full = jpeg_with_exif(true, 6);
        // Every prefix of a valid file: truncated at any point.
        for cut in [0, 2, 4, 8, 12, 20, 30, 50, full.len() - 1] {
            let bytes = &full[..cut];
            if bytes.is_empty() {
                assert!(matches!(
                    parse_exif(bytes, MediaKind::Jpeg),
                    Err(ExifError::EmptyInput)
                ));
            } else {
                // Error or (for a cut that still holds a whole segment)
                // a summary — never a panic.
                let _ = parse_exif(bytes, MediaKind::Jpeg);
            }
        }
        // Random garbage, wrong magic, and a TIFF header that lies
        // about its IFD offset.
        for garbage in [
            vec![0u8; 64],
            vec![0xffu8; 128],
            b"not an image at all, just text".to_vec(),
            {
                let mut b = tiff_payload(true, 6);
                b[4..8].copy_from_slice(&9999u32.to_le_bytes()); // IFD past EOF
                b
            },
            {
                let mut b = tiff_payload(true, 6);
                b[0..2].copy_from_slice(b"ZZ"); // not a TIFF at all
                b
            },
        ] {
            let _ = parse_exif(&garbage, MediaKind::Jpeg);
            let _ = parse_exif(&garbage, MediaKind::Tiff);
            let _ = parse_exif(&garbage, MediaKind::Png);
        }
    }

    #[test]
    fn empty_input_and_video_kinds_fail_typed() {
        let err = parse_exif(&[], MediaKind::Jpeg).expect_err("empty fails");
        assert!(matches!(err, ExifError::EmptyInput));
        for kind in [MediaKind::Mp4, MediaKind::Mkv, MediaKind::Webm] {
            let err = parse_exif(&[0u8; 64], kind).expect_err("video fails");
            assert!(
                matches!(err, ExifError::NotAnImage(k) if k == kind),
                "unexpected: {err}"
            );
        }
    }

    #[test]
    fn metadata_fragment_shape_is_the_uploads_shape() {
        // The fragment P2-008 merges into uploads.metadata: the exif
        // key namespaces the summary; absent fields are omitted.
        let summary = expected_summary();
        let value = summary.to_metadata_value();
        assert_eq!(
            value,
            serde_json::json!({
                "exif": {
                    "orientation": 6,
                    "make": "Acme",
                    "model": "X100V",
                    "software": "Detag 1.0"
                }
            })
        );
        assert_eq!(
            ExifSummary::default().to_metadata_value(),
            serde_json::json!({ "exif": {} }),
            "no-EXIF still records that EXIF was parsed"
        );
    }

    #[test]
    fn gps_is_never_read() {
        // The guarantee is structural: the parser only ever asks the
        // library for the four summary tags (Orientation/Make/Model/
        // Software) and `ExifSummary` has no field that could hold
        // location — so no GPS data can reach the summary or the
        // metadata fragment, whatever the file carries.
        let summary = parse_exif(&jpeg_with_exif(true, 6), MediaKind::Jpeg).expect("parses");
        let json = serde_json::to_string(&summary).expect("serializes");
        assert!(!json.contains("GPS"), "no GPS in the summary: {json}");
        assert!(
            !json.contains("latitude"),
            "no location in the summary: {json}"
        );
    }
}
