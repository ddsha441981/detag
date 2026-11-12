//! Typed repository errors (P0-041, E0.4).
//!
//! Postgres constraint failures surface as typed variants instead of
//! raw `sqlx::Error`, so services can map them to problem+json codes
//! (e.g. duplicate email -> 409) without string-matching. Postgres
//! error codes: 23505 unique, 23514 check, 23503 foreign key,
//! 23502 not-null (left as `Db` — a not-null failure is a bug in
//! the caller, not a client error worth a dedicated variant).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("unique constraint violated: {constraint:?}")]
    UniqueViolation { constraint: Option<String> },

    #[error("check constraint violated: {constraint:?}")]
    CheckViolation { constraint: Option<String> },

    #[error("foreign key violated: {constraint:?}")]
    ForeignKeyViolation { constraint: Option<String> },

    #[error("row not found")]
    NotFound,

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl RepoError {
    /// Classify a sqlx error: Postgres constraint failures become
    /// typed variants (with the constraint name when the server
    /// reports one); everything else passes through as `Db`.
    pub fn from_sqlx(err: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db_err) = &err {
            let code = db_err.code().unwrap_or_default();
            let constraint = db_err.constraint().map(str::to_string);
            return match code.as_ref() {
                "23505" => Self::UniqueViolation { constraint },
                "23514" => Self::CheckViolation { constraint },
                "23503" => Self::ForeignKeyViolation { constraint },
                _ => Self::Db(err),
            };
        }
        Self::Db(err)
    }
}
