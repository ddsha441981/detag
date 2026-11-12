//! Migration CLI (P0-036). Thin wrapper over `detag_core::migrate`;
//! `just migrate` drives it.
//!
//!   cargo run -p detag-core --example migrate -- up       # apply pending
//!   cargo run -p detag-core --example migrate -- new <name>
//!   cargo run -p detag-core --example migrate -- revert   # dev only
//!
//! The migrations dir is `./migrations` (repo root, where `just`
//! runs from), falling back to the workspace-relative path. Errors
//! are printed redacted (P0-032: never log credentials).

use detag_core::config::Settings;
use detag_core::db::{connect, PoolConfig};
use detag_core::migrate::{create_migration, is_local_database, migrate_revert_last, migrate_up};
use detag_core::telemetry::redact;
use std::path::PathBuf;

fn migrations_dir() -> PathBuf {
    let cwd = PathBuf::from("migrations");
    if cwd.is_dir() {
        return cwd;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

fn fail(err: impl std::fmt::Display) -> ! {
    eprintln!("migrate: {}", redact(&err.to_string()));
    std::process::exit(1);
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("migrate: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    match args.first().map(String::as_str) {
        Some("new") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: migrate new <name>");
                std::process::exit(2);
            };
            match create_migration(&migrations_dir(), name, chrono::Utc::now()) {
                Ok((up, down)) => {
                    println!("created:\n  {}\n  {}", up.display(), down.display());
                }
                Err(err) => fail(err),
            }
        }
        Some("revert") => {
            // Dev only: refuse anything that is not localhost.
            if !is_local_database(&settings.database_url) {
                eprintln!(
                    "migrate: revert refused — database is not local (dev only): {}",
                    redact(&settings.database_url)
                );
                std::process::exit(2);
            }
            let pool = connect(&settings.database_url, &PoolConfig::default())
                .await
                .unwrap_or_else(|err| fail(err));
            match migrate_revert_last(&pool, &migrations_dir()).await {
                Ok(Some(version)) => println!("reverted migration {version}"),
                Ok(None) => println!("nothing applied — nothing to revert"),
                Err(err) => fail(err),
            }
            pool.close().await;
        }
        Some("up") | None => {
            let pool = connect(&settings.database_url, &PoolConfig::default())
                .await
                .unwrap_or_else(|err| fail(err));
            match migrate_up(&pool, &migrations_dir()).await {
                Ok(applied) if applied.is_empty() => {
                    println!("already up to date");
                }
                Ok(applied) => {
                    println!("applied migrations: {applied:?}");
                }
                Err(err) => fail(err),
            }
            pool.close().await;
        }
        Some(other) => {
            eprintln!("usage: migrate [up|new <name>|revert] (unknown: {other})");
            std::process::exit(2);
        }
    }
}
