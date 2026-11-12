//! Transaction helper (P0-042, E0.4).
//!
//! Services run multi-statement work through [`with_tx`]: begin,
//! run the closure, commit on success, roll back on error. The
//! closure receives the transaction's connection (`&mut
//! PgConnection`) — never the pool and never the `Transaction`
//! itself, so it cannot commit, roll back, or open a second
//! transaction of its own. The only way to nest is to capture the
//! pool from the outer scope, and that is caught by the nesting
//! guard (a tokio task-local depth counter) which fails with
//! [`TxError::Nested`] instead of opening a second transaction.
//!
//! Why the connection and not the `Transaction`: passing
//! `&mut Transaction<'_, Postgres>` into an async closure does not
//! compile — the type carries two independent lifetimes (the `&mut`
//! borrow and the transaction's own pool borrow) and `&mut` is
//! invariant over the latter, so no closure bound can unify them.
//! `PgConnection` is a concrete type with no lifetime parameter,
//! so `&mut PgConnection` has exactly one lifetime and the bound
//! works. Repositories take `impl Executor`, so call them with a
//! reborrow (`&mut *conn`).
//!
//! Timeout: the caller passes a `Duration` (the default lives in
//! [`PoolConfig::transaction_timeout`], 30 s); it is applied with
//! `SET LOCAL statement_timeout`, so it bounds each statement in
//! the transaction and vanishes with it. `Duration::ZERO` disables
//! it.
//!
//! No service layer exists yet in Phase 0 — services land in Phase
//! 1 and use this helper then. The helper itself is covered by the
//! live tests in `crates/core/tests/tx_live.rs`.

use crate::db::DbPool;
use sqlx::PgConnection;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;

/// What `with_tx` can fail with. `Inner` carries the closure's own
/// error (the transaction was rolled back); the other variants are
/// failures of the transaction machinery itself.
#[derive(Debug, Error)]
pub enum TxError<E> {
    /// A `with_tx` call was attempted inside another `with_tx`
    /// closure in the same task. Nested transactions (savepoints)
    /// are not supported — restructure the caller instead.
    #[error("nested transaction attempted")]
    Nested,

    /// `BEGIN` (or the `SET LOCAL statement_timeout` setup) failed.
    #[error("transaction failed to begin: {0}")]
    Begin(sqlx::Error),

    /// The closure succeeded but `COMMIT` failed. The transaction
    /// is not committed; the caller must assume its writes are lost.
    #[error("transaction failed to commit: {0}")]
    Commit(sqlx::Error),

    /// The closure failed; the transaction was rolled back. (A
    /// rollback failure of its own is ignored — the connection is
    /// discarded either way — and the closure's error is what the
    /// caller needs.)
    #[error("transaction body failed")]
    Inner(E),
}

tokio::task_local! {
    /// Depth of `with_tx` nesting in the current task. `scope`d to 1
    /// by the outer call; any inner call sees 1 and fails Nested.
    /// Task-local, so a `tokio::spawn`ed task starts at 0 — spawned
    /// work is a separate execution, not nesting.
    static TX_DEPTH: std::cell::Cell<u32>;
}

/// Run `f` inside a transaction: commit on `Ok`, roll back on `Err`.
///
/// `f` is a closure returning a boxed future (write it as
/// `|conn| Box::pin(async move { ... })`); it gets `&mut
/// PgConnection` — the transaction's connection — and must use it
/// for every statement, so all of the work commits or rolls back
/// together. Repositories take `impl Executor`: pass a reborrow
/// (`&mut *conn`). `timeout` is applied as `SET LOCAL
/// statement_timeout` (see the module docs).
///
/// The boxed future is load-bearing: a plain `FnOnce -> Fut` bound
/// cannot express a future that borrows its `&mut PgConnection`
/// argument (the compiler cannot tie the future's lifetime to the
/// borrow), and neither can `AsyncFnOnce` here — the reborrow
/// inside the closure creates a second, shorter lifetime that the
/// trait bounds cannot unify. Boxing makes the future's lifetime
/// (`'c`) part of its type, so the higher-ranked bound works.
pub async fn with_tx<T, E, F>(pool: &DbPool, timeout: Duration, f: F) -> Result<T, TxError<E>>
where
    F: for<'c> FnOnce(
        &'c mut PgConnection,
    ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'c>>,
{
    // Fast path for the common case: no task-local set yet (depth 0).
    let nested = TX_DEPTH.try_with(|d| d.get()).unwrap_or(0) > 0;
    if nested {
        return Err(TxError::Nested);
    }

    TX_DEPTH
        .scope(std::cell::Cell::new(1), async move {
            let mut tx = pool.begin().await.map_err(TxError::Begin)?;
            if !timeout.is_zero() {
                let ms = format!("{}ms", timeout.as_millis());
                sqlx::query("SELECT set_config('statement_timeout', $1, true)")
                    .bind(ms)
                    .execute(&mut *tx)
                    .await
                    .map_err(TxError::Begin)?;
            }
            // Bind the outcome first: the closure's future borrows
            // the connection, and a temporary in the `match`
            // scrutinee would live until the end of the match — past
            // the `commit()` / `rollback()` that moves `tx`. The
            // `let` drops the future (and its borrow) before the
            // match arms run.
            let outcome = f(&mut tx).await;
            match outcome {
                Ok(value) => {
                    tx.commit().await.map_err(TxError::Commit)?;
                    Ok(value)
                }
                Err(err) => {
                    // Best effort: a rollback failure means the
                    // connection is already dead; the closure's error
                    // is the one the caller can act on.
                    let _ = tx.rollback().await;
                    Err(TxError::Inner(err))
                }
            }
        })
        .await
}
