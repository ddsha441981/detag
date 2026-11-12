//! `queue-sweep` — the `just queue:requeue-stuck` command (P0-079).
//!
//! Re-queues jobs orphaned by dead workers: jobs whose worker's
//! last keep-alive is older than `--older-than-secs` (default:
//! the visibility timeout, `PF_QUEUE_VISIBILITY_TIMEOUT_SECS`).
//! A live worker's jobs are never touched — only a consumer
//! that stopped heartbeating counts as dead (NFR-21).
//!
//! Usage: `queue-sweep [--queue <name>]... [--older-than-secs N]
//! [--limit N]` — no `--queue` sweeps all four queues.
//! Prints one `queue: N requeued` line per queue and exits 0;
//! a Redis failure exits 1. The Redis URL is never printed.

use detag_core::config::Settings;
use detag_worker::queue_hooks::QUEUES;
use detag_worker::requeue::requeue_stuck;
use std::time::Duration;

const USAGE: &str = "usage: queue-sweep [--queue <name>]... [--older-than-secs N] [--limit N]\n  --queue           queue to sweep (pipeline, cleanup, email, reconciliation); repeatable; default: all four\n  --older-than-secs re-queue jobs whose worker last heartbeated longer ago than this (default: PF_QUEUE_VISIBILITY_TIMEOUT_SECS)\n  --limit           max jobs re-queued per queue per run (default: 100)";

fn usage_error(message: &str) -> ! {
    eprintln!("queue-sweep: {message}\n{USAGE}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("queue-sweep: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    let mut queue_names: Vec<String> = Vec::new();
    let mut older_than = Duration::from_secs(settings.queue_visibility_timeout_secs);
    let mut limit: i32 = 100;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "--queue" => {
                let name = args.next().unwrap_or_else(|| {
                    usage_error("--queue needs a queue name");
                });
                if !QUEUES.contains(&name.as_str()) {
                    usage_error(&format!(
                        "unknown queue {name:?} (known: {})",
                        QUEUES.join(", ")
                    ));
                }
                queue_names.push(name);
            }
            "--older-than-secs" => {
                let value = args.next().unwrap_or_else(|| {
                    usage_error("--older-than-secs needs a number of seconds");
                });
                older_than = Duration::from_secs(value.parse().unwrap_or_else(|_| {
                    usage_error(&format!(
                        "--older-than-secs needs a number of seconds, got {value:?}"
                    ));
                }));
            }
            "--limit" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage_error("--limit needs a number"));
                limit = value.parse().unwrap_or_else(|_| {
                    usage_error(&format!("--limit needs a number, got {value:?}"));
                });
                if limit < 1 {
                    usage_error("--limit must be >= 1");
                }
            }
            other => usage_error(&format!("unknown argument {other:?}")),
        }
    }
    let queues: Vec<&str> = if queue_names.is_empty() {
        QUEUES.to_vec()
    } else {
        queue_names.iter().map(String::as_str).collect()
    };
    match requeue_stuck(
        &settings.redis_url,
        &queues,
        detag_worker::workers::QUEUE_NAMESPACE_PREFIX,
        older_than,
        limit,
    )
    .await
    {
        Ok(results) => {
            let mut total = 0;
            for (queue, requeued) in &results {
                println!("{queue}: {requeued} requeued");
                total += requeued;
            }
            println!("total: {total} requeued ({} queues swept)", results.len());
        }
        Err(err) => {
            eprintln!("queue-sweep: {err}");
            std::process::exit(1);
        }
    }
}
