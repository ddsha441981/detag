//! Orientation normalize — the E2.1 intake orientation applier (P2-005, FR-U-06, FR-D-07, NFR-43).
//!
//! [`normalize_orientation`] turns a decoded image plus the EXIF
//! orientation P2-003 already parsed (`ExifSummary.orientation`) into
//! pixels that are already upright, plus a record of what was applied.
//! The mechanism is the `image` crate's own: `Orientation::from_exif`
//! maps the EXIF value 1..=8 to a transform and
//! `DynamicImage::apply_orientation` applies it — verified against the
//! `image` 0.25.10 registry sources (not docs alone), and the mapping
//! was independently derived from the EXIF spec's row/column table
//! before the upstream code was trusted (ADR-0074). No new dependency.
//!
//! # Error semantics (what a caller should do with each outcome)
//!
//! * `Ok` with `record.applied == None` — the image carried no
//!   orientation tag. The pixels are returned unchanged (identity);
//!   that is normal, not a failure.
//! * `Ok` with `record.applied == Some(v)` — the pixels were rotated
//!   / flipped per value `v`, and the tag is consumed: the record says
//!   `stripped: true`, so the caller must NOT carry the tag into the
//!   output (it would double-rotate on any EXIF-aware reader).
//! * `Err(OrientationError::InvalidOrientation(..))` — the value is
//!   outside 1..=8. `parse_exif` already rejects such values (ADR-0072
//!   §3), so this is defense in depth: a bogus value must never reach
//!   pixel code.
//!
//! The transform itself cannot fail: `apply_orientation` is infallible
//! on a decoded image, and it maps over every `DynamicImage` variant,
//! so a 16-bit source stays 16-bit (FR-D-07 — this ticket must not
//! undo P2-002's bit-depth preservation).
//!
//! # Where the record goes
//!
//! [`OrientationRecord::to_metadata_value`] serializes the record to
//! the exact `uploads.metadata` jsonb fragment
//! (`{"orientation": {"applied": 6, "transform": "rotate-90-cw",
//! "stripped": true}}` — the `orientation` key namespaces it alongside
//! `exif` (ADR-0072 §5) and `icc` (ADR-0073), so P2-008 merges without
//! collisions). The WRITE itself lands in P2-008, the first ticket that
//! persists intake results onto `uploads` — there is no intake caller
//! yet (job creation is still the 501 stub, `decode_image`'s only
//! caller is its bench), so P2-005 ships parse/transform-only with the
//! shape proven (ADR-0074). No migration: `uploads.metadata jsonb`
//! already exists (migration `20261112064924`, `DEFAULT '{}'::jsonb`).
//!
//! # What this does NOT do
//!
//! No EXIF stripping — `stripped` is a RECORDED state, not a re-encode:
//! the physical strip happens at encode (E2.6, ADR-0006, NFR-43). No
//! decoding here — the caller decodes first (`decode_image`, P2-002)
//! and applies orientation after, never before (there are no pixels to
//! rotate before decode). No quality metrics — E2.7.

use image::{metadata::Orientation, DynamicImage};
use serde::Serialize;

/// The transform applied to the pixels, as a stable name.
///
/// The names are the upstream `image::metadata::Orientation` variant
/// names, hyphenated: value 5 is the transpose (`rotate-90-flip-h`)
/// and value 7 the transverse (`rotate-270-flip-h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AppliedTransform {
    /// Value 1: the image is already upright.
    #[serde(rename = "identity")]
    Identity,
    /// Value 2: mirror left-right.
    #[serde(rename = "flip-horizontal")]
    FlipHorizontal,
    /// Value 3: rotate 180 degrees.
    #[serde(rename = "rotate-180")]
    Rotate180,
    /// Value 4: mirror top-bottom.
    #[serde(rename = "flip-vertical")]
    FlipVertical,
    /// Value 5: rotate 90 degrees clockwise, then flip horizontally
    /// (the transpose).
    #[serde(rename = "rotate-90-flip-h")]
    Rotate90FlipH,
    /// Value 6: rotate 90 degrees clockwise.
    #[serde(rename = "rotate-90-cw")]
    Rotate90,
    /// Value 7: rotate 270 degrees clockwise, then flip horizontally
    /// (the transverse).
    #[serde(rename = "rotate-270-flip-h")]
    Rotate270FlipH,
    /// Value 8: rotate 270 degrees clockwise.
    #[serde(rename = "rotate-270-cw")]
    Rotate270,
}

