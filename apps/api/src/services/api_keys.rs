//! API key creation (P1-033, FR-A-06): `POST /v1/api-keys`.
//!
//! A key is a machine credential: `pf_live_` + 24 random
//! bytes (base64url, 32 chars). The full secret is shown
//! exactly once — in the creation response — and is never
//! stored: only its sha256 (`secret_hash`) lands in the row.
//! There is no endpoint that returns the secret again (the
//! list shows prefix + last4 only, P1-036's revoke target is
//! the row id), so a lost secret means minting a new key.
//!
//! Rules, in order:
//! - The request validates first (name: display-name rules,
//!   1–100 chars, no control chars; scopes: at least one,
//!   each one of `jobs:write` / `jobs:read` /
//!   `webhooks:manage`, no duplicates) — 422 `PF-2002` with
//!   `field_errors`, via `ValidatedJson` in the handler.
//! - Max 10 LIVE keys per user (revoked keys do not count).
//!   The 11th create is 409 `PF-2011` (Conflict — the
//!   request is valid, the user's state refuses it; there is
//!   no dedicated PF code for the cap, and the adding-an-
//!   endpoint rule forbids inventing one without recording it
//!   in the taxonomy, so the existing Conflict carries it).
//!   The count + insert run in one transaction holding the
//!   user's row lock (`FOR UPDATE`), so two concurrent
//!   creates cannot both pass the cap.
//! - Creation + audit (`api_key.created`, `after` carrying
//!   the key id, name, prefix and scopes — never the secret
//!   or its hash) are one transaction.
//!
//! Deliberately NOT here (see the ASSUMPTION entry in
//! `docs/09-STATE/ISSUES.md`): plan entitlement — FR-P-01
//! lists API keys as a Pro/Business feature, but no ticket
//! owns the gate yet, so creation does not check the plan;
//! and email verification — the spec does not require a
//! verified user to mint a key, and the key is unusable for
//! jobs until verified anyway (job-create calls the
//! `require_verified` guard).

use crate::AppState;
use chrono::{DateTime, Utc};
use detag_core::{
    api_keys::{self, API_KEY_MAX_PER_USER},
    db::PoolConfig,
    error::FieldError,
    repos::{
        api_keys::{self as api_keys_repo, NewApiKey},
        audit_logs::{self, NewAuditLog},
        users::User,
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_display_name, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// The POST body: a human label + the scopes the key may use.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    /// Human label, e.g. `"CI deploy"` — shown in listings.
    pub name: String,
    /// At least one of `jobs:write`, `jobs:read`,
    /// `webhooks:manage`; no duplicates.
    pub scopes: Vec<String>,
}

impl Validate for CreateApiKeyRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_display_name("name", &self.name) {
            errors.push(err);
        }
        if self.scopes.is_empty() {
            errors.push(FieldError::new(
                "scopes",
                "required",
                "at least one scope is required",
            ));
        } else {
            let mut seen: Vec<&str> = Vec::with_capacity(self.scopes.len());
            for scope in &self.scopes {
                if !api_keys::is_known_scope(scope) {
                    errors.push(FieldError::new("scopes", "invalid_scope", "unknown scope"));
                } else if seen.contains(&scope.as_str()) {
                    errors.push(FieldError::new(
                        "scopes",
                        "duplicate_scope",
                        "duplicate scope",
                    ));
                } else {
                    seen.push(scope.as_str());
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// The creation response — the ONLY place the full secret ever
/// appears. Everything else about the key (listings, audit)
/// shows the prefix + last4, never this.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreateApiKeyResponse {
    /// The key id — the revoke/rotate target.
    pub id: Uuid,
    /// The label the caller gave.
    pub name: String,
    /// Display identifier: `pf_live_` + first 4 secret chars.
    pub prefix: String,
    /// The full secret (`pf_live_` + 32 chars). Shown once.
    pub secret: String,
    /// The scopes, as stored (request order).
    pub scopes: Vec<String>,
    /// When the key was created.
    pub created_at: DateTime<Utc>,
}

/// What key creation can fail with. `Validation` is produced
/// by the DTO's `Validate` impl (the handler's `ValidatedJson`
/// enforces it before the service runs); `CapReached` is the
/// 10-live-keys cap; `Internal` covers the DB and the user
/// vanishing between auth and the transaction.
#[derive(Debug, Error)]
pub enum ApiKeyError {
    #[error("api key request validation failed")]
    Validation(ValidationErrors),
    #[error("api key limit reached")]
    CapReached,
    #[error("api key creation failed")]
    Internal,
}

/// The transaction's inner error: the cap is a business
/// outcome, not a DB failure, so it rides its own variant
/// out of the closure instead of being logged as an error.
#[derive(Debug, Error)]
enum CreateTxError {
    #[error("db error: {0}")]
    Db(#[from] RepoError),
    #[error("api key limit reached")]
    CapReached,
}

/// Mint a key for `user`: validate, enforce the 10-live-keys
/// cap under the user's row lock, insert the row + audit in
/// one transaction, and return the response carrying the
/// secret — the only copy that will ever exist.
pub async fn create_api_key(
    state: &AppState,
    user: &User,
    actor_user_id: Uuid,
    req: &CreateApiKeyRequest,
) -> Result<CreateApiKeyResponse, ApiKeyError> {
    req.validate().map_err(ApiKeyError::Validation)?;
    let secret = api_keys::generate_secret();
    let prefix = api_keys::display_prefix(&secret);
    let secret_hash = api_keys::hash_secret(&secret);
    let scopes = req.scopes.clone();
    let name = req.name.trim().to_string();
    let user = user.clone();
    let timeout = PoolConfig::default().transaction_timeout;

    let key = tx::with_tx(&state.pool, timeout, |conn| {
        let name = name.clone();
        let prefix = prefix.clone();
        let secret_hash = secret_hash.clone();
        let scopes = scopes.clone();
        let user = user.clone();
        Box::pin(async move {
            api_keys_repo::lock_user_for_key_change(&mut *conn, user.id)
                .await
                .map_err(CreateTxError::Db)?;
            let live = api_keys_repo::count_live_for_user(&mut *conn, user.id)
                .await
                .map_err(CreateTxError::Db)?;
            if live >= i64::from(API_KEY_MAX_PER_USER) {
                return Err(CreateTxError::CapReached);
            }
            let key = api_keys_repo::create(
                &mut *conn,
                &NewApiKey {
                    user_id: user.id,
                    name: name.clone(),
                    prefix: prefix.clone(),
                    secret_hash,
                    scopes: scopes.clone(),
                },
            )
            .await
            .map_err(CreateTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(actor_user_id),
                    actor_client: None,
                    action: "api_key.created".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({
                        "api_key_id": key.id,
                        "name": name,
                        "prefix": prefix,
                        "scopes": scopes,
                    })),
                    reason: None,
                },
            )
            .await
            .map_err(CreateTxError::Db)?;
            Ok(key)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(CreateTxError::CapReached) => ApiKeyError::CapReached,
        other => {
            tracing::error!(error = ?other, "api-keys: create transaction failed");
            ApiKeyError::Internal
        }
    })?;

    Ok(CreateApiKeyResponse {
        id: key.id,
        name: key.name,
        prefix: key.prefix,
        secret,
        scopes: key.scopes,
        created_at: key.created_at,
    })
}
