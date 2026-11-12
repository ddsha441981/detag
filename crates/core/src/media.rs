//! Content-type sniffing (P0-059, E0.6; FR-U-03).
//!
//! The server never trusts a claimed extension or MIME type: it
//! sniffs the magic bytes of the completed object — head AND tail
//! — and rejects anything that does not match the allowlist
//! (`png/jpeg/webp/mp4/mkv/webm`) with `AppError::UnsupportedMediaType`
//! (PF-2001, 415). Sniffing both ends is what catches polyglots:
//! a file that is a valid PNG at the head but carries a ZIP at
//! the tail is not a PNG.
//!
//! The caller (intake, Phase 2/3) reads the head (first 4 KiB)
//! and the tail (last 4 KiB) of the completed object and calls
//! [`sniff_media`]; [`HEAD_SNIFF_LEN`] / [`TAIL_SNIFF_LEN`] are
//! those read sizes. Small files pass the whole file as both.
//!
//! Per-kind rules:
//! - PNG: 8-byte signature at the head, IEND chunk (`00 00 00 00
//!   "IEND" AE 42 60 82`) as the last 12 bytes.
//! - JPEG: `FF D8 FF` + a marker byte at the head, `FF D9` (EOI)
//!   as the last 2 bytes.
//! - WebP: `RIFF` + size + `WEBP` at the head, and the RIFF size
//!   field must equal `total_size - 8` (the tail check: a
//!   truncated or appended file fails it).
//! - MP4: `ftyp` at offset 4 at the head (any brand).
//! - MKV / WebM: EBML magic `1A 45 DF A3` at the head, and the
//!   DocType string (`matroska` / `webm`) within the head.
//! - Every kind: the tail must not contain a ZIP signature
//!   (`PK\x03\x04` local header or `PK\x05\x06` end-of-central-
//!   directory). MP4, MKV and WebM have no tail marker of their
//!   own, so this scan is their whole tail check.
//!
//! A trailing archive signature inside compressed media data is
//! possible in principle (random bytes can spell anything), so
//! the scan can false-reject at ~1e-6 per file — acceptable for a
//! security boundary, and documented here rather than hidden.

use crate::error::AppError;

/// Recommended head read size for the caller (first 4 KiB).
pub const HEAD_SNIFF_LEN: usize = 4096;
/// Recommended tail read size for the caller (last 4 KiB).
pub const TAIL_SNIFF_LEN: usize = 4096;

/// A sniffed media kind (the allowlist, FR-U-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// PNG image.
    Png,
    /// JPEG image.
    Jpeg,
    /// WebP image.
    WebP,
    /// MP4 video.
    Mp4,
    /// Matroska video.
    Mkv,
    /// WebM video.
    Webm,
}

impl MediaKind {
    /// The canonical MIME type.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
            Self::Mp4 => "video/mp4",
            Self::Mkv => "video/x-matroska",
            Self::Webm => "video/webm",
        }
    }

    /// The canonical lowercase extension (no dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
        }
    }

    /// Parse a claimed extension (case-insensitive, dot optional)
    /// to the kind it claims (`None` for anything else).
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::WebP),
            "mp4" => Some(Self::Mp4),
            "mkv" => Some(Self::Mkv),
            "webm" => Some(Self::Webm),
            _ => None,
        }
    }
}

/// Sniff the media kind from the head and tail of a completed
/// object. `total_size` is the object's full byte size (the
/// caller knows it from the upload row / storage head).
pub fn sniff_media(head: &[u8], tail: &[u8], total_size: u64) -> Result<MediaKind, AppError> {
    let kind = sniff_head(head)?;
    check_tail(kind, head, tail, total_size)?;
    Ok(kind)
}

/// Detect the kind from head magic bytes only.
fn sniff_head(head: &[u8]) -> Result<MediaKind, AppError> {
    // PNG signature (8 bytes).
    if head.len() >= 8 && head[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return Ok(MediaKind::Png);
    }
    // JPEG: SOI FF D8, then a marker FF xx (xx is never 00 or FF).
    if head.len() >= 4
        && head[0] == 0xFF
        && head[1] == 0xD8
        && head[2] == 0xFF
        && head[3] != 0x00
        && head[3] != 0xFF
    {
        return Ok(MediaKind::Jpeg);
    }
    // WebP: "RIFF" + 4-byte LE size + "WEBP" (12 bytes).
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Ok(MediaKind::WebP);
    }
    // MP4: "ftyp" at offset 4 (any brand after it).
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return Ok(MediaKind::Mp4);
    }
    // MKV / WebM: EBML magic, DocType string within the head.
    if head.len() >= 4 && head[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        let window = &head[..head.len().min(1024)];
        if contains(window, b"webm") {
            return Ok(MediaKind::Webm);
        }
        if contains(window, b"matroska") {
            return Ok(MediaKind::Mkv);
        }
        return Err(AppError::UnsupportedMediaType);
    }
    Err(AppError::UnsupportedMediaType)
}

