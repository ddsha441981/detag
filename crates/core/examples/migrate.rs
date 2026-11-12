//! Migration CLI (P0-036). Thin wrapper over `detag_core::migrate`;
//! `just migrate` drives it. The `reset` subcommand (P0-043) is
//! `just db::reset`: drop + recreate the local dev database, then
//! migrate + seed. DEV ONLY — it refuses any non-localhost URL.
//!
//!   cargo run -p detag-core --example migrate -- up       # apply pending
//!   cargo run -p detag-core --example migrate -- new <name>
//!   cargo run -p detag-core --example migrate -- revert   # dev only
//!   cargo run -p detag-core --example migrate -- reset    # dev only
//!
//! The migrations dir is `./migrations` (repo root, where `just`
//! runs from), falling back to the workspace-relative path. Errors
//! are printed redacted (P0-032: never log credentials).

use detag_core::config::Settings;
use detag_core::db::{connect, PoolConfig};
use detag_core::migrate::{create_migration, is_local_database, migrate_revert_last, migrate_up};
use detag_core::seed::seed;
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
        Some("reset") => {
            // Dev only: refuse anything that is not localhost.
            // This drops the whole database — there is no undo.
            if !is_local_database(&settings.database_url) {
                eprintln!(
                    "migrate: reset refused — database is not local (dev only): {}",
                    redact(&settings.database_url)
                );
                std::process::exit(2);
            }
            let db_name = database_name(&settings.database_url).unwrap_or_else(|| {
                eprintln!("migrate: reset needs a database name in PF_DATABASE_URL");
                std::process::exit(2);
            });
            // The name is interpolated into DDL, so it must be a
            // bare identifier — anything else (quotes, spaces,
            // semicolons) is refused rather than escaped.
            if !is_bare_identifier(&db_name) {
                eprintln!(
                    "migrate: reset refused — database name is not a bare identifier: {db_name}"
                );
                std::process::exit(2);
            }
            let maintenance_url = maintenance_url(&settings.database_url);
            let admin = connect(&maintenance_url, &PoolConfig::default())
                .await
                .unwrap_or_else(|err| fail(err));
            // `AssertSqlSafe` is the audited escape hatch: the only
            // interpolated value is `db_name`, which
            // `is_bare_identifier` has already restricted to
            // [A-Za-z0-9_] (no quotes, spaces, or semicolons), so
            // no injection is possible through it. DROP/CREATE
            // DATABASE run as two single statements: `raw_sql`
            // wraps its statements in a transaction block, and
            // DROP DATABASE cannot run inside one.
            let drop = format!("DROP DATABASE IF EXISTS {db_name}");
            let create = format!("CREATE DATABASE {db_name}");
            if let Err(err) = sqlx::query(sqlx::AssertSqlSafe(drop.as_str()))
                .execute(&admin)
                .await
            {
                fail(err);
            }
            if let Err(err) = sqlx::query(sqlx::AssertSqlSafe(create.as_str()))
                .execute(&admin)
                .await
            {
                fail(err);
            }
            admin.close().await;
            println!("recreated database {db_name}");
            let pool = connect(&settings.database_url, &PoolConfig::default())
                .await
                .unwrap_or_else(|err| fail(err));
            match migrate_up(&pool, &migrations_dir()).await {
                Ok(applied) => println!("applied migrations: {applied:?}"),
                Err(err) => fail(err),
            }
            match seed(&pool).await {
                Ok(report) => println!(
                    "seeded: {} plans, {} feature flags",
                    report.plans, report.flags
                ),
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
            eprintln!("usage: migrate [up|new <name>|revert|reset] (unknown: {other})");
            std::process::exit(2);
        }
    }
}

/// Database name from a Postgres URL (`/pf` -> `pf`).
fn database_name(database_url: &str) -> Option<String> {
    let url = url::Url::parse(database_url).ok()?;
    let name = url.path().trim_start_matches('/');
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// The same URL with the database replaced by `postgres` (the
/// maintenance DB, always present) — for DROP/CREATE DATABASE.
fn maintenance_url(database_url: &str) -> String {
    let Ok(mut url) = url::Url::parse(database_url) else {
        return database_url.to_string();
    };
    url.set_path("/postgres");
    url.to_string()
}

/// True when `name` is safe to interpolate into DDL unquoted:
/// ASCII letters, digits, underscore, not empty, not starting
/// with a digit.
fn is_bare_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
}
