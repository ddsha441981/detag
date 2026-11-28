//! Image hashing — the P2-008 hash record (FR-U-05).
//!
//! Every input gets three hashes, all stored on `uploads`:
//! * `sha256` — the whole-object cryptographic hash, computed
//!   STREAMING: [`sha256_streaming`] feeds the hasher in fixed
//!   8 KiB chunks (the same buffer size as core's
//!   [`detag_core::checksum::sha256_reader`]), so no second copy
//!   of the input is ever materialized. It equals core's one-shot
//!   [`detag_core::checksum::sha256_hex`] — the tests prove it.
//! * `dhash` — the difference hash (gradient sign over a 9x8
//!   luma thumbnail), the near-duplicate key. This is what
//!   `uploads.phash` holds ([`PHASH_ALGORITHM`]).
//! * `ahash` — the average hash (mean threshold over an 8x8 luma
//!   thumbnail), the coarser second opinion. It lives in the
//!   metadata fragment only, not in a column.
//!
//! The perceptual hashes are computed IN-TREE from the `image`
//! crate's own primitives — grayscale via `DynamicImage::to_luma8`
//! (REC709 luma, verified in the image 0.25.10 registry sources:
//! `SRGB_LUMA = [2126, 7152, 722] / 10000`, color.rs:606), resize
//! via `imageops::resize` with a Triangle filter, then the
//! textbook bit rules (dHash: left pixel dimmer than right;
//! aHash: pixel brighter than the mean). No new crate enters the
//! tree for two 15-line algorithms — and the alternatives were
//! evaluated and REJECTED from the registry sources, see ADR-0077:
//! `imageproc` 0.25.1 has NO hash API at all (the spec's
//! "(imageproc)" is wrong for this tree; `image_hash` exists only
//! in 0.26.x, which pairs with image 0.26 — out of scope), and
//! `imghash` 2.0.0, though API-compatible, declares `image` with
//! DEFAULT features — cargo feature unification then turns on the
//! image crate's whole `default-formats` set (avif/bmp/gif/…) in
//! every workspace build, violating P2-002's explicit "no default
//! features … must not enter the build" decision and dragging
//! ravif/rav1e (+ libfuzzer-sys, NCSA) into the plain
//! `cargo deny check` graph — proven: plain deny is green on
//! pristine phase-2 HEAD and FAILED with imghash in the tree.
//! The in-tree hashes are therefore NOT Python-`imagehash`
//! compatible (REC709 + Triangle vs REC601 + Lanczos3) — that
//! costs nothing, because the hashes are internal dedupe keys
//! with no external interop requirement.
//!
//! # The metadata write (this ticket)
//!
//! [`HashRecord::to_metadata_value`] serializes the record to the
//! exact `uploads.metadata` jsonb fragment (`{"hashes": {...}}` —
//! the `hashes` key namespaces it alongside `exif` (ADR-0072),
//! `icc` (ADR-0073) and `orientation` (ADR-0074), so
//! [`merge_metadata_fragments`] assembles the four fragments into
//! the single object the intake persists, with a duplicate key
//! failing closed instead of silently dropping a fragment). The
//! WRITE itself is the core repo setters
//! (`repos::uploads::set_phash` / `set_metadata`, P2-008) — there
//! is still no intake caller (job creation is the 501 stub), so
//! this ticket ships the computation + the persistence surface
//! with the shape proven, the P2-003–P2-007 precedent. No
//! migration: `uploads.phash` and `uploads.metadata` already exist.
//!
//! # What the hashes are NOT
//!
//! The perceptual hashes are similarity keys, not identity: two
//! different images CAN share a dHash by chance (64 bits), and a
//! crop / rotation / heavy re-encode moves the bits — the
//! dedupe/analytics consumer compares by Hamming distance
//! ([`hamming_distance_hex`]), never by equality alone, and the
//! sha256 is the only exact-identity claim. A perceptual hash
//! says nothing about provenance or ownership.

use image::imageops::FilterType;
use image::DynamicImage;
use sha2::{Digest, Sha256};

/// Chunk size for streaming sha256: 8 KiB, the same buffer core's
/// `sha256_reader` uses — one constant for both halves of the
/// hash story.
pub const HASH_CHUNK_SIZE: usize = 8 * 1024;

