//! Snapshot tests (P1-029): the exact bytes of every transactional
//! template — subject + plain text + HTML — pinned with insta.
//!
//! The rendered mail is what a user actually receives, so the
//! snapshots are the copy review surface: a changed snapshot is
//! reviewed like a code change (see `05-QUALITY/01-test-strategy.md`,
//! "Snapshot testing"). Accept with `cargo insta review` (or
//! `INSTA_UPDATE=always cargo test -p detag-mailer --test snapshots`
//! without the CLI), then commit the `.snap` files. CI runs with
//! `INSTA_UPDATE=no` and fails on any unaccepted snapshot.
//!
//! The context is fixed (never generated): snapshots must be
//! deterministic. The token, base URL and session count are the
//! same values the unit tests use, so a snapshot diff is always a
//! template change, never test data drift.

use detag_mailer::{TemplateEngine, TEMPLATES};
use serde_json::json;

/// Fixed render context for every template (all variables any
/// template needs — a template that needs a variable not listed
/// here fails loudly instead of snapshotting a hole).
fn context() -> serde_json::Value {
    json!({
        "base_url": "http://localhost:8080",
        "app_name": "Detag",
        "token": "tok-abc-123",
        "sessions_revoked": 2,
        "user_id": "00000000-0000-0000-0000-000000000001",
    })
}

/// One snapshot per template: subject line, text body and HTML
/// body together, so a copy change in any part shows up in
/// exactly one diff.
#[test]
fn all_templates_snapshot() {
    let engine = TemplateEngine::new().expect("templates compile");
    for name in TEMPLATES {
        let rendered = engine
            .render(name, &context())
            .unwrap_or_else(|err| panic!("template {name} must render: {err}"));
        insta::assert_snapshot!(
            *name,
            format!(
                "subject: {}\n\n--- text ---\n{}\n--- html ---\n{}",
                rendered.subject, rendered.text_body, rendered.html_body
            )
        );
    }
}
