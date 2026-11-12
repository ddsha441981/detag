//! P0-072: the access log, verified end to end through the real
//! router (the decision function itself is unit-tested in
//! `request_id`; these tests prove the middleware emits the event
//! per that decision).
//!
//! Every test uses fresh request ids and searches the shared log
//! buffer for its own ids only, so the tests stay exact while
//! running in parallel. The capturing subscriber records event
//! lines only (no span lifecycle events), so a request id appears
//! in the buffer if and only if its `request completed` event was
//! emitted.
//!
//! Slow requests (> 250 ms) cannot be driven through this router —
//! every route answers immediately — so the slow rule stays
//! unit-verified (`should_log` with a 251 ms elapsed).

mod common;

use axum::http::{Method, Request};
use common::spawn_app;
use detag_api::request_id::{is_sampled, should_log};
use std::{
    io::Write,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use uuid::Uuid;

static LOG_BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

#[derive(Clone)]
struct TestWriter(Arc<Mutex<Vec<u8>>>);

impl Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Install the capturing subscriber once; returns the shared log buffer.
fn install_subscriber() -> Arc<Mutex<Vec<u8>>> {
    Arc::clone(LOG_BUF.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer_buf = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || TestWriter(Arc::clone(&writer_buf)))
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("test subscriber installs once");
        buf
    }))
}

/// The `request completed` event lines captured so far.
fn event_lines(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<String> {
    let logs = buf.lock().unwrap();
    let text = String::from_utf8_lossy(&logs);
    text.lines()
        .filter(|line| line.contains("request completed"))
        .map(str::to_string)
        .collect()
}

/// The event line for one request id, if its access event fired.
fn event_for(buf: &Arc<Mutex<Vec<u8>>>, id: &str) -> Option<String> {
    event_lines(buf)
        .into_iter()
        .find(|line| line.contains(&format!("request_id={id}")))
}

/// Send one request with an explicit id through the app.
async fn send(app: &common::TestApp, method: Method, uri: &str, id: &str) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", id)
        .body(axum::body::Body::empty())
        .expect("test request builds");
    let res = app.request(req).await;
    // The status is the test's business; here it only has to answer.
    assert!(res.status.is_success() || res.status.is_client_error());
}

fn fresh_id() -> String {
    Uuid::now_v7().to_string()
}

#[tokio::test]
async fn errors_always_emit_the_access_event() {
    let buf = install_subscriber();
    let app = spawn_app().await;
    let ids: Vec<String> = (0..20).map(|_| fresh_id()).collect();
    for id in &ids {
        send(&app, Method::GET, "/nope", id).await;
    }
    for id in &ids {
        let line = event_for(&buf, id)
            .unwrap_or_else(|| panic!("404 for {id} must emit the access event"));
        assert!(
            line.contains("status=404"),
            "the event must carry the status: {line}"
        );
        assert!(
            line.contains("sampled=false"),
            "an error logs because it is an error, not a sample: {line}"
        );
    }
}

#[tokio::test]
async fn access_event_carries_the_structured_fields() {
    let buf = install_subscriber();
    let app = spawn_app().await;
    let id = fresh_id();
    send(&app, Method::GET, "/nope", &id).await;
    let line = event_for(&buf, &id).expect("404 must emit the access event");
    for field in [
        &format!("request_id={id}"),
        "method=GET",
        "route=unmatched",
        "status=404",
        "elapsed_ms=",
        "sampled=false",
    ] {
        assert!(
            line.contains(field),
            "the access event must carry {field}: {line}"
        );
    }
}

#[tokio::test]
async fn sampled_success_emits_unsampled_does_not() {
    let buf = install_subscriber();
    let app = spawn_app().await;
    let fast = Duration::from_millis(10);
    // Pick ids whose sampling verdict is known (the hash is
    // deterministic per id), so the test asserts the wiring, not
    // the hash.
    let mut sampled = String::new();
    let mut unsampled = String::new();
    while sampled.is_empty() || unsampled.is_empty() {
        let id = fresh_id();
        if should_log(200, fast, &id) {
            if sampled.is_empty() {
                sampled = id;
            }
        } else if unsampled.is_empty() {
            unsampled = id;
        }
    }
    assert!(is_sampled(200, fast), "a fast success is a sampled line");
    send(&app, Method::GET, "/healthz", &sampled).await;
    send(&app, Method::GET, "/healthz", &unsampled).await;
    let line = event_for(&buf, &sampled).expect("the sampled success must emit the access event");
    assert!(
        line.contains("sampled=true"),
        "a sampled success carries sampled=true: {line}"
    );
    assert!(
        event_for(&buf, &unsampled).is_none(),
        "the unsampled success must emit no access event"
    );
}

#[tokio::test]
async fn success_sampling_is_about_one_percent_end_to_end() {
    let buf = install_subscriber();
    let app = spawn_app().await;
    let ids: Vec<String> = (0..5_000).map(|_| fresh_id()).collect();
    for id in &ids {
        send(&app, Method::GET, "/healthz", id).await;
    }
    let lines = event_lines(&buf);
    let emitted = ids
        .iter()
        .filter(|id| {
            lines
                .iter()
                .any(|line| line.contains(&format!("request_id={id}")))
        })
        .count();
    // Mean 50 of 5,000 (σ ≈ 7); the band is ±5σ, so a failure
    // means the sampling is off, not the statistics.
    assert!(
        (10..=150).contains(&emitted),
        "expected ~50 of 5,000 sampled successes, got {emitted}"
    );
}
