//! Live tests for the P0-050 SSE pubsub channel.
//!
//! Against the dev Redis (native redis-server on this VM, compose
//! redis where docker runs — PF_REDIS_URL, dev default
//! `redis://localhost:6379`). Every test uses fresh job ids, so
//! parallel runs never share a channel.

use detag_core::redis::pubsub::{publish, subscribe, JobEvent, SUBSCRIBER_BUFFER};
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn pool() -> RedisPool {
    create_pool(&redis_url(), &RedisPoolConfig::default()).expect("pool builds")
}

fn job_id() -> Uuid {
    Uuid::now_v7()
}

#[tokio::test]
async fn roundtrip_and_channel_isolation() {
    let pool = pool();
    let job_a = job_id();
    let job_b = job_id();
    let mut stream = subscribe(&redis_url(), &job_a, SUBSCRIBER_BUFFER)
        .await
        .expect("subscribe to job A");

    // An event for job B reaches nobody (and does not leak into A's stream).
    let receivers_b = publish(
        &pool,
        &JobEvent::Stage {
            job_id: job_b,
            stage: "detect".to_string(),
            progress: 0.1,
        },
    )
    .await
    .expect("publish to B");
    assert_eq!(receivers_b, 0, "nobody listens to job B");

    let stage = JobEvent::Stage {
        job_id: job_a,
        stage: "detect".to_string(),
        progress: 0.34,
    };
    let done = JobEvent::Done {
        job_id: job_a,
        status: "succeeded".to_string(),
        result_url: format!("/v1/jobs/{job_a}/result"),
        gate: "pass".to_string(),
    };
    assert_eq!(publish(&pool, &stage).await.expect("publish stage"), 1);
    assert_eq!(publish(&pool, &done).await.expect("publish done"), 1);

    let first = timeout(Duration::from_secs(5), stream.next_event())
        .await
        .expect("first event arrives in time")
        .expect("stream stays open");
    assert_eq!(first, stage);
    let second = timeout(Duration::from_secs(5), stream.next_event())
        .await
        .expect("second event arrives in time")
        .expect("stream stays open");
    assert_eq!(second, done);

    // Nothing else arrives (job B's event did not leak).
    assert!(
        timeout(Duration::from_millis(200), stream.next_event())
            .await
            .is_err(),
        "no stray event may arrive"
    );
}

#[tokio::test]
async fn latency_p95_is_well_under_500ms() {
    let pool = pool();
    let job = job_id();
    // Buffer bigger than the burst: this test measures latency,
    // not drops — the slow-consumer test covers drops.
    let mut stream = subscribe(&redis_url(), &job, 128).await.expect("subscribe");

    const N: usize = 100;
    let mut sent_at: Vec<Instant> = Vec::with_capacity(N);
    for i in 0..N {
        let event = JobEvent::Progress {
            job_id: job,
            progress: i as f32 / N as f32,
        };
        sent_at.push(Instant::now());
        publish(&pool, &event).await.expect("publish");
    }

    let mut latencies: Vec<Duration> = Vec::with_capacity(N);
    for (i, t0) in sent_at.iter().enumerate() {
        let event = timeout(Duration::from_secs(10), stream.next_event())
            .await
            .expect("event arrives in time")
            .expect("stream stays open");
        latencies.push(Instant::now().duration_since(*t0));
        assert_eq!(
            event,
            JobEvent::Progress {
                job_id: job,
                progress: i as f32 / N as f32,
            },
            "events arrive in publish order"
        );
    }
    latencies.sort_unstable();
    // p95: the 95th percentile sample (index 94 of 100, 0-based).
    let p95 = latencies[(N * 95 / 100) - 1];
    assert!(
        p95 <= Duration::from_millis(500),
        "p95 latency must be <= 500 ms, got {p95:?} (max {:?})",
        latencies[N - 1]
    );
}

#[tokio::test]
async fn slow_consumer_drops_safely() {
    let pool = pool();
    let job = job_id();
    // Buffer of 1: the burst below must overflow it.
    let mut stream = subscribe(&redis_url(), &job, 1).await.expect("subscribe");

    const BURST: usize = 30;
    for i in 0..BURST {
        let receivers = publish(
            &pool,
            &JobEvent::Progress {
                job_id: job,
                progress: i as f32 / BURST as f32,
            },
        )
        .await
        .expect("publish");
        assert_eq!(receivers, 1, "the subscriber must be listening");
    }
    // Nothing was polled during the burst: the buffer held the
    // first event and the pump dropped the rest.
    assert!(
        stream.dropped_count() >= 20,
        "most of the burst must be dropped, got {}",
        stream.dropped_count()
    );
    let first = timeout(Duration::from_secs(5), stream.next_event())
        .await
        .expect("first event arrives in time")
        .expect("stream stays open");
    assert_eq!(
        first,
        JobEvent::Progress {
            job_id: job,
            progress: 0.0,
        },
        "the buffered event is the first one (drop-tail, FIFO)"
    );

    // The stream is still alive after the drop: a fresh event arrives.
    let fresh = JobEvent::Done {
        job_id: job,
        status: "succeeded".to_string(),
        result_url: format!("/v1/jobs/{job}/result"),
        gate: "pass".to_string(),
    };
    publish(&pool, &fresh).await.expect("publish fresh");
    let got = timeout(Duration::from_secs(5), stream.next_event())
        .await
        .expect("fresh event arrives in time")
        .expect("stream stays open");
    assert_eq!(got, fresh);
}
