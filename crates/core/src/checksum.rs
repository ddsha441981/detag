//! Streaming integrity checksums (P0-058, E0.6; FR-U-05).
//!
//! The upload flow's hash story:
//! - At creation the client reports the whole-object sha256
//!   (`uploads.sha256`) — a claim, not yet verified.
//! - At complete the client reports one sha256 per part, and the
//!   server recomputes: [`UploadVerifier`] streams each part's
//!   bytes (ranged reads from storage, supplied by the caller as
//!   an `AsyncRead`) through a hasher, compares per-part hashes,
//!   and returns the whole-object hash. A mismatch is
//!   `AppError::PartChecksumMismatch` (PF-2004, 400); a malformed
//!   report (unknown part, repeated part, missing part) is
//!   `AppError::ValidationFailed` (PF-2002, 400).
//! - On success the service stores the server-computed hash with
//!   `repos::uploads::set_sha256` — the stored value is the
//!   verified hash, not the claim — and marks the upload ready.
//!   That hash feeds dedupe and abuse analytics (FR-U-05); the
//!   perceptual hash (`uploads.phash`) is computed at intake in
//!   Phase 2, not here.
//!
//! Everything here streams with a fixed 8 KiB buffer: no part —
//! and no whole object — is ever held in memory.

use std::collections::HashSet;

use sha2::{Digest, Sha256};
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;

use crate::error::AppError;

/// Read buffer for streaming hashes: 8 KiB.
const HASH_BUF_SIZE: usize = 8 * 1024;

/// One-shot sha256 hex of `data` (lowercase). For tests and small
/// values; large inputs go through [`UploadVerifier`] or
/// [`sha256_reader`].
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

/// Streaming sha256 hex of everything `reader` yields.
pub async fn sha256_reader<R: AsyncRead + Unpin>(mut reader: R) -> Result<String, AppError> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; HASH_BUF_SIZE];
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|_| AppError::Internal)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// Lowercase hex encoding (no `hex` crate needed for one call).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Case-insensitive hex comparison: hashes are stored lowercase,
/// but a client may report uppercase.
pub fn hash_matches(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.eq_ignore_ascii_case(b)
}

/// One part's expected hash, as the client reports it at complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedPart {
    /// 1-based part number.
    pub part_number: i32,
    /// Expected sha256 hex (any case).
    pub sha256: String,
}

/// Verifies an upload's parts one streaming pass at a time and
/// yields the whole-object hash.
///
/// Construct with the client's per-part report, call
/// [`verify_part`](Self::verify_part) once per part **in ascending
/// part-number order**, then [`finish`](Self::finish) to check
/// every expected part was seen and get the whole-object hash.
/// Order matters because the whole-object hash is computed over
/// the bytes in part order — a single streaming pass cannot hash
/// part 2 before part 1. Every byte read is fed to both its part
/// hasher and the whole-object hasher, so one pass verifies the
/// parts and computes the final hash.
pub struct UploadVerifier {
    expected: Vec<ExpectedPart>,
    /// Index into `expected` (sorted by part number) of the next
    /// part `verify_part` must be called with. Parts arrive in
    /// ascending order — the whole-object hash is order-dependent.
    /// Also the count of verified parts, so `finish` can tell
    /// whether any expected part was never verified.
    next_index: usize,
    whole: Sha256,
}

impl UploadVerifier {
    /// New verifier for `expected` parts. An empty report is a
    /// caller error — complete with no parts is rejected here,
    /// not after streaming nothing.
    pub fn new(expected: &[ExpectedPart]) -> Result<Self, AppError> {
        if expected.is_empty() {
            return Err(AppError::ValidationFailed);
        }
        // Part numbers must be unique in the report itself.
        let mut numbers = HashSet::new();
        for part in expected {
            if !numbers.insert(part.part_number) {
                return Err(AppError::ValidationFailed);
            }
            if !is_valid_hex(&part.sha256) {
                return Err(AppError::ValidationFailed);
            }
        }
        let mut sorted = expected.to_vec();
        sorted.sort_by_key(|p| p.part_number);
        Ok(Self {
            expected: sorted,
            next_index: 0,
            whole: Sha256::new(),
        })
    }

    /// Stream one part's bytes through the hashers and compare.
    /// `reader` is the part's bytes (a ranged storage read in the
    /// real flow, a slice in tests). The part must be the next
    /// expected one in ascending order, not yet verified, and its
    /// hash must match — a hash mismatch is `PartChecksumMismatch`
    /// (PF-2004); anything else is `ValidationFailed` (PF-2002).
    pub async fn verify_part<R: AsyncRead + Unpin>(
        &mut self,
        part_number: i32,
        mut reader: R,
    ) -> Result<(), AppError> {
        let expected = self
            .expected
            .get(self.next_index)
            .filter(|p| p.part_number == part_number)
            .ok_or(AppError::ValidationFailed)?;
        self.next_index += 1;
        let mut part_hasher = Sha256::new();
        let mut buf = [0u8; HASH_BUF_SIZE];
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|_| AppError::Internal)?;
            if n == 0 {
                break;
            }
            part_hasher.update(&buf[..n]);
            self.whole.update(&buf[..n]);
        }
        let computed = hex_encode(&part_hasher.finalize());
        if !hash_matches(&computed, &expected.sha256) {
            return Err(AppError::PartChecksumMismatch);
        }
        Ok(())
    }

    /// All parts verified: returns the whole-object sha256 hex.
    /// Fails when any expected part was never verified.
    pub fn finish(self) -> Result<String, AppError> {
        if self.next_index != self.expected.len() {
            return Err(AppError::ValidationFailed);
        }
        Ok(hex_encode(&self.whole.clone().finalize()))
    }
}

