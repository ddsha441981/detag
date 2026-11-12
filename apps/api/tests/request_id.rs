//! Integration tests for the request-id middleware (P0-019).
//!
//! Drives the real router with `tower::ServiceExt::oneshot` and asserts
//! the `x-request-id` echo plus the tracing span (request_id + route)
//! via a capturing global subscriber installed once for the binary.

use axum::http::{Request, StatusCode};
use detag_api::build_router;
use detag_core::metrics::Metrics;
use std::{
    io::Write,
    sync::{Arc, Mutex, OnceLock},
};
use tower::ServiceExt;
use tracing_subscriber::fmt::format::FmtSpan;
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
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_writer(move || TestWriter(Arc::clone(&writer_buf)))
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("test subscriber installs once");
        buf
    }))
}

fn logs_contain(buf: &Arc<Mutex<Vec<u8>>>, needle: &str) -> bool {
    let logs = buf.lock().unwrap();
    let text = String::from_utf8_lossy(&logs);
    text.contains(needle)
}

fn header_id(res: &axum::http::Response<axum::body::Body>) -> String {
    res.headers()
        .get("x-request-id")
        .expect("x-request-id echo")
        .to_str()
        .expect("header is ascii")
        .to_string()
}

#[tokio::test]
async fn echoes_client_request_id() {
    let buf = install_subscriber();
    let client_id = "123e4567-e89b-42d3-a456-426614174000";
    let res = build_router(Metrics::new())
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("x-request-id", client_id)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(header_id(&res), client_id);
    assert!(logs_contain(&buf, &format!("request_id={client_id}")));
    assert!(logs_contain(&buf, "route=/healthz"));
}

#[tokio::test]
async fn generates_uuidv7_when_missing() {
    let buf = install_subscriber();
    let res = build_router(Metrics::new())
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let id = header_id(&res);
    let uuid = Uuid::parse_str(&id).expect("echo is a UUID");
    assert_eq!(uuid.get_version_num(), 7, "generated ids are UUIDv7");
    assert!(logs_contain(&buf, &format!("request_id={id}")));
    assert!(logs_contain(&buf, "route=/healthz"));
}

#[tokio::test]
async fn replaces_invalid_request_id() {
    let buf = install_subscriber();
    let res = build_router(Metrics::new())
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("x-request-id", "not-a-uuid")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let id = header_id(&res);
    assert_ne!(id, "not-a-uuid");
    let uuid = Uuid::parse_str(&id).expect("echo is a UUID");
    assert_eq!(uuid.get_version_num(), 7);
    assert!(logs_contain(&buf, &format!("request_id={id}")));
}

#[tokio::test]
async fn unmatched_route_gets_unmatched_span() {
    let buf = install_subscriber();
    let res = build_router(Metrics::new())
        .oneshot(
            Request::builder()
                .uri("/nope")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let id = header_id(&res);
    Uuid::parse_str(&id).expect("echo is a UUID");
    assert!(logs_contain(&buf, &format!("request_id={id}")));
    assert!(logs_contain(&buf, "route=unmatched"));
}