/// Verify the tail is consistent with `kind`.
fn check_tail(kind: MediaKind, head: &[u8], tail: &[u8], total_size: u64) -> Result<(), AppError> {
    match kind {
        MediaKind::Png => {
            // Last 12 bytes must be the IEND chunk.
            const IEND: [u8; 12] = [
                0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
            ];
            if tail.len() < 12 || tail[tail.len() - 12..] != IEND {
                return Err(AppError::UnsupportedMediaType);
            }
        }
        MediaKind::Jpeg => {
            // Last 2 bytes must be EOI.
            if tail.len() < 2 || tail[tail.len() - 2..] != [0xFF, 0xD9] {
                return Err(AppError::UnsupportedMediaType);
            }
        }
        MediaKind::WebP => {
            // RIFF size field (LE u32 at offset 4) must equal the
            // file size minus the 8-byte RIFF header.
            if head.len() < 8 {
                return Err(AppError::UnsupportedMediaType);
            }
            let riff_size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
            if total_size < 8 || riff_size != total_size - 8 {
                return Err(AppError::UnsupportedMediaType);
            }
        }
        MediaKind::Mp4 | MediaKind::Mkv | MediaKind::Webm => {
            // No tail marker of their own; the archive scan below
            // is the whole tail check.
        }
    }
    // No trailing archive, whatever the kind: a ZIP local header
    // or end-of-central-directory in the tail means the file
    // carries a second format.
    if contains(tail, b"PK\x03\x04") || contains(tail, b"PK\x05\x06") {
        return Err(AppError::UnsupportedMediaType);
    }
    Ok(())
}

/// Check a claimed extension against the sniffed kind: the file
/// is what its bytes say, not what the client called it.
pub fn check_claimed(kind: MediaKind, claimed_ext: &str) -> Result<(), AppError> {
    match MediaKind::from_extension(claimed_ext) {
        Some(claimed) if claimed == kind => Ok(()),
        _ => Err(AppError::UnsupportedMediaType),
    }
}

