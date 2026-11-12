//! Tracing bootstrap + PII redaction (P0-025, NFR-42).
//!
//! Both binaries call [`init_tracing`] once at boot, after loading
//! settings: JSON logs to stdout, filtered by `PF_LOG_LEVEL` via
//! `EnvFilter`. When stdout is a terminal the logs are pretty-printed
//! for a human instead (local dev); piped or file output stays JSON,
//! which is what log collectors and `just dev`'s log files want.
//! `RUST_LOG` is intentionally NOT consulted — `PF_LOG_LEVEL` is the
//! one source of truth for the level.
//!
//! Span fields: every log line carries the fields of the spans it was
//! emitted in. The convention is `request_id` (P0-019 middleware),
//! `job_id` ([`job_span`]), and `user_id` when known — never an email
//! address, never a raw IP; see [`redact`].
//!
//! PII rule (coding standards, NFR-42): never log emails, tokens, raw
//! IPs, or file bytes. [`redact`] is the helper for free text: emails
//! are replaced with `[email]`, raw IPs with a short deterministic
//! hash (`[ip:…]`) so abuse counters can still correlate an address
//! without ever seeing it. The hash is FNV-1a — correlation-only, NOT
//! cryptographic. File bytes must never reach a log call at all: log
//! a length (or a content hash) instead.

use crate::config::Settings;
use std::io::IsTerminal;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line (production, piped output, log files).
    Json,
    /// Human-readable multi-line output (a developer's terminal).
    Pretty,
}

/// Install the global tracing subscriber: JSON to stdout, level from
/// `PF_LOG_LEVEL`; pretty instead of JSON when stdout is a terminal.
/// Panics if a global subscriber is already installed — binaries call
/// this exactly once at boot. Tests use [`build_subscriber`] with
/// `tracing::subscriber::with_default` instead (scoped, no global).
pub fn init_tracing(settings: &Settings) {
    let format = if std::io::stdout().is_terminal() {
        LogFormat::Pretty
    } else {
        LogFormat::Json
    };
    init_tracing_with_format(settings, format);
}

/// Install the global subscriber with an explicit format.
pub fn init_tracing_with_format(settings: &Settings, format: LogFormat) {
    build_subscriber(settings, format, std::io::stdout)
        .try_init()
        .expect("tracing subscriber installs (called once at boot)");
}

/// Build the subscriber without installing it (tests, embedding).
/// `writer` is usually `std::io::stdout`; tests pass a buffer.
pub fn build_subscriber<W>(
    settings: &Settings,
    format: LogFormat,
    writer: W,
) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> fmt::writer::MakeWriter<'a> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_new(&settings.log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = fmt::layer().with_writer(writer).with_ansi(false);
    let layer = match format {
        LogFormat::Json => layer.json().boxed(),
        LogFormat::Pretty => layer.pretty().boxed(),
    };
    tracing_subscriber::registry().with(filter).with(layer)
}

/// Span for one background job. `job_id` is the job's id; `user_id` is
/// recorded only when known (never an email address).
pub fn job_span(job_id: &str) -> tracing::Span {
    tracing::info_span!("job", job_id = %job_id)
}

// ---------------------------------------------------------------------------
// PII redaction
// ---------------------------------------------------------------------------

/// Redact PII from free text: emails become `[email]`, raw IPs become
/// a short deterministic hash (`[ip:…]`). Clean text is unchanged.
pub fn redact(input: &str) -> String {
    redact_ip(&redact_email(input))
}

/// Replace email-like substrings with `[email]`.
pub fn redact_email(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    // `input[..copied_up_to]` is already in `out`; matches start before
    // the `@`, so plain char-by-char copying cannot rewind to them.
    let mut copied_up_to = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            if let Some((start, end)) = email_span(input, i) {
                out.push_str(&input[copied_up_to..start]);
                out.push_str("[email]");
                copied_up_to = end;
                i = end;
                continue;
            }
        }
        i += utf8_len(bytes[i]);
    }
    out.push_str(&input[copied_up_to..]);
    out
}

/// The span of the email containing the `@` at `at`, if it looks like
/// a real address. Returns (start, end) byte indices.
fn email_span(input: &str, at: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    // Local part: [A-Za-z0-9._%+-]+ immediately before the `@`.
    let mut start = at;
    while start > 0 && is_email_local(bytes[start - 1]) {
        start -= 1;
    }
    if start == at {
        return None; // nothing before the `@`
    }
    // Domain: labels separated by '.', immediately after the `@`.
    let mut end = at + 1;
    while end < bytes.len() && is_email_domain(bytes[end]) {
        end += 1;
    }
    // A trailing '.' belongs to the sentence, not the address.
    while end > at + 1 && bytes[end - 1] == b'.' {
        end -= 1;
    }
    let domain = &input[at + 1..end];
    let mut labels = domain.split('.');
    let tld = labels.next_back()?;
    if !domain.contains('.') || tld.len() < 2 || !tld.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    if !labels.all(|label| !label.is_empty() && label.bytes().all(is_email_domain)) {
        return None;
    }
    // The local part must not start or end with '.'.
    let local = &input[start..at];
    if local.starts_with('.') || local.ends_with('.') {
        return None;
    }
    Some((start, end))
}

