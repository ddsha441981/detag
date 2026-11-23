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
        "report_id": "00000000-0000-0000-0000-000000000002",
        "plan_code": "pro",
        "day": "2026-11-20",
        "unit": "images",
        "used": 8,
        "limit": 10,
        "percent": 80,
        "reason": "repeated stock-agency template matches",
        // P1-101: the weekly digest renders the week's
        // totals + the plan + the tips list.
        "week_start": "2026-11-16",
        "images_used": 56,
        "images_limit": 70,
        "video_seconds_used": 0,
        "video_seconds_limit": 0,
        "tips": ["You used most of your weekly image allowance — your daily quota resets at midnight UTC.", "Your plan includes no video seconds — upgrade to Pro or Business to process video."],
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

/// P1-104: the snapshot set is complete — every template in
/// [`TEMPLATES`] has a committed `.snap` file, and the count
/// is pinned (the CI-FAIL-022/025 tripwire pattern: a new
/// template must consciously add its snapshot, not silently
/// ride on the insta test's auto-create).
#[test]
fn all_templates_have_committed_snapshots() {
    assert_eq!(
        TEMPLATES.len(),
        21,
        "a template was added or removed — update the count and the snapshot set: {TEMPLATES:?}"
    );
    let manifest = env!("CARGO_MANIFEST_DIR");
    for name in TEMPLATES {
        let snap = format!("{manifest}/tests/snapshots/snapshots__{name}.snap");
        assert!(
            std::path::Path::new(&snap).exists(),
            "template {name} has no committed snapshot: {snap}"
        );
    }
}

/// P1-104: no PII in any subject line. Every template is
/// rendered with a context stuffed with PII values (an email
/// address, a display name, a user id, a link token, an IP),
/// and no rendered subject may contain any of them. The
/// subject SOURCES are pinned too: the only variables a
/// subject template may interpolate are `app_name`,
/// `percent`, and `plan_code` — none of which is PII
/// (a plan code names a price tier, never a person). A
/// subject that interpolated an email, a name, or a token
/// would leak it to every mail log and inbox preview that
/// records subjects.
#[test]
fn no_pii_in_any_subject_line() {
    let pii_context = serde_json::json!({
        "base_url": "http://localhost:8080",
        "app_name": "Detag",
        // PII values: distinctive strings, so a substring
        // match is proof of a leak, never a coincidence.
        "email": "alice.pii-regression@example.com",
        "display_name": "Alice Pii Regression",
        "user_id": "11111111-2222-3333-4444-555555555555",
        "token": "tok-pii-regression-999",
        "ip_address": "203.0.113.99",
        "sessions_revoked": 2,
        "report_id": "00000000-0000-0000-0000-000000000002",
        "plan_code": "pro",
        "day": "2026-11-20",
        "unit": "images",
        "used": 8,
        "limit": 10,
        "percent": 80,
        "reason": "repeated stock-agency template matches",
        "week_start": "2026-11-16",
        "images_used": 56,
        "images_limit": 70,
        "video_seconds_used": 0,
        "video_seconds_limit": 0,
        "tips": ["You used most of your weekly image allowance — your daily quota resets at midnight UTC."],
    });
    let pii_values = [
        "alice.pii-regression@example.com",
        "Alice Pii Regression",
        "11111111-2222-3333-4444-555555555555",
        "tok-pii-regression-999",
        "203.0.113.99",
    ];
    let engine = TemplateEngine::new().expect("templates compile");
    for name in TEMPLATES {
        let rendered = engine
            .render(name, &pii_context)
            .unwrap_or_else(|err| panic!("template {name} must render with a PII context: {err}"));
        assert!(
            !rendered.subject.contains('\n'),
            "template {name} subject must be a single line: {:?}",
            rendered.subject
        );
        for pii in pii_values {
            assert!(
                !rendered.subject.contains(pii),
                "template {name} subject leaks PII {pii:?}: {:?}",
                rendered.subject
            );
        }
    }

    // The source pin: a subject file may interpolate only
    // the allowlisted variables. Parsed by hand (no regex
    // in dev-dependencies): every `{{ ... }}` segment's
    // first token must be in the list.
    let manifest = env!("CARGO_MANIFEST_DIR");
    for name in TEMPLATES {
        let path = format!("{manifest}/templates/{name}.subject.tera");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("subject file {path} must read: {err}"));
        for var in subject_variables(&source) {
            assert!(
                ["app_name", "percent", "plan_code"].contains(&var.as_str()),
                "template {name} subject interpolates {var:?} — only app_name, percent, plan_code are allowed: {source:?}"
            );
        }
    }
}

/// The variable names interpolated by `{{ ... }}` segments
/// in a template source, in order. Filters and field access
/// (`{{ x | upper }}`, `{{ user.name }}`) reduce to the root
/// name — the pin is about WHICH value reaches the subject,
/// not how it is formatted.
fn subject_variables(source: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let expr = after[..end].trim();
        let var = expr
            .split([' ', '|', '.'])
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !var.is_empty() {
            vars.push(var);
        }
        rest = &after[end + 2..];
    }
    vars
}

/// P1-104: the P1-029 copy rule, suite-wide. Six templates
/// pin the forbidden phrases in their own unit tests; the
/// other fifteen had no pin at all — a new template (or a
/// copy edit to an old one) could ship an
/// invisible-watermark claim and no test would notice. This
/// test renders every template and checks subject + text +
/// HTML together, so the rule holds for the whole set, not
/// just the templates somebody remembered to pin.
#[test]
fn no_forbidden_claim_in_any_rendered_template() {
    let engine = TemplateEngine::new().expect("templates compile");
    for name in TEMPLATES {
        let rendered = engine
            .render(name, &context())
            .unwrap_or_else(|err| panic!("template {name} must render: {err}"));
        for part in [
            ("subject", rendered.subject.as_str()),
            ("text", rendered.text_body.as_str()),
            ("html", rendered.html_body.as_str()),
        ] {
            let lower = part.1.to_lowercase();
            for forbidden in ["invisible", "undetectable", "synthid", "defeats"] {
                assert!(
                    !lower.contains(forbidden),
                    "template {name} {} must not claim {forbidden}",
                    part.0
                );
            }
        }
    }
}