/// Hash width/height for both perceptual hashes: 8x8 (dHash
/// resizes to 9x8 internally, one extra column for the gradient
/// comparisons).
pub const HASH_WIDTH: u8 = 8;
/// Hash height for both perceptual hashes: 8.
pub const HASH_HEIGHT: u8 = 8;

/// What `uploads.phash` holds: the dHash hex (16 lowercase hex
/// chars, [`HASH_WIDTH`] x [`HASH_HEIGHT`] bits). dHash is the
/// dedupe key — gradient-based, so it survives brightness and
/// exposure shifts that move the aHash; the aHash lives in the
/// metadata fragment only.
pub const PHASH_ALGORITHM: &str = "dhash-8x8-rec709-triangle";

/// Why a hash failed. No panics: every failure is a value.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// The image has a zero dimension — the resizers target 8x8 /
    /// 9x8 and a 0-pixel source has nothing to hash.
    #[error("cannot hash an empty (0-dimension) image")]
    EmptyImage,
    /// A perceptual-hash hex string is not 16 hex chars.
    #[error("invalid perceptual-hash hex {0:?}: expected 16 hex chars")]
    InvalidHashHex(String),
    /// Two fragments claimed the same top-level metadata key —
    /// merging would silently drop one, so the merge fails.
    #[error("duplicate metadata key: {0}")]
    DuplicateKey(String),
    /// A fragment is not a JSON object — fragments are
    /// `{"key": {...}}` by construction, anything else cannot merge.
    #[error("metadata fragment is not a JSON object")]
    FragmentNotAnObject,
}

/// Streaming sha256 hex of `bytes` (lowercase). The hasher is fed
/// in fixed [`HASH_CHUNK_SIZE`] chunks — the input is never copied
/// wholesale — and the result equals the one-shot hash over the
/// same bytes (see the tests: NIST vectors + 100_000-byte random
/// input vs core's `sha256_hex`).
pub fn sha256_streaming(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    for chunk in bytes.chunks(HASH_CHUNK_SIZE) {
        hasher.update(chunk);
    }
    hex_encode(&hasher.finalize())
}

/// Lowercase hex encoding (no `hex` crate needed for one call).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The two perceptual hashes of one decoded image, as 16-char
/// lowercase hex strings (8x8 bits each).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PerceptualHashes {
    /// Difference hash: gradient sign over a 9x8 luma thumbnail.
    /// The near-duplicate key — this is what `uploads.phash`
    /// holds.
    pub dhash: String,
    /// Average hash: mean threshold over an 8x8 luma thumbnail.
    /// The coarser second opinion, metadata only.
    pub ahash: String,
}

/// Compute both perceptual hashes of a decoded image.
///
/// The pipeline is fixed and documented, so a hash computed today
/// compares with one computed later: grayscale via the image
/// crate's `to_luma8` (REC709), resize with a Triangle filter
/// (8x8 for aHash, 9x8 for dHash — Triangle reproduces a linear
/// ramp exactly, so the derived-value tests below hold for any
/// reasonable filter, but the filter is pinned here anyway), then
/// the textbook bit rules. Bit order is row-major with the first
/// bit as the most significant bit of the first byte.
///
/// The image is hashed as decoded — orientation is NOT applied
/// here (the intake applies it first via `orientation.rs`, so a
/// hash always describes the upright pixels). 16-bit sources hash
/// on their 8-bit luma conversion (`round(c * 255 / 65535)`,
/// monotone — the image crate's `FromPrimitive<u16> for u8`):
/// a perceptual hash is coarse by design, the extra bits carry no
/// similarity signal. Alpha is ignored (`to_luma8` reads the RGB
/// channels only): a transparent pixel hashes by its colour, not
/// its opacity.
pub fn perceptual_hashes(image: &DynamicImage) -> Result<PerceptualHashes, HashError> {
    if image.width() == 0 || image.height() == 0 {
        return Err(HashError::EmptyImage);
    }
    let luma = image.to_luma8();
    let dhash = {
        // dHash: 9x8 thumbnail, one bit per adjacent pair — the
        // bit is set when the RIGHT pixel is brighter.
        let dthumb = image::imageops::resize(
            &luma,
            HASH_WIDTH as u32 + 1,
            HASH_HEIGHT as u32,
            FilterType::Triangle,
        );
        let mut bits = [false; 64];
        for y in 0..HASH_HEIGHT as usize {
            for x in 0..HASH_WIDTH as usize {
                bits[y * HASH_WIDTH as usize + x] = dthumb.get_pixel(x as u32, y as u32)[0]
                    < dthumb.get_pixel(x as u32 + 1, y as u32)[0];
            }
        }
        encode_bits(&bits)
    };
    let ahash = {
        // aHash: 8x8 thumbnail, one bit per pixel — the bit is set
        // when the pixel is brighter than the mean.
        let athumb = image::imageops::resize(
            &luma,
            HASH_WIDTH as u32,
            HASH_HEIGHT as u32,
            FilterType::Triangle,
        );
        let pixels = athumb.as_raw();
        let mean = pixels.iter().map(|&v| v as u32).sum::<u32>() / pixels.len() as u32;
        let mut bits = [false; 64];
        for (i, &v) in pixels.iter().enumerate() {
            bits[i] = (v as u32) > mean;
        }
        encode_bits(&bits)
    };
    Ok(PerceptualHashes { dhash, ahash })
}

