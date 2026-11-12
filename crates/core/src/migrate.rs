//! detag-core::migrate — sqlx migration tooling (P0-036, E0.4).
//!
//! The runner is embedded (sqlx's `Migrator`), not the sqlx-cli: no
//! extra install, works offline, same files (`migrations/`). The thin
//! CLI is `crates/core/examples/migrate.rs`; `just migrate` drives
//! it. CI runs `just migrate` against an empty database.
//!
//! Conventions: new migrations are reversible pairs
//! (`<version>_<name>.up.sql` + `.down.sql`), versioned by UTC
//! timestamp (`YYYYMMDDHHMMSS`, the sqlx-cli format). Only
//! reversible migrations can be reverted; the pre-existing
//! `0001_feature_flags.sql` (P0-022) is up-only and is never
//! reverted.
//! - `migrate_up` is idempotent: a second run applies nothing.
//! - `migrate_revert_last` is DEV ONLY: the caller must check
//!   `is_local_database` first (the CLI does) — revert refuses
//!   anything that is not localhost.

use crate::db::DbPool;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

/// Errors from the migration tooling.
#[derive(Debug, thiserror::Error)]
pub enum MigrateToolError {
    #[error("migration failed: {0}")]
    Sqlx(#[from] sqlx::migrate::MigrateError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration {0} is not reversible (up-only file, no .down.sql); refusing to revert")]
    NotReversible(i64),
    #[error(
        "migration {0} is applied but its files are missing from the migrations dir; cannot revert"
    )]
    MissingFiles(i64),
    #[error("revert refused: database {0} is not local (dev only)")]
    NotLocal(String),
    #[error("invalid migration name {0:?}: use letters, digits, spaces, hyphens and underscores")]
    BadName(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Apply all pending migrations from `dir`. Returns the versions
/// applied by this run (empty when already up to date). Idempotent.
pub async fn migrate_up(pool: &DbPool, dir: &Path) -> Result<Vec<i64>, MigrateToolError> {
    let migrator = sqlx::migrate::Migrator::new(dir).await?;
    let before = applied_versions(pool).await?;
    migrator.run(pool).await?;
    let after = applied_versions(pool).await?;
    Ok(after.into_iter().filter(|v| !before.contains(v)).collect())
}

/// Revert the most recently applied migration. Returns the reverted
/// version, or `None` when nothing is applied. Fails with
/// [`MigrateToolError::NotReversible`] when the last migration is
/// up-only (no down step). Callers must check
/// [`is_local_database`] first — this function does not.
pub async fn migrate_revert_last(
    pool: &DbPool,
    dir: &Path,
) -> Result<Option<i64>, MigrateToolError> {
    let migrator = sqlx::migrate::Migrator::new(dir).await?;
    let applied = applied_versions(pool).await?;
    let Some(&last) = applied.iter().max() else {
        return Ok(None);
    };
    let has_files = migrator.migrations.iter().any(|m| m.version == last);
    if !has_files {
        return Err(MigrateToolError::MissingFiles(last));
    }
    // A reversible migration is a pair: a ReversibleUp entry and a
    // ReversibleDown entry with the same version. A simple up-only
    // migration has no down entry; `undo` would silently do nothing
    // for it, so refuse loudly instead.
    let has_down = migrator
        .migrations
        .iter()
        .any(|m| m.version == last && m.migration_type.is_down_migration());
    if !has_down {
        return Err(MigrateToolError::NotReversible(last));
    }
    let target = applied
        .iter()
        .filter(|&&v| v < last)
        .max()
        .copied()
        .unwrap_or(0);
    migrator.undo(pool, target).await?;
    Ok(Some(last))
}

/// Versions recorded in `_sqlx_migrations`, ascending. A database
/// that never ran a migration has no table yet — that is "nothing
/// applied", not an error.
pub async fn applied_versions(pool: &DbPool) -> Result<Vec<i64>, sqlx::Error> {
    let rows =
        sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await;
    match rows {
        Ok(versions) => Ok(versions),
        Err(sqlx::Error::Database(db_err)) if db_err.code().as_deref() == Some("42P01") => {
            Ok(Vec::new())
        }
        Err(err) => Err(err),
    }
}

/// True when the database URL points at localhost (hostname or IP).
/// The revert guard: production databases are never localhost.
pub fn is_local_database(database_url: &str) -> bool {
    use std::str::FromStr;
    let Ok(options) = sqlx::postgres::PgConnectOptions::from_str(database_url) else {
        return false;
    };
    // IPv6 localhost may come back with or without brackets depending
    // on how the URL was written.
    matches!(
        options.get_host(),
        "localhost" | "127.0.0.1" | "::1" | "[::1]"
    )
}

/// New-migration version: UTC timestamp, `YYYYMMDDHHMMSS` (the
/// sqlx-cli format).
pub fn new_migration_version(now: DateTime<Utc>) -> i64 {
    now.format("%Y%m%d%H%M%S")
        .to_string()
        .parse()
        .expect("UTC timestamp formats as digits")
}

/// Normalize a migration name: lowercase, spaces/hyphens become
/// underscores, anything else is rejected.
pub fn sanitize_name(name: &str) -> Result<String, MigrateToolError> {
    let cleaned: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '-' { '_' } else { c })
        .collect();
    if cleaned.is_empty()
        || !cleaned
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(MigrateToolError::BadName(name.to_string()));
    }
    Ok(cleaned)
}