/// Byte-substring search (no extra dependency for one call).
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split test data the way the caller does: head = first 4 KiB
    /// (or all), tail = last 4 KiB (or all).
    fn split(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let head = data[..data.len().min(HEAD_SNIFF_LEN)].to_vec();
        let tail_start = data.len().saturating_sub(TAIL_SNIFF_LEN);
        (head, data[tail_start..].to_vec())
    }

    fn sniff(data: &[u8]) -> Result<MediaKind, AppError> {
        let (head, tail) = split(data);
        sniff_media(&head, &tail, data.len() as u64)
    }

    fn png() -> Vec<u8> {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        // Minimal IHDR chunk (length + "IHDR" + 13 data bytes + CRC).
        v.extend_from_slice(&[0, 0, 0, 13]);
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&[0; 13]);
        v.extend_from_slice(&[0; 4]);
        // IEND chunk.
        v.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]);
        v
    }

    fn jpeg() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
        v.extend_from_slice(b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00");
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    fn webp() -> Vec<u8> {
        let mut v = Vec::new();
        let body = b"VP8 \x0A\x00\x00\x00\x9D\x01\x2A\x01\x00\x02\x00\x34\x25\xA0\x02\x74\x01\x00";
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        v.extend_from_slice(b"WEBP");
        v.extend_from_slice(body);
        v
    }

    fn mp4() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 24];
        v.extend_from_slice(b"ftypisom\x00\x00\x02\x00isomiso2");
        v.extend_from_slice(b"mdat\x00\x01\x02\x03");
        v
    }

    fn ebml(doctype: &[u8]) -> Vec<u8> {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x42, 0x82];
        v.push(0x80 | doctype.len() as u8);
        v.extend_from_slice(doctype);
        v.extend_from_slice(&[0x42, 0x87, 0x81, 0x01]);
        v
    }

    #[test]
    fn every_allowlisted_kind_sniffs() {
        assert_eq!(sniff(&png()).expect("png"), MediaKind::Png);
        assert_eq!(sniff(&jpeg()).expect("jpeg"), MediaKind::Jpeg);
        assert_eq!(sniff(&webp()).expect("webp"), MediaKind::WebP);
        assert_eq!(sniff(&mp4()).expect("mp4"), MediaKind::Mp4);
        assert_eq!(sniff(&ebml(b"matroska")).expect("mkv"), MediaKind::Mkv);
        assert_eq!(sniff(&ebml(b"webm")).expect("webm"), MediaKind::Webm);
    }

    #[test]
    fn mime_and_extension_are_canonical() {
        assert_eq!(MediaKind::Png.mime(), "image/png");
        assert_eq!(MediaKind::Jpeg.mime(), "image/jpeg");
        assert_eq!(MediaKind::Jpeg.extension(), "jpg");
        assert_eq!(MediaKind::Mkv.mime(), "video/x-matroska");
        assert_eq!(MediaKind::from_extension("JPEG"), Some(MediaKind::Jpeg));
        assert_eq!(MediaKind::from_extension(".png"), Some(MediaKind::Png));
        assert_eq!(MediaKind::from_extension("exe"), None);
    }

    #[test]
    fn claimed_extension_mismatch_is_rejected() {
        let kind = sniff(&png()).expect("png");
        assert!(check_claimed(kind, "png").is_ok());
        assert!(matches!(
            check_claimed(kind, "jpg"),
            Err(AppError::UnsupportedMediaType)
        ));
        assert!(matches!(
            check_claimed(kind, "exe"),
            Err(AppError::UnsupportedMediaType)
        ));
    }

    #[test]
    fn polyglot_png_plus_zip_is_rejected() {
        // A valid PNG with a ZIP appended: the PNG head is fine,
        // but the tail no longer ends with IEND — and the tail
        // carries ZIP signatures.
        let mut data = png();
        data.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00\x08\x00");
        data.extend_from_slice(
            b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        );
        assert!(matches!(sniff(&data), Err(AppError::UnsupportedMediaType)));
    }

    #[test]
    fn polyglot_jpeg_plus_zip_is_rejected() {
        let mut data = jpeg();
        data.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00\x08\x00");
        data.extend_from_slice(
            b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        );
        assert!(matches!(sniff(&data), Err(AppError::UnsupportedMediaType)));
    }

    #[test]
    fn polyglot_mp4_plus_zip_is_rejected() {
        // MP4 has no tail marker, so the archive scan is what
        // catches this one.
        let mut data = mp4();
        data.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00\x08\x00");
        data.extend_from_slice(
            b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        );
        assert!(matches!(sniff(&data), Err(AppError::UnsupportedMediaType)));
    }

    #[test]
    fn truncated_and_wrong_size_files_are_rejected() {
        // PNG without IEND.
        let mut no_iend = png();
        no_iend.truncate(no_iend.len() - 12);
        assert!(matches!(
            sniff(&no_iend),
            Err(AppError::UnsupportedMediaType)
        ));
        // JPEG without EOI.
        let mut no_eoi = jpeg();
        no_eoi.truncate(no_eoi.len() - 2);
        assert!(matches!(
            sniff(&no_eoi),
            Err(AppError::UnsupportedMediaType)
        ));
        // WebP with a lying RIFF size.
        let mut bad_webp = webp();
        bad_webp[4] = 0xFF;
        assert!(matches!(
            sniff(&bad_webp),
            Err(AppError::UnsupportedMediaType)
        ));
        // EBML without a DocType.
        assert!(matches!(
            sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0x42, 0x82, 0x81, 0x01]),
            Err(AppError::UnsupportedMediaType)
        ));
        // Random bytes, empty input, and a ZIP-first file.
        assert!(matches!(
            sniff(b"hello, this is not media at all"),
            Err(AppError::UnsupportedMediaType)
        ));
        assert!(matches!(sniff(b""), Err(AppError::UnsupportedMediaType)));
        assert!(matches!(
            sniff(b"PK\x03\x04\x14\x00\x00\x00\x08\x00"),
            Err(AppError::UnsupportedMediaType)
        ));
    }

    #[test]
    fn error_code_is_pf_2001() {
        assert_eq!(AppError::UnsupportedMediaType.code(), "PF-2001");
        assert_eq!(AppError::UnsupportedMediaType.status(), 415);
    }
}