fn is_email_local(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

/// Replace raw IPv4/IPv6 substrings with `[ip:<hash>]` (FNV-1a, first
/// 12 hex chars — deterministic per address, correlation-only).
pub fn redact_ip(input: &str) -> String {
    let mut out = redact_ipv6(input);
    out = redact_ipv4(&out);
    out
}

/// Short deterministic hash of an IP address (FNV-1a 64-bit, 12 hex).
fn hash_ip(ip: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in ip.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("[ip:{:012x}]", hash & 0xffff_ffff_ffff)
}

/// Replace IPv6 substrings (full, `::`-compressed, or with an embedded
/// IPv4 tail) with their hash.
fn redact_ipv6(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' || bytes[i].is_ascii_hexdigit() {
            // Maximal run of hex digits, ':' and '.' (for embedded v4).
            let mut end = i;
            while end < bytes.len()
                && (bytes[end].is_ascii_hexdigit() || bytes[end] == b':' || bytes[end] == b'.')
            {
                end += 1;
            }
            let candidate = &input[i..end];
            if looks_like_ipv6(candidate) {
                out.push_str(&hash_ip(candidate));
                i = end;
                continue;
            }
            // Not an address: copy the first char, retry inside the run.
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&input[i..i + ch_len]);
            i += ch_len;
        } else {
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&input[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

/// Whether `candidate` (a run of hex/`:`/`.`) is an IPv6 address.
fn looks_like_ipv6(candidate: &str) -> bool {
    if candidate.matches(':').count() < 2 {
        return false;
    }
    // At most one "::".
    if candidate.matches("::").count() > 1 {
        return false;
    }
    let (head, tail) = match candidate.split_once("::") {
        Some((head, tail)) => (head, Some(tail)),
        None => (candidate, None),
    };
    let mut groups = 0;
    for side in [head, tail.unwrap_or("")] {
        if side.is_empty() {
            continue;
        }
        for group in side.split(':') {
            if group.contains('.') {
                // Embedded IPv4 tail: counts as two groups.
                if !looks_like_ipv4(group) {
                    return false;
                }
                groups += 2;
            } else if group.len() <= 4
                && !group.is_empty()
                && group.bytes().all(|b| b.is_ascii_hexdigit())
            {
                groups += 1;
            } else {
                return false;
            }
        }
    }
    match tail {
        Some(_) => groups < 8, // "::" fills the rest
        None => groups == 8,
    }
}

/// Replace IPv4 substrings (`a.b.c.d`, each octet 0-255) with their hash.
fn redact_ipv4(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            if let Some((end, addr)) = ipv4_span(input, i) {
                out.push_str(&hash_ip(&addr));
                i = end;
                continue;
            }
        }
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&input[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// The IPv4 address starting at `start`, if one parses there.
/// Returns (end index, address text).
fn ipv4_span(input: &str, start: usize) -> Option<(usize, String)> {
    let bytes = input.as_bytes();
    // The char before must not be a digit or '.' (else we are mid-address).
    if start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
        return None;
    }
    let mut end = start;
    let mut octets = Vec::with_capacity(4);
    for _ in 0..4 {
        let group_start = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let group = &input[group_start..end];
        if group.is_empty() || group.len() > 3 {
            return None;
        }
        let value: u32 = group.parse().ok()?;
        if value > 255 {
            return None;
        }
        octets.push(group);
        if octets.len() == 4 {
            break;
        }
        if end >= bytes.len() || bytes[end] != b'.' {
            return None;
        }
        end += 1; // consume the '.'
    }
    // The char after must not be a digit or '.' (else more groups follow).
    if end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        return None;
    }
    Some((end, octets.join(".")))
}

/// Whether `candidate` parses as an IPv4 address (used for embedded tails).
fn looks_like_ipv4(candidate: &str) -> bool {
    let parts: Vec<&str> = candidate.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|b| b.is_ascii_digit())
                && part.parse::<u32>().map(|v| v <= 255).unwrap_or(false)
        })
}

