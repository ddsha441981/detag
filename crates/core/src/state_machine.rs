//! Upload state machine (P0-057, E0.6): the `uploads` table's
//! `pending_parts → ready | aborted` transitions.
//!
//! The machine is pure: it knows the rules, not the database.
//! The repository (`repos::uploads`) persists transitions with a
//! guarded `UPDATE ... WHERE status = $from` so a concurrent
//! transition cannot slip through; the Phase 3 upload service
//! composes the two — fetch the row, run [`transition`], then the
//! guarded update — with the S3 `complete_multipart` call between
//! verification and persistence (the server verifies the client's
//! part list first, then tells S3 to complete, then marks the row
//! ready).
//!
//! Rules:
//! - `pending_parts → ready` (complete): [`verify_complete`]
//!   checks the part list against the row's expectations — part
//!   count, contiguous part numbers 1..=N, every part non-empty,
//!   sizes summing to the row's `bytes`. Failures are
//!   `AppError::ValidationFailed` (PF-2002, 400): the client
//!   reported the wrong parts.
//! - `pending_parts → aborted` (abort): always legal.
//! - `ready` and `aborted` are terminal: any event from them is
//!   `AppError::InvalidStateTransition` (PF-5002, 409).
//!
//! Deliberately NOT checked here: S3's 5 MiB minimum per
//! non-final part. That is the backend's rule, enforced at
//! `complete_multipart` time and surfaced as a backend error;
//! this machine checks only what the row knows (count + sizes).

use crate::error::AppError;

/// The `uploads.status` values (`CHECK` in migration 002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UploadStatus {
    /// Parts are still being uploaded.
    PendingParts,
    /// All parts uploaded and verified; the object is complete.
    Ready,
    /// The upload was aborted; its parts are gone.
    Aborted,
}

impl UploadStatus {
    /// The database spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingParts => "pending_parts",
            Self::Ready => "ready",
            Self::Aborted => "aborted",
        }
    }
}

impl std::str::FromStr for UploadStatus {
    type Err = AppError;

    /// Parse the database spelling.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending_parts" => Ok(Self::PendingParts),
            "ready" => Ok(Self::Ready),
            "aborted" => Ok(Self::Aborted),
            _ => Err(AppError::ValidationFailed),
        }
    }
}

/// One part as the client reports it at complete time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadPart {
    /// 1-based part number.
    pub part_number: i32,
    /// Bytes in this part, as uploaded.
    pub size_bytes: u64,
}

/// Verify a complete request against the row's expectations:
/// `expected_parts` is the row's `parts`, `expected_bytes` the
/// row's `bytes`.
pub fn verify_complete(
    expected_parts: i32,
    expected_bytes: i64,
    parts: &[UploadPart],
) -> Result<(), AppError> {
    if parts.is_empty() {
        return Err(AppError::ValidationFailed);
    }
    if parts.len() as i64 != i64::from(expected_parts) {
        return Err(AppError::ValidationFailed);
    }
    // Part numbers must be exactly 1..=N, each once. Sorted
    // comparison catches duplicates, gaps, and out-of-range
    // numbers in one check.
    let mut numbers: Vec<i32> = parts.iter().map(|p| p.part_number).collect();
    numbers.sort_unstable();
    let want: Vec<i32> = (1..=expected_parts).collect();
    if numbers != want {
        return Err(AppError::ValidationFailed);
    }
    if parts.iter().any(|p| p.size_bytes == 0) {
        return Err(AppError::ValidationFailed);
    }
    let total: u64 = parts.iter().map(|p| p.size_bytes).sum();
    let expected = u64::try_from(expected_bytes).map_err(|_| AppError::ValidationFailed)?;
    if total != expected {
        return Err(AppError::ValidationFailed);
    }
    Ok(())
}

/// An event the upload state machine accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadEvent<'a> {
    /// Finish the upload with the client's part list.
    Complete {
        /// The row's `parts`.
        expected_parts: i32,
        /// The row's `bytes`.
        expected_bytes: i64,
        /// The parts the client reports.
        parts: &'a [UploadPart],
    },
    /// Abandon the upload.
    Abort,
}

