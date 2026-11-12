//! `smoke-ping` — the smoke job probe (P0-082).
//!
//! Enqueues one `ping` envelope on the `pipeline` queue and
//! asserts the worker processes it: the api answers `/healthz`,
//! the ping is pushed to Redis, and the worker's `/metrics`
//! scrape shows the dequeue count rise and the in-flight gauge
//! return to 0. Prints `smoke ping done (job_id=...)` and exits
//! 0; anything else exits 1 with the observed state on stderr.
//!
//! Usage: `smoke-ping [--timeout-secs N]` (default 30).
//! Settings come from the `PF_*` env (Settings::load):
//! PF_REDIS_URL, PF_WORKER_QUEUE_NAMESPACE (default `pf:queue`),
//! PF_WORKER_METRICS_PORT (default 9092), PF_API_PORT (default
//! 8080, for the `/healthz` check).
//!
//! What this proves — and what it does not: the probe proves the
//! redis → worker leg end to end (enqueue → dequeue → handled)
//! plus the api being up. It does NOT prove the api enqueues:
//! in Phase 0 the api has no enqueue path (`POST /v1/jobs` is a
//! 501 stub; job creation lands in Phase 1), so the probe
//! enqueues directly. `just smoke` (P0-090) calls this bin
//! against the compose stack; the api-enqueues leg is proven
//! when job creation lands.

use apalis::prelude::*;
use apalis_redis::RedisStorage;
use detag_core::config::Settings;
use detag_worker::envelope::JobEnvelope;
use detag_worker::workers::{apalis_config, WorkerConfig};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

const USAGE: &str = "usage: smoke-ping [--timeout-secs N]\n  --timeout-secs  give up after this many seconds waiting for the ping to be processed (default: 30)";
const QUEUE: &str = "pipeline";

fn usage_error(message: &str) -> ! {
    eprintln!("smoke-ping: {message}\n{USAGE}");
    std::process::exit(2);
}

/// Minimal HTTP GET over a raw socket; returns (status, body).
async fn http_get(host: &str, port: u16, path: &str) -> std::io::Result<(u16, String)> {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect((host, port)))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timed out"))??;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or_default();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    Ok((status, body.to_string()))
}

/// The value of a gauge/counter line `name{labels} value` (or
/// bare `name value`) in a Prometheus scrape; `None` when the
/// series is absent (a counter that never fired has no line).
/// `label` filters the line (e.g. `queue="pipeline"`): the
/// dequeue counter has one line per queue, and the wrong queue's
/// line would prove nothing.
fn scrape_value(scrape: &str, name: &str, label: Option<&str>) -> Option<f64> {
    for line in scrape.lines() {
        if line.starts_with('#') {
            continue;
        }
        let (metric, value) = line.rsplit_once(' ')?;
        let metric_name = metric.split('{').next()?;
        if metric_name != name {
            continue;
        }
        if let Some(label) = label {
            if !metric.contains(label) {
                continue;
            }
        }
        return value.parse().ok();
    }
    None
}

#[tokio::main]
async fn main() {
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("smoke-ping: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    let mut timeout = Duration::from_secs(30);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "--timeout-secs" => {
                let value = args.next().unwrap_or_else(|| {
                    usage_error("--timeout-secs needs a number of seconds");
                });
                let secs: u64 = value.parse().unwrap_or_else(|_| {
                    usage_error(&format!(
                        "--timeout-secs needs a number of seconds, got {value:?}"
                    ))
                });
                if secs == 0 {
                    usage_error("--timeout-secs must be >= 1");
                }
                timeout = Duration::from_secs(secs);
            }
            other => usage_error(&format!("unknown argument {other:?}")),
        }
    }

    if let Err(err) = run(&settings, timeout).await {
        eprintln!("smoke-ping: FAILED: {err}");
        std::process::exit(1);
    }
}

async fn run(settings: &Settings, timeout: Duration) -> Result<(), String> {
    // 1. The api is up (liveness only — /readyz needs MinIO and
    // models, which a smoke run must not require).
    let (status, _) = http_get("127.0.0.1", settings.api_port, "/healthz")
        .await
        .map_err(|err| {
            format!(
                "api /healthz unreachable at 127.0.0.1:{}: {err}",
                settings.api_port
            )
        })?;
    if status != 200 {
        return Err(format!(
            "api /healthz is {status}, not 200 (api on 127.0.0.1:{})",
            settings.api_port
        ));
    }

    // 2. Baseline: the dequeue counter before our ping.
    let metrics_port = settings.worker_metrics_port;
    let scrape = || async move {
        http_get("127.0.0.1", metrics_port, "/metrics")
            .await
            .map(|(_, body)| body)
    };
    let baseline_scrape = scrape()
        .await
        .map_err(|err| format!("worker /metrics unreachable at 127.0.0.1:{metrics_port}: {err}"))?;
    let baseline = scrape_value(
        &baseline_scrape,
        "detag_queue_dequeued_total",
        Some("queue=\"pipeline\""),
    )
    .unwrap_or(0.0);

    // 3. Enqueue one ping.
    let config = WorkerConfig::from_settings(settings);
    let conn = apalis_redis::connect(config.redis_url.as_str())
        .await
        .map_err(|err| format!("Redis unreachable at {}: {err}", config.redis_url))?;
    let mut storage = RedisStorage::new_with_config(conn, apalis_config(&config, QUEUE));
    let job_id = Uuid::now_v7();
    storage
        .push(JobEnvelope::new(
            job_id,
            "ping",
            serde_json::json!({"probe": true}),
        ))
        .await
        .map_err(|err| format!("ping enqueue failed: {err}"))?;

    // 4. Wait until the dequeue count rose AND nothing is in
    // flight: fetched and finished, not merely fetched.
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let body = scrape()
            .await
            .map_err(|err| format!("worker /metrics unreachable while waiting: {err}"))?;
        let dequeued = scrape_value(
            &body,
            "detag_queue_dequeued_total",
            Some("queue=\"pipeline\""),
        )
        .unwrap_or(0.0);
        let inflight = scrape_value(&body, "detag_worker_inflight_jobs", None).unwrap_or(0.0);
        if dequeued >= baseline + 1.0 && inflight == 0.0 {
            println!("smoke ping done (job_id={job_id} queue={QUEUE})");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "ping {job_id} not processed within {} s (dequeued {dequeued}, baseline {baseline}, in-flight {inflight})",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
