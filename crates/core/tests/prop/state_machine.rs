//! Upload state-machine properties (P0-096): the rules in
//! `crates/core/src/state_machine.rs` — terminal states, the
//! complete/abort transitions, and `verify_complete` over arbitrary
//! part lists.

use detag_core::{
    error::AppError,
    state_machine::{transition, verify_complete, UploadEvent, UploadPart, UploadStatus},
};
use proptest::prelude::*;

fn all_statuses() -> impl Strategy<Value = UploadStatus> {
    prop::sample::select(vec![
        UploadStatus::PendingParts,
        UploadStatus::Ready,
        UploadStatus::Aborted,
    ])
}

/// A valid part list: part numbers exactly 1..=N, every part
/// non-empty, sizes summing to `expected_bytes`.
fn valid_parts() -> impl Strategy<Value = (i32, i64, Vec<UploadPart>)> {
    (1i32..=10, prop::collection::vec(1u64..1024, 1..=10)).prop_map(|(n, sizes)| {
        let sizes = sizes.into_iter().take(n as usize).collect::<Vec<_>>();
        let total: u64 = sizes.iter().sum();
        let parts = sizes
            .iter()
            .enumerate()
            .map(|(i, &size_bytes)| UploadPart {
                part_number: i as i32 + 1,
                size_bytes,
            })
            .collect();
        (sizes.len() as i32, total as i64, parts)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Status spellings round-trip: `as_str` then `FromStr` is the
    /// identity, for every status.
    #[test]
    fn status_spelling_roundtrips(status in all_statuses()) {
        use std::str::FromStr;
        let spelled = status.as_str();
        let parsed = UploadStatus::from_str(spelled).expect("spelling parses");
        prop_assert_eq!(parsed, status);
    }

    /// Terminal states are terminal: any event from Ready or Aborted
    /// is InvalidStateTransition — never a silent no-op, never a
    /// success.
    #[test]
    fn terminal_states_reject_every_event(status in all_statuses()) {
        prop_assume!(status != UploadStatus::PendingParts);
        let abort = transition(status, UploadEvent::Abort);
        prop_assert!(matches!(abort, Err(AppError::InvalidStateTransition)), "abort: {:?}", abort);
        let complete = transition(
            status,
            UploadEvent::Complete { expected_parts: 1, expected_bytes: 1, parts: &[] },
        );
        prop_assert!(
            matches!(complete, Err(AppError::InvalidStateTransition)),
            "complete: {:?}", complete
        );
    }

    /// A valid part list completes: PendingParts + Complete with the
    /// row's own expectations reaches Ready.
    #[test]
    fn valid_complete_reaches_ready((n, bytes, parts) in valid_parts()) {
        let next = transition(
            UploadStatus::PendingParts,
            UploadEvent::Complete { expected_parts: n, expected_bytes: bytes, parts: &parts },
        );
        prop_assert_eq!(next, Ok(UploadStatus::Ready));
    }

    /// Part order does not matter: the check sorts — a shuffled
    /// valid list still completes.
    #[test]
    fn shuffled_valid_complete_still_reaches_ready((n, bytes, mut parts) in valid_parts()) {
        // Deterministic shuffle (reverse + rotate): proptest's own
        // shrinking stays meaningful without pulling in a RNG.
        parts.reverse();
        if parts.len() > 1 {
            parts.rotate_left(1);
        }
        let next = transition(
            UploadStatus::PendingParts,
            UploadEvent::Complete { expected_parts: n, expected_bytes: bytes, parts: &parts },
        );
        prop_assert_eq!(next, Ok(UploadStatus::Ready));
    }

    /// Dropping any part fails verification with ValidationFailed —
    /// never InvalidStateTransition (the state was legal, the parts
    /// were wrong).
    #[test]
    fn dropped_part_fails_validation((n, bytes, mut parts) in valid_parts()) {
        prop_assume!(!parts.is_empty());
        parts.pop();
        let err = verify_complete(n, bytes, &parts);
        prop_assert!(matches!(err, Err(AppError::ValidationFailed)), "err: {:?}", err);
    }

    /// An empty part fails verification.
    #[test]
    fn empty_part_fails_validation((n, bytes, mut parts) in valid_parts()) {
        prop_assume!(!parts.is_empty());
        parts[0].size_bytes = 0;
        let err = verify_complete(n, bytes, &parts);
        prop_assert!(matches!(err, Err(AppError::ValidationFailed)), "err: {:?}", err);
    }

    /// A wrong total fails verification.
    #[test]
    fn wrong_total_fails_validation((n, bytes, parts) in valid_parts()) {
        let err = verify_complete(n, bytes + 1, &parts);
        prop_assert!(matches!(err, Err(AppError::ValidationFailed)), "err: {:?}", err);
    }
}

/// Abort from PendingParts always succeeds. A plain test, not a
/// property: there is nothing to vary — generating a random status
/// and assuming it is PendingParts rejects 2/3 of cases and trips
/// proptest's global-reject limit.
#[test]
fn abort_from_pending_always_succeeds() {
    assert_eq!(
        transition(UploadStatus::PendingParts, UploadEvent::Abort),
        Ok(UploadStatus::Aborted)
    );
}