impl AppliedTransform {
    /// The stable name this transform serializes as.
    pub fn name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::FlipHorizontal => "flip-horizontal",
            Self::Rotate180 => "rotate-180",
            Self::FlipVertical => "flip-vertical",
            Self::Rotate90FlipH => "rotate-90-flip-h",
            Self::Rotate90 => "rotate-90-cw",
            Self::Rotate270FlipH => "rotate-270-flip-h",
            Self::Rotate270 => "rotate-270-cw",
        }
    }
}

/// The record of what orientation normalization did: the applied EXIF
/// value, the transform name, and whether the tag was consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OrientationRecord {
    /// The EXIF orientation value applied (1..=8), or `None` when no
    /// orientation tag was present. Serializes as `null` when absent —
    /// the record explicitly says "nothing was applied", so P2-008 can
    /// tell "step ran, nothing to do" apart from "step never ran".
    pub applied: Option<u16>,
    /// The transform applied to the pixels (`identity` when no tag).
    pub transform: AppliedTransform,
    /// True when the orientation tag was consumed: the transform is in
    /// the pixels, so the tag must NOT be carried into the output (it
    /// would double-rotate). False when there was no tag to consume.
    /// The physical strip lands at encode (E2.6); this is the record
    /// of intent, not a re-encode (ADR-0074).
    pub stripped: bool,
}

impl OrientationRecord {
    /// The `uploads.metadata` jsonb fragment this record becomes:
    /// `{"orientation": {"applied": 6, "transform": "rotate-90-cw",
    /// "stripped": true}}`, or `{"orientation": {"applied": null,
    /// "transform": "identity", "stripped": false}}` when no tag was
    /// present. The write lands in P2-008; this only proves the shape
    /// (ADR-0074).
    pub fn to_metadata_value(&self) -> serde_json::Value {
        serde_json::json!({ "orientation": self })
    }
}

/// A normalized image: the upright pixels plus the record of what was
/// applied to them.
#[derive(Debug)]
pub struct NormalizedImage {
    /// The pixels after the orientation was applied (unchanged when no
    /// orientation tag was present). 16-bit sources stay 16-bit.
    pub image: DynamicImage,
    /// What was applied (or that nothing was).
    pub record: OrientationRecord,
}

/// Why orientation normalization failed. No panics: every failure is a
/// value.
#[derive(Debug, thiserror::Error)]
pub enum OrientationError {
    /// The orientation value is outside the EXIF range 1..=8. Fails
    /// loudly on purpose: a bogus value must never reach pixel code.
    #[error("EXIF orientation {0} is out of range (1..=8)")]
    InvalidOrientation(u16),
}