/// Apply `event` to `from`, returning the next status.
pub fn transition(from: UploadStatus, event: UploadEvent<'_>) -> Result<UploadStatus, AppError> {
    match (from, event) {
        (
            UploadStatus::PendingParts,
            UploadEvent::Complete {
                expected_parts,
                expected_bytes,
                parts,
            },
        ) => {
            verify_complete(expected_parts, expected_bytes, parts)?;
            Ok(UploadStatus::Ready)
        }
        (UploadStatus::PendingParts, UploadEvent::Abort) => Ok(UploadStatus::Aborted),
        _ => Err(AppError::InvalidStateTransition),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(sizes: &[u64]) -> Vec<UploadPart> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &size_bytes)| UploadPart {
                part_number: i as i32 + 1,
                size_bytes,
            })
            .collect()
    }

    #[test]
    fn status_spellings_round_trip() {
        use std::str::FromStr;
        for status in [
            UploadStatus::PendingParts,
            UploadStatus::Ready,
            UploadStatus::Aborted,
        ] {
            assert_eq!(UploadStatus::from_str(status.as_str()), Ok(status));
        }
        assert!(matches!(
            UploadStatus::from_str("queued"),
            Err(AppError::ValidationFailed)
        ));
    }

    #[test]
    fn complete_happy_path() {
        let p = parts(&[5_000_000, 5_000_000, 123]);
        assert_eq!(
            transition(
                UploadStatus::PendingParts,
                UploadEvent::Complete {
                    expected_parts: 3,
                    expected_bytes: 10_000_123,
                    parts: &p,
                }
            ),
            Ok(UploadStatus::Ready)
        );
    }

    #[test]
    fn complete_rejects_wrong_count() {
        let p = parts(&[100, 200]);
        assert!(matches!(
            transition(
                UploadStatus::PendingParts,
                UploadEvent::Complete {
                    expected_parts: 3,
                    expected_bytes: 300,
                    parts: &p,
                }
            ),
            Err(AppError::ValidationFailed)
        ));
    }

    #[test]
    fn complete_rejects_wrong_total_size() {
        let p = parts(&[100, 200]);
        assert!(matches!(
            transition(
                UploadStatus::PendingParts,
                UploadEvent::Complete {
                    expected_parts: 2,
                    expected_bytes: 301,
                    parts: &p,
                }
            ),
            Err(AppError::ValidationFailed)
        ));
    }

    #[test]
    fn complete_rejects_gaps_duplicates_and_out_of_range() {
        // Gap: part 2 missing, part 3 present.
        let gap = vec![
            UploadPart {
                part_number: 1,
                size_bytes: 100,
            },
            UploadPart {
                part_number: 3,
                size_bytes: 100,
            },
        ];
        // Duplicate: part 1 twice.
        let dup = vec![
            UploadPart {
                part_number: 1,
                size_bytes: 100,
            },
            UploadPart {
                part_number: 1,
                size_bytes: 100,
            },
        ];
        // Out of range: part 0 / part 3 of 2.
        let zero = vec![
            UploadPart {
                part_number: 0,
                size_bytes: 100,
            },
            UploadPart {
                part_number: 1,
                size_bytes: 100,
            },
        ];
        for bad in [gap, dup, zero] {
            assert!(matches!(
                transition(
                    UploadStatus::PendingParts,
                    UploadEvent::Complete {
                        expected_parts: 2,
                        expected_bytes: 200,
                        parts: &bad,
                    }
                ),
                Err(AppError::ValidationFailed)
            ));
        }
    }

    #[test]
    fn complete_rejects_empty_parts_and_empty_part() {
        assert!(matches!(
            transition(
                UploadStatus::PendingParts,
                UploadEvent::Complete {
                    expected_parts: 1,
                    expected_bytes: 0,
                    parts: &[],
                }
            ),
            Err(AppError::ValidationFailed)
        ));
        let p = vec![UploadPart {
            part_number: 1,
            size_bytes: 0,
        }];
        assert!(matches!(
            transition(
                UploadStatus::PendingParts,
                UploadEvent::Complete {
                    expected_parts: 1,
                    expected_bytes: 0,
                    parts: &p,
                }
            ),
            Err(AppError::ValidationFailed)
        ));
    }

    #[test]
    fn abort_only_from_pending() {
        assert_eq!(
            transition(UploadStatus::PendingParts, UploadEvent::Abort),
            Ok(UploadStatus::Aborted)
        );
        assert!(matches!(
            transition(UploadStatus::Ready, UploadEvent::Abort),
            Err(AppError::InvalidStateTransition)
        ));
        assert!(matches!(
            transition(UploadStatus::Aborted, UploadEvent::Abort),
            Err(AppError::InvalidStateTransition)
        ));
    }

    #[test]
    fn ready_and_aborted_are_terminal() {
        let p = parts(&[100]);
        for from in [UploadStatus::Ready, UploadStatus::Aborted] {
            assert!(matches!(
                transition(
                    from,
                    UploadEvent::Complete {
                        expected_parts: 1,
                        expected_bytes: 100,
                        parts: &p,
                    }
                ),
                Err(AppError::InvalidStateTransition)
            ));
        }
    }

    #[test]
    fn error_codes_are_the_taxonomy_ones() {
        // PF-2002 (400) for bad part lists, PF-5002 (409) for
        // wrong-state transitions.
        assert_eq!(AppError::ValidationFailed.code(), "PF-2002");
        assert_eq!(AppError::InvalidStateTransition.code(), "PF-5002");
    }
}
