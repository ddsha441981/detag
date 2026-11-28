//! Intake suite, live half (P2-009, FR-U-05): the engine's
//! `run_intake` outcome is what the core setters store — the
//! dHash lands in `uploads.phash` and the merged metadata object
//! lands in `uploads.metadata`, on a ready upload, proven against
//! a real database (the `detag_api_harness` scratch DB, never the
//! dev `pf` database).
//!
//! The in-process half lives in `crates/engine/src/suite.rs`
//! (format matrix, polyglot + bomb rejects, orientation
//! correctness, ICC preserved, hash stability). This file proves
//! only the store half: the outcome's values round-trip through
//! `set_phash` / `set_metadata` exactly, and a pending upload
//! matches nothing (intake runs on ready uploads).

mod common;

use common::spawn_app;
use detag_core::repos::uploads;
use detag_engine::budget::DecodeBudget;
use detag_engine::hash::sha256_streaming;
use detag_engine::suite::run_intake;

/// Committed sRGB PNG fixture (`crates/engine/tests/fixtures/icc/`):
/// real pixels, a real ICC profile, no EXIF orientation — the
/// intake's happy path on committed bytes.
const PNG_SRGB: &[u8] = include_bytes!("../../../crates/engine/tests/fixtures/icc/png-srgb.png");

/// The Free plan's byte cap (seed value, `crates/core/src/plans.rs`):
/// 25 MiB. A real plan value, not a made-up small number.
const FREE_MAX_BYTES: u64 = 26_214_400;

#[tokio::test]
async fn intake_outcome_stores_phash_and_metadata() {
    let app = spawn_app().await;

    // A generated unique email (harness `seed_user_unique`): the
    // scratch database persists across runs in one CI job (the Test
    // step and the Coverage step share it), so a hardcoded email
    // collides on the second run (CI-FAIL-035).
    let user = app.seed_user_unique().await;
    let created = uploads::create(
        &app.pool,
        &uploads::NewUpload {
            user_id: user.id,
            kind: "image",
            filename: "input.png",
            bytes: PNG_SRGB.len() as i64,
            sha256: &sha256_streaming(PNG_SRGB),
            mime: "image/png",
            storage_key: "dev/2026-11/c/input.png",
            parts: 1,
        },
    )
    .await
    .expect("create upload");
    assert_eq!(created.phash, None);
    assert_eq!(created.metadata, serde_json::json!({}));

    // The whole intake on the committed fixture bytes.
    let outcome =
        run_intake(PNG_SRGB, FREE_MAX_BYTES, DecodeBudget::default()).expect("fixture intakes");
    assert_eq!(outcome.kind, detag_core::media::MediaKind::Png);
    assert!(outcome.icc.present(), "fixture carries sRGB ICC");
    assert_eq!(outcome.hashes.dhash.len(), 16);

    // The intake setters guard on `ready` (P2-008): a pending
    // upload matches nothing — the hash is stored at intake,
    // after complete, not during it.
    assert!(
        uploads::set_phash(&app.pool, created.id, &outcome.hashes.dhash)
            .await
            .expect("set_phash pending")
            .is_none(),
        "storing a phash on a pending upload must match nothing"
    );
    assert!(
        uploads::set_metadata(&app.pool, created.id, &outcome.metadata)
            .await
            .expect("set_metadata pending")
            .is_none(),
        "storing metadata on a pending upload must match nothing"
    );

    // Once ready, both store, and a re-read proves the outcome's
    // values round-trip exactly — the dHash in the column, the
    // merged object in the jsonb.
    uploads::set_status_guarded(&app.pool, created.id, "pending_parts", "ready")
        .await
        .expect("mark ready")
        .expect("guard must match");
    let stored = uploads::set_phash(&app.pool, created.id, &outcome.hashes.dhash)
        .await
        .expect("set_phash")
        .expect("guard must match a ready upload");
    assert_eq!(stored.phash.as_deref(), Some(outcome.hashes.dhash.as_str()));
    let stored = uploads::set_metadata(&app.pool, created.id, &outcome.metadata)
        .await
        .expect("set_metadata")
        .expect("guard must match a ready upload");
    assert_eq!(stored.metadata, outcome.metadata);
    let by_id = uploads::by_id(&app.pool, created.id)
        .await
        .expect("by_id")
        .expect("upload must exist");
    assert_eq!(by_id.phash.as_deref(), Some(outcome.hashes.dhash.as_str()));
    assert_eq!(by_id.metadata, outcome.metadata);
    assert_eq!(by_id.metadata["hashes"]["dhash"], outcome.hashes.dhash);
    assert_eq!(by_id.metadata["icc"]["present"], true);
    assert_eq!(by_id.metadata["orientation"]["transform"], "identity");
}
