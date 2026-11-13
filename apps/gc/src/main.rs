//! detag-gc — orphan + retention garbage collector (P0-060, E0.6).
//!
//! The fallback behind the ILM ceiling: lifecycle rules have
//! day granularity, so the per-plan result TTLs (Free 6 h, Pro
//! 24 h, FR-J-09) cannot live in ILM — this job deletes expired
//! artifacts from `artifacts.expires_at`, and deletes orphans
//! (objects no database row references) older than the grace
//! period. Inputs are never deleted by this job (no `expires_at`);
//! the ILM 3-day ceiling is their backstop.
//!
//! Fails closed: the database sets are fetched before anything is
//! listed, so a down database means no run at all (never a run
//! that mistakes every object for an orphan). Dry-run is the
//! default; `--execute` is the only way anything is deleted.
//!
//! Usage:
//!   detag-gc [--dry-run|--execute] [--orphan-grace-hours N]
//!   detag-gc --purge-users [--dry-run|--execute]
//!   detag-gc --purge-sessions [--dry-run|--execute]
//!
//! The `--purge-users` mode (P1-007) is the nightly account
//! purge: hard-delete accounts whose 30-day deletion grace has
//! passed (cascade per the data-model doc). It runs INSTEAD of
//! the artifact/orphan sweep (one mode per invocation — the
//! nightly cron calls it separately). Dry-run lists the
//! candidates and deletes nothing.
//!
//! The `--purge-sessions` mode (P1-015) is the nightly session
//! purge: hard-delete session rows past their `expires_at`
//! (an expired refresh token can never mint an access token
//! again). It runs INSTEAD of the sweep, needs the database
//! only, and dry-runs by default (counts the candidates).
//! One trade-off, recorded in the data-model doc: purging an
//! expired REPLACED row loses the reuse signal for that token
//! (a later present is the generic 401 PF-1001, not PF-1007,
//! and does not revoke the family) — the token is expired, so
//! it cannot mint anything, and the window is bounded by the
//! 30-day TTL.
//!
//! Scheduling (a daily run) lands with the delivery work; today
//! the operator runs it by hand (`just gc` dry-runs it).

use std::collections::HashSet;
use std::process::ExitCode;

use chrono::Duration;
use detag_core::config::Settings;
use detag_core::db::{connect, PoolConfig};
use detag_core::repos::artifacts;
use detag_storage::s3::{S3Config, S3Storage};

mod plan;
mod purge;
mod run;

use purge::{purge_expired_sessions, purge_expired_users};
use run::{run_gc, GcOpts};

fn usage() -> &'static str {
    "usage: detag-gc [--dry-run|--execute] [--orphan-grace-hours N]\n       detag-gc --purge-users [--dry-run|--execute]\n       detag-gc --purge-sessions [--dry-run|--execute]"
}