/// The complete hash record for one input: the cryptographic hash
/// of the raw bytes plus the perceptual hashes of the decoded
/// pixels.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HashRecord {
    /// Streaming sha256 hex of the raw input bytes (64 chars).
    pub sha256: String,
    /// Difference hash hex of the decoded pixels (16 chars) —
    /// the value `uploads.phash` stores.
    pub dhash: String,
    /// Average hash hex of the decoded pixels (16 chars).
    pub ahash: String,
}

impl HashRecord {
    /// Compute the record for one input: the sha256 over the raw
    /// `bytes` plus the perceptual hashes of the already-decoded
    /// `image`. The caller decodes once (P2-002) and passes the
    /// pixels here — this function never re-decodes.
    pub fn compute(bytes: &[u8], image: &DynamicImage) -> Result<Self, HashError> {
        let sha256 = sha256_streaming(bytes);
        let PerceptualHashes { dhash, ahash } = perceptual_hashes(image)?;
        Ok(Self {
            sha256,
            dhash,
            ahash,
        })
    }

    /// Serialize to the exact `uploads.metadata` jsonb fragment:
    /// `{"hashes": {"sha256": ..., "dhash": ..., "ahash": ...}}`.
    /// The `hashes` key namespaces the fragment alongside `exif`,
    /// `icc` and `orientation`, so the four merge without
    /// collisions (ADR-0072 §5).
    pub fn to_metadata_value(&self) -> serde_json::Value {
        serde_json::json!({ "hashes": self })
    }
}

/// The Hamming distance between two perceptual-hash hex strings
/// (8x8 each): the number of differing bits, 0..=64. This is the
/// dedupe/analytics primitive — near-duplicates sit a few bits
/// apart, unrelated images sit ~32 apart (half the bits differ by
/// chance). Both strings must be 16 lowercase-or-uppercase hex
/// chars; anything else is a typed error, never a panic.
pub fn hamming_distance_hex(a: &str, b: &str) -> Result<usize, HashError> {
    let ha = decode_hash_hex(a)?;
    let hb = decode_hash_hex(b)?;
    Ok(ha
        .iter()
        .zip(hb.iter())
        .map(|(x, y)| (x ^ y).count_ones() as usize)
        .sum())
}

/// Pack 64 row-major bits into 16 lowercase hex chars: the first
/// bit is the most significant bit of the first byte.
fn encode_bits(bits: &[bool; 64]) -> String {
    let mut bytes = [0u8; 8];
    for (i, &bit) in bits.iter().enumerate() {
        if bit {
            bytes[i / 8] |= 1 << (7 - (i % 8));
        }
    }
    hex_encode(&bytes)
}

/// Decode a 16-char perceptual-hash hex string back to its 8
/// bytes. Any other length, or a non-hex character, is a typed
/// error — never a panic, never a silent truncation.
fn decode_hash_hex(s: &str) -> Result<[u8; 8], HashError> {
    if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(HashError::InvalidHashHex(s.to_string()));
    }
    let mut out = [0u8; 8];
    for i in 0..8 {
        out[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
            .map_err(|_| HashError::InvalidHashHex(s.to_string()))?;
    }
    Ok(out)
}