/// Length in bytes of the UTF-8 char starting with `first`.
fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn settings_with_level(level: &str) -> Settings {
        Settings {
            log_level: level.to_string(),
            ..Settings::default()
        }
    }

    // -- redaction ---------------------------------------------------------

    #[test]
    fn redact_removes_emails() {
        assert_eq!(
            redact("contact alice@example.com please"),
            "contact [email] please"
        );
        assert_eq!(redact("user+tag@mail.co.uk wrote"), "[email] wrote");
        // Trailing sentence punctuation survives.
        assert_eq!(redact("mail me at a@b.com."), "mail me at [email].");
    }

    #[test]
    fn redact_leaves_non_emails_alone() {
        assert_eq!(redact("no @ here, just text"), "no @ here, just text");
        assert_eq!(redact("a@ (no domain)"), "a@ (no domain)");
        assert_eq!(redact("a@b (no TLD dot)"), "a@b (no TLD dot)");
        assert_eq!(
            redact("clean text, nothing to redact"),
            "clean text, nothing to redact"
        );
    }

    #[test]
    fn redact_hashes_ipv4() {
        let one = redact("from 10.0.0.1 and done");
        assert!(!one.contains("10.0.0.1"), "raw IP must not survive: {one}");
        assert!(one.contains("[ip:"), "hashed IP expected: {one}");
        // Deterministic: same address, same hash; different, different.
        assert_eq!(redact("10.0.0.1"), redact("10.0.0.1"));
        assert_ne!(redact("10.0.0.1"), redact("10.0.0.2"));
        // Octet > 255 is not an address.
        assert_eq!(redact("192.168.1.300"), "192.168.1.300");
        // Five groups is not an address either.
        assert_eq!(redact("1.10.0.0.1"), "1.10.0.0.1");
    }

    #[test]
    fn redact_hashes_ipv6() {
        for addr in [
            "2001:db8::1",
            "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
            "fe80::1ff:fe23:4567:890a",
            "::ffff:192.0.2.1",
            "::1",
        ] {
            let out = redact(&format!("from {addr} and done"));
            assert!(!out.contains(addr), "raw IPv6 must not survive: {out}");
            assert!(out.contains("[ip:"), "hashed IP expected: {out}");
        }
        // A single colon pair is not an address.
        assert_eq!(redact("see 12:30 o'clock"), "see 12:30 o'clock");
    }

    #[test]
    fn redact_combined_text() {
        let out = redact("user bob@x.io from 10.1.2.3 did the thing");
        assert_eq!(
            out,
            format!("user [email] from {} did the thing", hash_ip("10.1.2.3"))
        );
    }

    // -- JSON logs -----------------------------------------------------------

    /// A writer that collects into a shared buffer (for tests).
    #[derive(Clone, Default)]
    struct BufWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl BufWriter {
        fn take_string(&self) -> String {
            let bytes = self.buf.lock().unwrap().clone();
            String::from_utf8(bytes).expect("logs are UTF-8")
        }
    }

    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> fmt::writer::MakeWriter<'a> for BufWriter {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn json_logs_parse_and_carry_span_fields() {
        let writer = BufWriter::default();
        let subscriber = build_subscriber(
            &settings_with_level("info"),
            LogFormat::Json,
            writer.clone(),
        );
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                "job",
                job_id = "job-1",
                request_id = "req-1",
                user_id = "user-1",
            );
            let _enter = span.enter();
            tracing::info!("hello");
        });
        let output = writer.take_string();
        assert!(!output.is_empty(), "expected at least one log line");
        for line in output.lines() {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("every log line must parse as JSON");
            assert_eq!(value["level"], "INFO");
            // The message lives under "fields" in tracing-subscriber JSON.
            assert_eq!(value["fields"]["message"], "hello");
            // Span fields land under "span".
            let span = &value["span"];
            assert_eq!(span["name"], "job");
            assert_eq!(span["job_id"], "job-1");
            assert_eq!(span["request_id"], "req-1");
            assert_eq!(span["user_id"], "user-1");
        }
    }

    #[test]
    fn level_filter_comes_from_settings() {
        let writer = BufWriter::default();
        let subscriber = build_subscriber(
            &settings_with_level("error"),
            LogFormat::Json,
            writer.clone(),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("dropped by filter");
            tracing::error!("kept by filter");
        });
        let output = writer.take_string();
        assert!(!output.contains("dropped by filter"));
        assert!(output.contains("kept by filter"));
    }

    #[test]
    fn pretty_format_is_not_json() {
        let writer = BufWriter::default();
        let subscriber = build_subscriber(
            &settings_with_level("info"),
            LogFormat::Pretty,
            writer.clone(),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("pretty hello");
        });
        let output = writer.take_string();
        assert!(output.contains("pretty hello"));
        assert!(
            serde_json::from_str::<serde_json::Value>(&output).is_err(),
            "pretty output must not parse as JSON: {output}"
        );
    }
}
