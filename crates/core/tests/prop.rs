//! Property tests (P0-096): proptest harness for the pure codecs and
//! invariants in `detag-core`. Every `proptest!` block runs 1000 cases
//! (`ProptestConfig::with_cases(1000)` — the P0-096 AC); strategies
//! use proptest's standard generators so failures shrink to a minimal
//! case automatically.
//!
//! Covered here:
//! - `cursor` — `pagination::Cursor` encode/decode roundtrip.
//! - `keys` — `redis::keys` shape invariants (`pf:{ns}:{key}`).
//! - `state_machine` — upload transitions + `verify_complete`.
//!
//! NOT covered here: the mask codec — no mask codec exists yet
//! (engine is a Phase 2 placeholder). Its roundtrip properties land
//! in `tests/prop/mask.rs` with the codec; do not invent a codec
//! here just to test it.
//!
//! Runs on every commit (`cargo test --workspace` picks this target
//! up) and on the nightly job (`.github/workflows/nightly.yml`,
//! scheduled re-run of the same 1000-case config).
//!
//! Note: proptest's failure-persistence file does not work for
//! integration tests (it looks for a `lib.rs`/`main.rs` next to the
//! test target and warns `FileFailurePersistence::SourceParallel`);
//! a found failure still prints its shrunk minimal case, which is
//! what gets turned into a regression test.

#[path = "prop/cursor.rs"]
mod cursor;
#[path = "prop/keys.rs"]
mod keys;
#[path = "prop/state_machine.rs"]
mod state_machine;