/// Assemble the single `uploads.metadata` object from the intake
/// fragments — the exif / icc / orientation / hashes values, each
/// a `{"key": {...}}` object. Top-level keys merge; a duplicate
/// key fails closed ([`HashError::DuplicateKey`]) instead of
/// silently dropping a fragment, and a non-object fragment fails
/// as [`HashError::FragmentNotAnObject`]. No fragment is ever
/// inspected or rewritten — the merge is purely structural.
pub fn merge_metadata_fragments(
    fragments: &[serde_json::Value],
) -> Result<serde_json::Value, HashError> {
    let mut merged = serde_json::Map::new();
    for fragment in fragments {
        let obj = fragment.as_object().ok_or(HashError::FragmentNotAnObject)?;
        for (key, value) in obj {
            if merged.contains_key(key) {
                return Err(HashError::DuplicateKey(key.clone()));
            }
            merged.insert(key.clone(), value.clone());
        }
    }
    Ok(serde_json::Value::Object(merged))
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::media::MediaKind;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::{ImageEncoder, Rgb, RgbImage};

    // NIST vectors: sha256("") and sha256("abc").
    const EMPTY_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC_HEX: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn sha256_nist_vectors() {
        assert_eq!(sha256_streaming(b""), EMPTY_HEX);
        assert_eq!(sha256_streaming(b"abc"), ABC_HEX);
    }

    #[test]
    fn sha256_streaming_matches_one_shot() {
        // 100_000 pseudo-random bytes, spanning many 8 KiB chunks.
        let data: Vec<u8> = (0..100_000).map(|i| (i * 37 % 251) as u8).collect();
        assert_eq!(
            sha256_streaming(&data),
            detag_core::checksum::sha256_hex(&data)
        );
    }

    #[test]
    fn sha256_streaming_chunk_boundaries() {
        // Exactly one chunk, one byte over, and just under — the
        // chunking must not change the hash at any boundary.
        for len in [HASH_CHUNK_SIZE - 1, HASH_CHUNK_SIZE, HASH_CHUNK_SIZE + 1] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(
                sha256_streaming(&data),
                detag_core::checksum::sha256_hex(&data),
                "len {len}"
            );
        }
    }

    /// 64x64 horizontal linear ramp, 0..252 in steps of 4.
    fn ramp_image() -> RgbImage {
        let mut img = RgbImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = (x * 4) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        img
    }

    /// The ramp plus deterministic ±3 noise (LCG): a photo-like
    /// input for the near-duplicate test.
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

    fn encode_png(img: &RgbImage) -> Vec<u8> {
        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgb8,
            )
            .expect("png encode");
        buf
    }

    fn encode_jpeg(img: &RgbImage, quality: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, quality)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgb8,
            )
            .expect("jpeg encode");
        buf
    }

    #[test]
    fn perceptual_hashes_are_deterministic_and_hex() {
        let img = DynamicImage::ImageRgb8(ramp_image());
        let first = perceptual_hashes(&img).expect("hash");
        let second = perceptual_hashes(&img).expect("hash again");
        assert_eq!(first, second, "same pixels, same hashes");
        for h in [&first.dhash, &first.ahash] {
            assert_eq!(h.len(), 16, "8x8 bits = 16 hex chars: {h}");
            assert!(h.bytes().all(|b| b.is_ascii_hexdigit()), "{h}");
            assert_eq!(h, &h.to_ascii_lowercase(), "lowercase hex: {h}");
        }
    }

    #[test]
    fn uniform_images_hash_to_zero() {
        // Derived from the algorithm, not from the implementation:
        // a uniform image resizes to a uniform thumbnail, so every
        // dHash comparison (left < right) is false and every pixel
        // equals the mean, so every aHash comparison (pixel > mean)
        // is false.
        for v in [0u8, 255] {
            let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(32, 32, Rgb([v, v, v])));
            let hashes = perceptual_hashes(&img).expect("hash");
            assert_eq!(hashes.dhash, "0000000000000000", "v={v}");
            assert_eq!(hashes.ahash, "0000000000000000", "v={v}");
        }
    }

    #[test]
    fn horizontal_ramp_dhash_is_all_ones() {
        // Derived: the ramp is strictly increasing left-to-right,
        // and Triangle reproduces a linear function, so every
        // resized row is strictly increasing — every dHash
        // comparison (left < right) is true, all 64 bits set. The
        // step between adjacent resized columns is ~32 grey levels,
        // so rounding cannot tie a comparison.
        let img = DynamicImage::ImageRgb8(ramp_image());
        let hashes = perceptual_hashes(&img).expect("hash");
        assert_eq!(hashes.dhash, "ffffffffffffffff");
    }

    #[test]
    fn horizontal_ramp_ahash_is_half_ones() {
        // Derived: the resized row is ~12,41,69,98,126,154,183,211
        // (linear reproduction), mean ~112 — the first four pixels
        // sit below it, the last four above, so each row hashes to
        // 0x0f and the 8 rows repeat: "0f" x 8. The boundary pixels
        // sit >=14 levels from the mean, so rounding cannot flip
        // one.
        let img = DynamicImage::ImageRgb8(ramp_image());
        let hashes = perceptual_hashes(&img).expect("hash");
        assert_eq!(hashes.ahash, "0f0f0f0f0f0f0f0f");
    }

    #[test]
    fn inverted_ramp_is_maximally_distant() {
        // Derived: inverting the ramp flips every dHash comparison
        // and every aHash threshold, so all 64 bits differ.
        let mut inverted = RgbImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = 255 - (x * 4) as u8;
                inverted.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let a = perceptual_hashes(&DynamicImage::ImageRgb8(ramp_image())).expect("hash");
        let b = perceptual_hashes(&DynamicImage::ImageRgb8(inverted)).expect("hash");
        assert_eq!(hamming_distance_hex(&a.dhash, &b.dhash).expect("dist"), 64);
        assert_eq!(hamming_distance_hex(&a.ahash, &b.ahash).expect("dist"), 64);
    }

    #[test]
    fn near_duplicate_jpeg_reencode_is_close() {
        // The AC in miniature: the same pixels re-encoded lossy
        // must stay a near-duplicate by Hamming distance, not drift
        // to an unrelated hash. Measured on this input: dHash 0,
        // aHash 0 (the 9x8/8x8 downscale averages the JPEG noise
        // away); the bound of 4 leaves room for busier images.
        let img = gradient_noise_image();
        let png = encode_png(&img);
        let jpeg = encode_jpeg(&img, 85);
        let from_png = crate::intake::decode_image(&png, MediaKind::Png).expect("png decodes");
        let from_jpeg = crate::intake::decode_image(&jpeg, MediaKind::Jpeg).expect("jpeg decodes");
        let a = perceptual_hashes(&from_png.image).expect("hash png");
        let b = perceptual_hashes(&from_jpeg.image).expect("hash jpeg");
        let d_dhash = hamming_distance_hex(&a.dhash, &b.dhash).expect("dist");
        let d_ahash = hamming_distance_hex(&a.ahash, &b.ahash).expect("dist");
        assert!(d_dhash <= 4, "dHash distance {d_dhash} > 4");
        assert!(d_ahash <= 4, "aHash distance {d_ahash} > 4");
    }

    #[test]
    fn hashes_survive_16_bit_input() {
        // A 16-bit source hashes on its 8-bit luma conversion —
        // no panic, 16-char hex, and the same pixels hash the same
        // as their 8-bit downsample (the perceptual content is
        // identical; only the depth differs).
        let mut img16 = image::ImageBuffer::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                let v = (x * 4096) as u16;
                img16.put_pixel(x, y, image::Rgb([v, v, v]));
            }
        }
        let mut buf = Vec::new();
        // PngEncoder expects NATIVE-endian u16 samples and converts
        // to big-endian on the wire itself — big-endian bytes here
        // come back byte-swapped (proven by probe: x*4096 arrived
        // as x*16), so feed it native-endian bytes.
        let raw_ne: Vec<u8> = img16
            .into_raw()
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        PngEncoder::new(&mut buf)
            .write_image(&raw_ne, 16, 16, image::ExtendedColorType::Rgb16)
            .expect("png16 encode");
        let decoded = crate::intake::decode_image(&buf, MediaKind::Png).expect("png16 decodes");
        assert_eq!(decoded.bit_depth(), 16, "fixture really is 16-bit");
        let hashes = perceptual_hashes(&decoded.image).expect("hash");
        assert_eq!(hashes.dhash.len(), 16);
        assert_eq!(hashes.ahash.len(), 16);
        // The 16-bit ramp is the same ramp as the 8-bit one, so the
        // dHash must agree with the 8-bit ramp's all-ones hash.
        assert_eq!(hashes.dhash, "ffffffffffffffff");
    }

    #[test]
    fn empty_image_is_a_typed_error() {
        let img = DynamicImage::new_rgb8(0, 0);
        assert!(matches!(
            perceptual_hashes(&img),
            Err(HashError::EmptyImage)
        ));
    }

    #[test]
    fn hamming_distance_hex_cases() {
        assert_eq!(
            hamming_distance_hex("0000000000000000", "0000000000000000").expect("dist"),
            0
        );
        assert_eq!(
            hamming_distance_hex("0000000000000000", "ffffffffffffffff").expect("dist"),
            64
        );
        assert_eq!(
            hamming_distance_hex("0000000000000000", "0000000000000001").expect("dist"),
            1
        );
        assert!(matches!(
            hamming_distance_hex("zzzzzzzzzzzzzzzz", "0000000000000000"),
            Err(HashError::InvalidHashHex(_))
        ));
        assert!(matches!(
            hamming_distance_hex("000000000000000", "0000000000000000"),
            Err(HashError::InvalidHashHex(_))
        ));
    }

    #[test]
    fn metadata_fragment_shape() {
        let record = HashRecord {
            sha256: ABC_HEX.to_string(),
            dhash: "0123456789abcdef".to_string(),
            ahash: "fedcba9876543210".to_string(),
        };
        assert_eq!(
            record.to_metadata_value(),
            serde_json::json!({
                "hashes": {
                    "sha256": ABC_HEX,
                    "dhash": "0123456789abcdef",
                    "ahash": "fedcba9876543210"
                }
            })
        );
    }

    #[test]
    fn merge_combines_fragments() {
        let merged = merge_metadata_fragments(&[
            serde_json::json!({"exif": {"orientation": 6}}),
            serde_json::json!({"icc": {"present": false}}),
            serde_json::json!({"orientation": {"transform": "rotate-90-cw"}}),
            serde_json::json!({"hashes": {"dhash": "0123456789abcdef"}}),
        ])
        .expect("merge");
        assert_eq!(
            merged,
            serde_json::json!({
                "exif": {"orientation": 6},
                "icc": {"present": false},
                "orientation": {"transform": "rotate-90-cw"},
                "hashes": {"dhash": "0123456789abcdef"}
            })
        );
    }

    #[test]
    fn merge_rejects_duplicate_key() {
        assert!(matches!(
            merge_metadata_fragments(&[
                serde_json::json!({"exif": {}}),
                serde_json::json!({"exif": {"orientation": 1}}),
            ]),
            Err(HashError::DuplicateKey(k)) if k == "exif"
        ));
    }

    #[test]
    fn merge_rejects_non_object_and_empty_is_empty() {
        assert!(matches!(
            merge_metadata_fragments(&[serde_json::json!(42)]),
            Err(HashError::FragmentNotAnObject)
        ));
        assert_eq!(
            merge_metadata_fragments(&[]).expect("empty merge"),
            serde_json::json!({})
        );
    }

    #[test]
    fn intake_fragments_merge_into_one_metadata_object() {
        // The end-to-end shape proof on a real fixture: decode,
        // parse EXIF, extract ICC, normalize orientation, hash —
        // then merge the four real fragments into the one object
        // the intake will persist as uploads.metadata.
        let bytes = include_bytes!("../tests/fixtures/icc/png-srgb.png");
        let decoded = crate::intake::decode_image(bytes, MediaKind::Png).expect("fixture decodes");
        let exif = crate::exif::parse_exif(bytes, MediaKind::Png).expect("exif parses");
        let icc = crate::icc::extract_icc(bytes, MediaKind::Png).expect("icc extracts");
        let normalized = crate::orientation::normalize_orientation(decoded.image, exif.orientation)
            .expect("orientation applies");
        let record = HashRecord::compute(bytes, &normalized.image).expect("hashes compute");
        let merged = merge_metadata_fragments(&[
            exif.to_metadata_value(),
            icc.to_metadata_value(),
            normalized.record.to_metadata_value(),
            record.to_metadata_value(),
        ])
        .expect("fragments merge");
        assert_eq!(merged["icc"]["present"], true, "fixture carries sRGB ICC");
        assert_eq!(
            merged["orientation"]["transform"], "identity",
            "fixture has no EXIF orientation"
        );
        assert_eq!(merged["hashes"]["dhash"], record.dhash);
        assert_eq!(merged["hashes"]["sha256"], sha256_streaming(bytes));
        assert_eq!(
            merged["hashes"]["sha256"],
            detag_core::checksum::sha256_hex(bytes)
        );
        // The phash column stores exactly this dHash.
        assert_eq!(merged["hashes"]["dhash"].as_str().unwrap().len(), 16);
    }
}