/// Apply the EXIF `orientation` into the pixels of a decoded `image`.
///
/// `orientation` is the value P2-003's `parse_exif` returned
/// (`ExifSummary.orientation`): `None` means no tag — the image is
/// returned unchanged with an identity record; `Some(v)` with `v` in
/// 1..=8 rotates/flips the pixels per the EXIF spec (via the `image`
/// crate's `Orientation::from_exif` + `DynamicImage::apply_orientation`,
/// ADR-0074) and records the tag as consumed (`stripped: true`). A
/// value outside 1..=8 fails as [`OrientationError::InvalidOrientation`].
pub fn normalize_orientation(
    mut image: DynamicImage,
    orientation: Option<u16>,
) -> Result<NormalizedImage, OrientationError> {
    let Some(value) = orientation else {
        return Ok(NormalizedImage {
            image,
            record: OrientationRecord {
                applied: None,
                transform: AppliedTransform::Identity,
                stripped: false,
            },
        });
    };
    let upstream = u8::try_from(value)
        .ok()
        .and_then(Orientation::from_exif)
        .ok_or(OrientationError::InvalidOrientation(value))?;
    let transform = match upstream {
        Orientation::NoTransforms => AppliedTransform::Identity,
        Orientation::FlipHorizontal => AppliedTransform::FlipHorizontal,
        Orientation::Rotate180 => AppliedTransform::Rotate180,
        Orientation::FlipVertical => AppliedTransform::FlipVertical,
        Orientation::Rotate90FlipH => AppliedTransform::Rotate90FlipH,
        Orientation::Rotate90 => AppliedTransform::Rotate90,
        Orientation::Rotate270FlipH => AppliedTransform::Rotate270FlipH,
        Orientation::Rotate270 => AppliedTransform::Rotate270,
    };
    image.apply_orientation(upstream);
    Ok(NormalizedImage {
        image,
        record: OrientationRecord {
            applied: Some(value),
            transform,
            stripped: true,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exif::parse_exif;
    use crate::intake::decode_image;
    use detag_core::media::MediaKind;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::{ColorType, DynamicImage, GenericImageView, ImageBuffer, Rgb, Rgba};

    /// A small asymmetric image: every pixel encodes its coordinates,
    /// so a wrong rotation/flip is visible at every pixel, not just at
    /// the edges. 4 wide × 3 high on purpose — non-square, so the
    /// dimension-swapping values (5–8) are exercised.
    fn coord_rgb8(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x * 40) as u8, (y * 40) as u8, ((x + y) * 20) as u8])
        })
    }

    /// Where output pixel (x, y) comes FROM in the source, per the
    /// EXIF spec's row/column definition of each orientation value —
    /// derived from the spec table, INDEPENDENT of the implementation
    /// (which uses the image crate's `apply_orientation`): a wrong
    /// mapping in the implementation fails this test instead of
    /// agreeing with itself. (x = column, left→right; y = row,
    /// top→bottom; w/h = source dimensions.)
    fn expected_source_coords(x: u32, y: u32, w: u32, h: u32, orientation: u16) -> (u32, u32) {
        match orientation {
            // Row 0 top, col 0 left: already upright.
            1 => (x, y),
            // Row 0 top, col 0 right: mirror left-right.
            2 => (w - 1 - x, y),
            // Row 0 bottom, col 0 right: rotate 180.
            3 => (w - 1 - x, h - 1 - y),
            // Row 0 bottom, col 0 left: mirror top-bottom.
            4 => (x, h - 1 - y),
            // Row 0 left, col 0 top: transpose.
            5 => (y, x),
            // Row 0 right, col 0 top: rotate 90 clockwise.
            6 => (y, h - 1 - x),
            // Row 0 right, col 0 bottom: transverse.
            7 => (w - 1 - y, h - 1 - x),
            // Row 0 left, col 0 bottom: rotate 270 clockwise.
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

    /// The transform name each EXIF value must record.
    fn expected_transform_name(orientation: u16) -> &'static str {
        match orientation {
            1 => "identity",
            2 => "flip-horizontal",
            3 => "rotate-180",
            4 => "flip-vertical",
            5 => "rotate-90-flip-h",
            6 => "rotate-90-cw",
            7 => "rotate-270-flip-h",
            8 => "rotate-270-cw",
            _ => unreachable!("test only calls this with 1..=8"),
        }
    }

    #[test]
    fn all_eight_orientations_match_the_spec() {
        // The AC, pixel-exact: every orientation value applied to the
        // asymmetric fixture equals the spec-derived expectation, with
        // the right dimensions and the right record.
        for value in 1..=8u16 {
            let src = coord_rgb8(4, 3);
            let image = DynamicImage::ImageRgb8(src.clone());
            let normalized =
                normalize_orientation(image, Some(value)).expect("orientation 1..=8 applies");
            assert_eq!(normalized.record.applied, Some(value), "value {value}");
            assert_eq!(
                normalized.record.transform.name(),
                expected_transform_name(value),
                "value {value}"
            );
            assert!(normalized.record.stripped, "value {value}");
            let expected = expected_rgb8(&src, value);
            assert_eq!(
                normalized.image.dimensions(),
                expected.dimensions(),
                "value {value}: dimensions"
            );
            let DynamicImage::ImageRgb8(got) = normalized.image else {
                panic!("value {value}: still Rgb8");
            };
            assert_eq!(got, expected, "value {value}: golden pixels");
        }
    }

    #[test]
    fn orientation_6_on_16_bit_stays_16_bit() {
        // 16-bit sources rotate as 16-bit (FR-D-07): no downsampling
        // at the normalize step.
        let src = ImageBuffer::from_fn(4, 3, |x, y| {
            Rgb([(x * 1000) as u16, (y * 1000) as u16, ((x + y) * 500) as u16])
        });
        let image = DynamicImage::ImageRgb16(src.clone());
        let normalized = normalize_orientation(image, Some(6)).expect("orientation 6 applies");
        assert_eq!(normalized.image.color(), ColorType::Rgb16);
        assert_eq!(normalized.image.dimensions(), (3, 4));
        let DynamicImage::ImageRgb16(got) = normalized.image else {
            panic!("still Rgb16");
        };
        let expected = ImageBuffer::from_fn(3, 4, |x, y| {
            let (sx, sy) = expected_source_coords(x, y, 4, 3, 6);
            *src.get_pixel(sx, sy)
        });
        assert_eq!(got, expected, "16-bit rotate-90 pixels");
    }

    #[test]
    fn orientation_5_on_rgba_preserves_alpha() {
        // The transpose (value 5) on an RGBA image: colour AND alpha
        // both transpose — no channel is dropped or reset.
        let src = ImageBuffer::from_fn(4, 3, |x, y| {
            Rgba([
                (x * 40) as u8,
                (y * 40) as u8,
                ((x + y) * 20) as u8,
                (200 - x * 10) as u8,
            ])
        });
        let image = DynamicImage::ImageRgba8(src.clone());
        let normalized = normalize_orientation(image, Some(5)).expect("orientation 5 applies");
        assert_eq!(normalized.image.dimensions(), (3, 4));
        let DynamicImage::ImageRgba8(got) = normalized.image else {
            panic!("still Rgba8");
        };
        let expected = ImageBuffer::from_fn(3, 4, |x, y| {
            let (sx, sy) = expected_source_coords(x, y, 4, 3, 5);
            *src.get_pixel(sx, sy)
        });
        assert_eq!(got, expected, "RGBA transpose pixels");
    }

    #[test]
    fn no_orientation_is_identity() {
        let src = coord_rgb8(4, 3);
        let image = DynamicImage::ImageRgb8(src.clone());
        let normalized = normalize_orientation(image, None).expect("no orientation applies");
        assert_eq!(normalized.record.applied, None);
        assert_eq!(normalized.record.transform, AppliedTransform::Identity);
        assert!(!normalized.record.stripped, "no tag, nothing stripped");
        let DynamicImage::ImageRgb8(got) = normalized.image else {
            panic!("still Rgb8");
        };
        assert_eq!(got, src, "pixels unchanged");
    }

    #[test]
    fn out_of_range_orientation_fails() {
        for value in [0u16, 9, 42, 256] {
            let image = DynamicImage::ImageRgb8(coord_rgb8(4, 3));
            let err =
                normalize_orientation(image, Some(value)).expect_err("bogus orientation fails");
            assert!(
                matches!(err, OrientationError::InvalidOrientation(got) if got == value),
                "unexpected: {err}"
            );
        }
    }

    #[test]
    fn metadata_fragment_shape_is_the_uploads_shape() {
        // The fragment P2-008 merges into uploads.metadata: the
        // orientation key namespaces the record; applied null means
        // the step ran and found no tag.
        let record = OrientationRecord {
            applied: Some(6),
            transform: AppliedTransform::Rotate90,
            stripped: true,
        };
        assert_eq!(
            record.to_metadata_value(),
            serde_json::json!({
                "orientation": {
                    "applied": 6,
                    "transform": "rotate-90-cw",
                    "stripped": true
                }
            })
        );
        let record = OrientationRecord {
            applied: None,
            transform: AppliedTransform::Identity,
            stripped: false,
        };
        assert_eq!(
            record.to_metadata_value(),
            serde_json::json!({
                "orientation": {
                    "applied": null,
                    "transform": "identity",
                    "stripped": false
                }
            })
        );
    }

    /// Minimal TIFF payload carrying ONLY an orientation tag (value 6,
    /// little-endian, one IFD0 entry): the bytes after `Exif\0\0` in a
    /// JPEG APP1 segment, or inside a PNG eXIf chunk.
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
        // IEND is the last 12 bytes: insert the eXIf chunk before it.
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

    #[test]
    fn end_to_end_png_orientation_6_exact() {
        // The full intake path on real bytes: parse_exif reads the
        // orientation out of the PNG, decode_image decodes the stored
        // pixels, normalize_orientation applies it — and the result is
        // pixel-exact against the spec-derived expectation of the
        // stored decode (lossless, so exactness is the bar).
        let stored = DynamicImage::ImageRgb8(coord_rgb8(4, 3));
        let bytes = png_with_orientation_6(&stored);
        let summary = parse_exif(&bytes, MediaKind::Png).expect("png exif parses");
        assert_eq!(summary.orientation, Some(6));
        let decoded = decode_image(&bytes, MediaKind::Png).expect("png decodes");
        let normalized =
            normalize_orientation(decoded.image, summary.orientation).expect("orientation applies");
        assert_eq!(normalized.record.applied, Some(6));
        assert!(normalized.record.stripped);
        // Expectation from the stored decode itself, mapped by the
        // spec — the test is that apply_orientation matches the spec
        // mapping on real decoded data (lossless PNG, so exactness
        // is the bar).
        let stored_decoded = decode_image(&png_with_orientation_6(&stored), MediaKind::Png)
            .expect("png decodes again");
        let DynamicImage::ImageRgb8(stored_pixels) = stored_decoded.image else {
            panic!("stored decode is Rgb8");
        };
        let expected = expected_rgb8(&stored_pixels, 6);
        let DynamicImage::ImageRgb8(got) = normalized.image else {
            panic!("normalized is Rgb8");
        };
        assert_eq!(got, expected, "PNG end-to-end golden pixels");
    }

    #[test]
    fn end_to_end_jpeg_orientation_6_exact() {
        // Same path on real JPEG bytes: parse + decode + normalize,
        // exact against the spec-mapped stored decode (both sides
        // share the same lossy decode, so exactness still holds).
        let stored = DynamicImage::ImageRgb8(coord_rgb8(4, 3));
        let bytes = jpeg_with_orientation_6(&stored);
        let summary = parse_exif(&bytes, MediaKind::Jpeg).expect("jpeg exif parses");
        assert_eq!(summary.orientation, Some(6));
        let decoded = decode_image(&bytes, MediaKind::Jpeg).expect("jpeg decodes");
        let normalized =
            normalize_orientation(decoded.image, summary.orientation).expect("orientation applies");
        let stored_decoded = decode_image(&jpeg_with_orientation_6(&stored), MediaKind::Jpeg)
            .expect("jpeg decodes again");
        let DynamicImage::ImageRgb8(stored_pixels) = stored_decoded.image else {
            panic!("stored decode is Rgb8");
        };
        let expected = expected_rgb8(&stored_pixels, 6);
        let DynamicImage::ImageRgb8(got) = normalized.image else {
            panic!("normalized is Rgb8");
        };
        assert_eq!(got, expected, "JPEG end-to-end golden pixels");
    }
}
