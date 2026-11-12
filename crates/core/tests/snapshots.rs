//! Snapshot tests (P0-095): the JSON the api serves, pinned with insta.
//!
//! What is snapshotted here:
//! - `problem+json` bodies (`AppError::to_problem`) in both profiles —
//!   the exact bytes a client sees, including the profile rule that
//!   production never shows caller detail and that `request_id` is
//!   omitted when there is none.
//! - The `Page<T>` list envelope (`items` + `next_cursor`).
//!
//! What is NOT snapshotted yet: engine quality reports — the engine
//! is a Phase 2 placeholder, so no report type exists to snapshot.
//! The Phase 2 golden suite snapshots reports with this same harness
//! (same review flow, same CI gate); do not invent a report type
//! here just to have a snapshot.
//!
//! Review flow (see `05-QUALITY/01-test-strategy.md`, "Snapshot
//! testing"): snapshots are code — a changed snapshot is reviewed
//! like a code change. Accept with `cargo insta review` (or
//! `INSTA_UPDATE=always cargo test -p detag-core --test snapshots`
//! without the CLI), then commit the `.snap` files. CI runs with
//! `INSTA_UPDATE=no` and fails on any unaccepted snapshot.

use detag_core::{
    error::{AppError, Problem, Profile},
    pagination::{Cursor, Page},
};
use uuid::Uuid;

/// Fixed request id for the snapshots (never generated: snapshots
/// must be deterministic).
const REQUEST_ID: &str = "01JREQID123";

fn problem(
    err: AppError,
    profile: Profile,
    request_id: Option<&str>,
    detail: Option<&str>,
) -> Problem {
    err.to_problem(profile, request_id, detail)
}

#[test]
fn problem_validation_failed_production() {
    insta::assert_json_snapshot!(problem(
        AppError::ValidationFailed,
        Profile::Production,
        Some(REQUEST_ID),
        Some("email must be valid"),
    )
    .to_json(),);
}

#[test]
fn problem_validation_failed_development() {
    insta::assert_json_snapshot!(problem(
        AppError::ValidationFailed,
        Profile::Development,
        Some(REQUEST_ID),
        Some("email must be valid"),
    )
    .to_json(),);
}

#[test]
fn problem_not_found_no_request_id() {
    insta::assert_json_snapshot!(
        problem(AppError::NotFound, Profile::Production, None, None).to_json(),
    );
}

#[test]
fn problem_unauthenticated_production() {
    insta::assert_json_snapshot!(problem(
        AppError::Unauthenticated,
        Profile::Production,
        Some(REQUEST_ID),
        None
    )
    .to_json(),);
}

#[test]
fn problem_cache_unavailable_production() {
    insta::assert_json_snapshot!(problem(
        AppError::CacheUnavailable,
        Profile::Production,
        Some(REQUEST_ID),
        None
    )
    .to_json(),);
}

#[test]
fn page_of_problems() {
    let cursor = Cursor {
        ts: 1_790_000_000_000,
        id: Uuid::parse_str("019a1b2c-3d4e-7f80-90a1-b2c3d4e5f607").expect("fixed uuid parses"),
    };
    let page = Page {
        items: vec![
            problem(
                AppError::ValidationFailed,
                Profile::Production,
                Some(REQUEST_ID),
                None,
            ),
            problem(AppError::NotFound, Profile::Production, None, None),
        ],
        next_cursor: Some(cursor.encode()),
    };
    insta::assert_json_snapshot!(page);
}

#[test]
fn page_empty_last() {
    let page: Page<Problem> = Page {
        items: vec![],
        next_cursor: None,
    };
    insta::assert_json_snapshot!(page);
}