/// A 64-char lowercase-or-uppercase hex string (a sha256).
fn is_valid_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    // NIST vectors: sha256("") and sha256("abc").
    const EMPTY_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC_HEX: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn known_vectors() {
        assert_eq!(sha256_hex(b""), EMPTY_HEX);
        assert_eq!(sha256_hex(b"abc"), ABC_HEX);
        assert!(hash_matches(ABC_HEX, &ABC_HEX.to_ascii_uppercase()));
        assert!(!hash_matches(ABC_HEX, EMPTY_HEX));
    }

    #[tokio::test]
    async fn reader_matches_one_shot() {
        let data: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
        assert_eq!(
            sha256_reader(&data[..]).await.expect("hash"),
            sha256_hex(&data)
        );
    }

    fn two_parts() -> (Vec<ExpectedPart>, Vec<u8>, Vec<u8>) {
        let p1 = b"hello ".to_vec();
        let p2 = b"world".to_vec();
        let expected = vec![
            ExpectedPart {
                part_number: 1,
                sha256: sha256_hex(&p1),
            },
            ExpectedPart {
                part_number: 2,
                sha256: sha256_hex(&p2),
            },
        ];
        (expected, p1, p2)
    }

    #[tokio::test]
    async fn verify_happy_path_returns_whole_hash() {
        let (expected, p1, p2) = two_parts();
        let mut v = UploadVerifier::new(&expected).expect("valid report");
        v.verify_part(1, &p1[..]).await.expect("part 1");
        v.verify_part(2, &p2[..]).await.expect("part 2");
        assert_eq!(v.finish().expect("finish"), sha256_hex(b"hello world"));
    }

    #[tokio::test]
    async fn verify_rejects_out_of_order_parts() {
        // The whole-object hash is order-dependent, so parts must
        // arrive in ascending order — part 2 before part 1 fails.
        let (expected, p1, p2) = two_parts();
        let mut v = UploadVerifier::new(&expected).expect("valid report");
        assert!(matches!(
            v.verify_part(2, &p2[..]).await,
            Err(AppError::ValidationFailed)
        ));
        v.verify_part(1, &p1[..]).await.expect("part 1");
        v.verify_part(2, &p2[..]).await.expect("part 2");
        assert_eq!(v.finish().expect("finish"), sha256_hex(b"hello world"));
    }

    #[tokio::test]
    async fn verify_part_mismatch_is_pf_2004() {
        let (mut expected, p1, p2) = two_parts();
        expected[1].sha256 = sha256_hex(b"wrld");
        let mut v = UploadVerifier::new(&expected).expect("valid report");
        v.verify_part(1, &p1[..]).await.expect("part 1");
        let err = v
            .verify_part(2, &p2[..])
            .await
            .expect_err("part 2 hash must mismatch");
        assert_eq!(err, AppError::PartChecksumMismatch);
        assert_eq!(err.code(), "PF-2004");
    }

    #[tokio::test]
    async fn verify_rejects_unknown_repeated_and_missing_parts() {
        let (expected, p1, _p2) = two_parts();
        let mut v = UploadVerifier::new(&expected).expect("valid report");
        assert!(matches!(
            v.verify_part(3, &p1[..]).await,
            Err(AppError::ValidationFailed)
        ));
        v.verify_part(1, &p1[..]).await.expect("part 1");
        assert!(matches!(
            v.verify_part(1, &p1[..]).await,
            Err(AppError::ValidationFailed)
        ));
        // Part 2 never verified: finish fails.
        assert!(matches!(v.finish(), Err(AppError::ValidationFailed)));
    }

    #[test]
    fn new_rejects_empty_report_duplicates_and_bad_hex() {
        assert!(matches!(
            UploadVerifier::new(&[]),
            Err(AppError::ValidationFailed)
        ));
        let dup = vec![
            ExpectedPart {
                part_number: 1,
                sha256: ABC_HEX.to_string(),
            },
            ExpectedPart {
                part_number: 1,
                sha256: ABC_HEX.to_string(),
            },
        ];
        assert!(matches!(
            UploadVerifier::new(&dup),
            Err(AppError::ValidationFailed)
        ));
        let bad_hex = vec![ExpectedPart {
            part_number: 1,
            sha256: "not-a-hash".to_string(),
        }];
        assert!(matches!(
            UploadVerifier::new(&bad_hex),
            Err(AppError::ValidationFailed)
        ));
    }
}