fn parse_args(args: &[String]) -> Result<GcOpts, String> {
    let mut opts = GcOpts::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => opts.dry_run = true,
            "--execute" => opts.dry_run = false,
            "--orphan-grace-hours" => {
                let value = args
                    .get(i + 1)
                    .ok_or("--orphan-grace-hours needs a value")?;
                let hours: i64 = value
                    .parse()
                    .map_err(|_| "--orphan-grace-hours needs an integer")?;
                if hours < 0 {
                    return Err("--orphan-grace-hours must not be negative".to_string());
                }
                opts.orphan_grace = Duration::hours(hours);
                i += 1;
            }
            // P1-007: purge accounts past the 30-day deletion
            // grace instead of the artifact/orphan sweep. The
            // flag is the mode switch — `--purge-users` without
            // `--execute` dry-runs (lists the candidates).
            "--purge-users" => opts.purge_users = true,
            // P1-015: purge sessions past their expiry instead
            // of the artifact/orphan sweep. Same shape —
            // `--purge-sessions` without `--execute` dry-runs
            // (counts the candidates).
            "--purge-sessions" => opts.purge_sessions = true,
            "--help" | "-h" => {
                println!(
                    "{}\n\nOrphan + retention garbage collector (P0-060). Dry-run by default; --execute deletes.",
                    usage()
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n{}", usage())),
        }
        i += 1;
    }
    Ok(opts)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("detag-gc: {err}");
            return ExitCode::from(2);
        }
    };

    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-gc: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    // Eager connect: a GC that cannot read the database must not
    // run at all (see the module docs).
    let pool = match connect(&settings.database_url, &PoolConfig::default()).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("detag-gc: cannot connect to Postgres: {err}");
            return ExitCode::from(1);
        }
    };
    // P1-007: the purge mode needs the database only — no
    // storage client, no artifact sets. It runs instead of the
    // sweep below.
    if opts.purge_users {
        match purge_expired_users(&pool, chrono::Utc::now(), opts.dry_run).await {
            Ok(report) => {
                println!(
                    "detag-gc: purge-users candidates={} purged={} failures={} mode={}",
                    report.candidates.len(),
                    report.purged.len(),
                    report.failures.len(),
                    if opts.dry_run { "dry-run" } else { "execute" },
                );
                for id in &report.candidates {
                    println!("detag-gc: purge-users candidate {id}");
                }
                if report.failures.is_empty() {
                    return ExitCode::SUCCESS;
                }
                for (id, err) in &report.failures {
                    eprintln!("detag-gc: failed to purge {id}: {err}");
                }
                return ExitCode::from(1);
            }
            Err(err) => {
                eprintln!("detag-gc: purge failed: {err}");
                return ExitCode::from(1);
            }
        }
    }
    // P1-015: the session purge mode needs the database only
    // too — no storage client. It runs instead of the sweep
    // below (one mode per invocation).
    if opts.purge_sessions {
        match purge_expired_sessions(&pool, chrono::Utc::now(), opts.dry_run).await {
            Ok(report) => {
                println!(
                    "detag-gc: purge-sessions candidates={} purged={} mode={}",
                    report.candidates,
                    report.purged,
                    if opts.dry_run { "dry-run" } else { "execute" },
                );
                return ExitCode::SUCCESS;
            }
            Err(err) => {
                eprintln!("detag-gc: purge-sessions failed: {err}");
                return ExitCode::from(1);
            }
        }
    }
    let storage = match S3Storage::new(S3Config {
        endpoint: settings.s3_endpoint.clone(),
        bucket: settings.s3_bucket.clone(),
        region: settings.s3_region.clone(),
        access_key: settings.s3_access_key.clone(),
        secret_key: settings.s3_secret_key.clone(),
    }) {
        Ok(storage) => storage,
        Err(err) => {
            eprintln!("detag-gc: invalid storage configuration: {err}");
            return ExitCode::from(1);
        }
    };

    let now = chrono::Utc::now();
    let expired: HashSet<String> = match artifacts::expired_storage_keys(&pool, now).await {
        Ok(keys) => keys.into_iter().collect(),
        Err(err) => {
            eprintln!("detag-gc: cannot read expired artifacts: {err}");
            return ExitCode::from(1);
        }
    };
    let known: HashSet<String> = match artifacts::all_storage_keys(&pool).await {
        Ok(keys) => keys.into_iter().collect(),
        Err(err) => {
            eprintln!("detag-gc: cannot read known storage keys: {err}");
            return ExitCode::from(1);
        }
    };

    match run_gc(&storage, &expired, &known, &opts).await {
        Ok(report) => {
            println!(
                "detag-gc: scanned={} expired={} orphans={} kept={} mode={}",
                report.scanned,
                report.deleted_expired,
                report.deleted_orphans,
                report.kept,
                if opts.dry_run { "dry-run" } else { "execute" },
            );
            if report.failures.is_empty() {
                ExitCode::SUCCESS
            } else {
                for (key, err) in &report.failures {
                    eprintln!("detag-gc: failed to delete {key}: {err}");
                }
                ExitCode::from(1)
            }
        }
        Err(err) => {
            eprintln!("detag-gc: run failed: {err}");
            ExitCode::from(1)
        }
    }
}