/// Create a reversible migration pair in `dir` and return the
/// `(up, down)` paths. Fails when the version already exists (two
/// migrations created in the same second — retry a second later).
pub fn create_migration(
    dir: &Path,
    name: &str,
    now: DateTime<Utc>,
) -> Result<(PathBuf, PathBuf), MigrateToolError> {
    let name = sanitize_name(name)?;
    let version = new_migration_version(now);
    std::fs::create_dir_all(dir)?;
    // The version alone must be unique: two migrations created in
    // the same second (different names) would otherwise share a
    // version and break ordering.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(file_name) = entry.file_name().to_str() {
                if file_name.starts_with(&format!("{version}_")) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "migration version {version} already exists in {} — retry a second later",
                            dir.display()
                        ),
                    )
                    .into());
                }
            }
        }
    }
    let up = dir.join(format!("{version}_{name}.up.sql"));
    let down = dir.join(format!("{version}_{name}.down.sql"));
    std::fs::write(&up, format!("-- {name} ({version})\n-- Up migration.\n\n"))?;
    std::fs::write(
        &down,
        format!("-- {name} ({version})\n-- Down migration: exactly reverses the up migration.\n\n"),
    )?;
    Ok((up, down))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn version_is_utc_timestamp() {
        let now = Utc.with_ymd_and_hms(2026, 11, 12, 6, 30, 0).unwrap();
        assert_eq!(new_migration_version(now), 20261112063000);
    }

    #[test]
    fn sanitize_name_rules() {
        assert_eq!(sanitize_name("Add Users Table").unwrap(), "add_users_table");
        assert_eq!(sanitize_name("billing-plans").unwrap(), "billing_plans");
        assert_eq!(sanitize_name("v2__cleanup").unwrap(), "v2__cleanup");
        assert!(sanitize_name("").is_err());
        assert!(sanitize_name("has/slash").is_err());
        assert!(sanitize_name("has.dot").is_err());
        assert!(sanitize_name("ümlaut").is_err());
    }

    #[test]
    fn local_database_guard() {
        assert!(is_local_database("postgres://pf:pf@localhost:5432/pf"));
        assert!(is_local_database("postgres://pf:pf@127.0.0.1:5432/pf"));
        assert!(is_local_database("postgres://pf:pf@[::1]:5432/pf"));
        assert!(!is_local_database("postgres://pf:pf@db.internal:5432/pf"));
        assert!(!is_local_database("not a url"));
    }

    #[test]
    fn create_migration_writes_a_reversible_pair() {
        let dir = std::env::temp_dir().join(format!("detag-migrate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = Utc.with_ymd_and_hms(2026, 11, 12, 6, 30, 0).unwrap();
        let (up, down) = create_migration(&dir, "Add Widget", now).unwrap();
        assert_eq!(
            up.file_name().unwrap().to_str().unwrap(),
            "20261112063000_add_widget.up.sql"
        );
        assert_eq!(
            down.file_name().unwrap().to_str().unwrap(),
            "20261112063000_add_widget.down.sql"
        );
        assert!(std::fs::read_to_string(&up)
            .unwrap()
            .contains("Up migration"));
        assert!(std::fs::read_to_string(&down)
            .unwrap()
            .contains("Down migration"));
        // Same version twice: the second create fails (already exists).
        assert!(create_migration(&dir, "Other Name", now).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
