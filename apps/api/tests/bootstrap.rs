//! P0-063: the api boots with the full AppState (pool, redis,
//! storage, settings, clock) and answers on a real socket.
//!
//! This is the "server boots" AC: it builds the state from the
//! default settings (lazy pools, config-only storage — no live
//! Postgres/Redis/MinIO needed to construct it), serves the
//! router on an ephemeral loopback port, and GETs /healthz and
//! /readyz. /readyz may answer 200 or 503 depending on whether
//! the dev Postgres/Redis are up at that moment — both prove the
//! server boots and the state is wired; the test asserts the body
//! is a well-formed readiness report either way. (The spawned-
//! binary readyz tests in `readyz.rs` pin the exact statuses.)

use std::time::Duration;

use detag_api::{build_router, AppState};
use detag_core::config::Settings;
use detag_core::metrics::Metrics;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Minimal HTTP GET over a raw socket; returns (status line, body).
async fn http_get(port: u16, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the api");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .expect("write request");
    let mut raw = Vec::new();
    timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("response in time")
        .expect("read response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no header/body split in response: {text}"));
    (
        head.lines().next().unwrap_or("").to_string(),
        body.to_string(),
    )
}

#[tokio::test]
async fn server_boots_with_full_state() {
    let state = AppState::from_settings(&Settings::default()).expect("default settings build");
    let app = build_router(state, Metrics::new());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let port = listener.local_addr().expect("port").port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let (status, body) = http_get(port, "/healthz").await;
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "/healthz must be 200, got {status}: {body}"
    );
    assert_eq!(body, "ok");

    let (status, body) = http_get(port, "/readyz").await;
    assert!(
        status.starts_with("HTTP/1.1 200") || status.starts_with("HTTP/1.1 503"),
        "/readyz must be 200 or 503, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("readyz body is JSON");
    let components = json["components"]
        .as_array()
        .expect("components is an array");
    assert!(
        components.iter().any(|c| c["name"] == "db"),
        "readyz must report the db component: {body}"
    );
    assert!(
        components.iter().any(|c| c["name"] == "redis"),
        "readyz must report the redis component: {body}"
    );
    assert!(
        json["status"] == "ok" || json["status"] == "degraded",
        "readyz status must be ok or degraded: {body}"
    );

    server.abort();
}
