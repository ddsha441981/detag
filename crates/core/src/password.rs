//! detag-core::password — Argon2id hashing (P1-001, FR-A-01).
//!
//! Params come from `Settings` (`PF_ARGON2_*`); the P1-027
//! defaults are m = 64 MiB, t = 3, p = 4 (the ticket spec —
//! stronger than the OWASP first choice m = 19 MiB, t = 2,
//! p = 1 that P1-001 shipped; hashes made under the old
//! defaults are upgraded on login by [`needs_rehash`]).
//! [`hash_password`] returns the PHC string
//! (`$argon2id$v=19$m=65536,t=3,p=4$<salt>$<hash>`) —
//! self-describing, so [`verify_password`] needs no params:
//! they are read from the hash. The salt is random per hash
//! (16 bytes, OS RNG); two hashes of the same password differ.
//! Verification is the argon2 crate's compare (constant-time
//! over the hash output); the `bool` return is `false` for a
//! wrong password and `Err` only for a malformed stored hash —
//! callers must not turn a `false` into a distinct error, or
//! login timing leaks whether the account exists (P1-003).

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, Params, ParamsBuilder,
};
use rand_core::OsRng;
use thiserror::Error;

/// Argon2id cost parameters (memory in KiB, time iterations,
/// parallelism). Mirrors the `PF_ARGON2_*` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Argon2Params {
    /// The P1-027 defaults (also the `Settings` defaults):
    /// m = 64 MiB, t = 3, p = 4.
    pub const fn defaults() -> Self {
        Self {
            m_cost_kib: 65_536,
            t_cost: 3,
            p_cost: 4,
        }
    }

    /// The P1-001 defaults (m = 19 MiB, t = 2, p = 1 — the
    /// OWASP first choice): what every hash made before P1-027
    /// carries. Kept for the rehash tests and for any caller
    /// that must reproduce a pre-P1-027 hash; production code
    /// hashes with [`defaults`] via `from_settings`.
    pub const fn previous_defaults() -> Self {
        Self {
            m_cost_kib: 19_456,
            t_cost: 2,
            p_cost: 1,
        }
    }

    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        Self {
            m_cost_kib: settings.argon2_m_cost_kib,
            t_cost: settings.argon2_t_cost,
            p_cost: settings.argon2_p_cost,
        }
    }

    fn build(&self) -> Result<Params, PasswordError> {
        ParamsBuilder::new()
            .m_cost(self.m_cost_kib)
            .t_cost(self.t_cost)
            .p_cost(self.p_cost)
            .build()
            .map_err(|_| PasswordError::InvalidParams)
    }
}

/// What hashing / verification can fail with. Displays are
/// static: the underlying argon2 errors carry no user data,
/// but a hash failure must never reach a client body verbatim.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PasswordError {
    #[error("invalid argon2 parameters")]
    InvalidParams,
    #[error("password hashing failed")]
    HashFailed,
    #[error("stored password hash is malformed")]
    MalformedHash,
}

/// Hash `password` with Argon2id and `params`. The returned PHC
/// string is what `users.password_hash` stores — never the
/// password, never a raw hash without its params and salt.
pub fn hash_password(password: &str, params: &Argon2Params) -> Result<String, PasswordError> {
    let argon2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        params.build()?,
    );
    let salt = SaltString::generate(&mut OsRng);
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::HashFailed)
}

/// Verify `password` against a stored PHC hash. `Ok(false)` is a
/// wrong password; `Err` is a malformed stored hash (a data
/// problem, not a wrong password — callers map it to 500, never
/// to "wrong password", so a corrupt row cannot be probed).
pub fn verify_password(password: &str, hash: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(hash).map_err(|_| PasswordError::MalformedHash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Does `stored_hash` need upgrading to `current` (P1-027)?
/// `true` when the stored PHC string's algorithm, version, or
/// any of m/t/p differs from the current policy — the caller
/// (login) rehashes the just-verified password with the current
/// params and persists the new hash, so old hashes migrate
/// lazily, one login at a time, with no bulk rewrite. `Err` is
/// a malformed stored hash (same meaning as in
/// `verify_password`); a caller that just verified the same
/// string cannot see it.
pub fn needs_rehash(stored_hash: &str, current: &Argon2Params) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(stored_hash).map_err(|_| PasswordError::MalformedHash)?;
    if parsed.algorithm.as_str() != argon2::Algorithm::Argon2id.as_str() {
        return Ok(true);
    }
    if parsed.version != Some(argon2::Version::V0x13 as u32) {
        return Ok(true);
    }
    let params = Params::try_from(&parsed).map_err(|_| PasswordError::MalformedHash)?;
    Ok(params.m_cost() != current.m_cost_kib
        || params.t_cost() != current.t_cost
        || params.p_cost() != current.p_cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_params() -> Argon2Params {
        // Small costs keep the unit tests fast; the P1-027
        // defaults are exercised by the config tests + the
        // register integration test (real params, one hash).
        Argon2Params {
            m_cost_kib: 4096,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn round_trip_verifies() {
        let hash = hash_password("correct horse battery staple", &fast_params()).unwrap();
        assert!(hash.starts_with("$argon2id$v=19$"), "{hash}");
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
    }

    #[test]
    fn wrong_password_fails_closed() {
        let hash = hash_password("correct horse battery staple", &fast_params()).unwrap();
        assert!(!verify_password("wrong horse battery staple", &hash).unwrap());
        assert!(!verify_password("", &hash).unwrap());
    }

    #[test]
    fn same_password_hashes_differ() {
        let a = hash_password("same-password", &fast_params()).unwrap();
        let b = hash_password("same-password", &fast_params()).unwrap();
        assert_ne!(a, b, "the salt must be random per hash");
        assert!(verify_password("same-password", &a).unwrap());
        assert!(verify_password("same-password", &b).unwrap());
    }

    #[test]
    fn malformed_hash_is_an_error_not_a_false() {
        let err = verify_password("whatever", "not-a-phc-string").unwrap_err();
        assert_eq!(err, PasswordError::MalformedHash);
        let err =
            verify_password("whatever", "$argon2id$v=19$m=4096,t=1,p=1$AAAA$BBBB").unwrap_err();
        assert_eq!(err, PasswordError::MalformedHash);
    }

    #[test]
    fn invalid_params_rejected() {
        // The crate's own floor is m = 8 KiB (Params::MIN_M_COST);
        // the config validation enforces the stricter 4 MiB floor
        // (P1-001) — here we prove the crate rejects below its own.
        let too_little_memory = Argon2Params {
            m_cost_kib: 7,
            t_cost: 1,
            p_cost: 1,
        };
        assert_eq!(
            hash_password("x", &too_little_memory).unwrap_err(),
            PasswordError::InvalidParams
        );
        let zero_time = Argon2Params {
            m_cost_kib: 4096,
            t_cost: 0,
            p_cost: 1,
        };
        assert_eq!(
            hash_password("x", &zero_time).unwrap_err(),
            PasswordError::InvalidParams
        );
    }

    #[test]
    fn defaults_are_the_documented_p1_027_values() {
        let params = Argon2Params::defaults();
        assert_eq!(params.m_cost_kib, 65_536);
        assert_eq!(params.t_cost, 3);
        assert_eq!(params.p_cost, 4);
        // The pre-P1-027 defaults are the OWASP first choice.
        let old = Argon2Params::previous_defaults();
        assert_eq!(old.m_cost_kib, 19_456);
        assert_eq!(old.t_cost, 2);
        assert_eq!(old.p_cost, 1);
    }

    #[test]
    fn needs_rehash_true_for_old_params_false_for_current() {
        let current = Argon2Params::defaults();
        let old_hash = hash_password(
            "correct horse battery staple",
            &Argon2Params::previous_defaults(),
        )
        .unwrap();
        assert!(
            old_hash.contains("m=19456,t=2,p=1"),
            "fixture carries the old params: {old_hash}"
        );
        assert!(needs_rehash(&old_hash, &current).unwrap());
        let new_hash = hash_password("correct horse battery staple", &current).unwrap();
        assert!(
            new_hash.contains("m=65536,t=3,p=4"),
            "fixture carries the new params: {new_hash}"
        );
        assert!(!needs_rehash(&new_hash, &current).unwrap());
    }

    #[test]
    fn needs_rehash_true_when_only_one_param_differs() {
        let current = Argon2Params::defaults();
        // Same memory + time, different parallelism.
        let p_differs = Argon2Params {
            m_cost_kib: 65_536,
            t_cost: 3,
            p_cost: 1,
        };
        let hash = hash_password("correct horse battery staple", &p_differs).unwrap();
        assert!(needs_rehash(&hash, &current).unwrap());
        // Same params but a different algorithm (Argon2i):
        // the PHC string says so, so it needs upgrading.
        let argon2i = Argon2::new(
            argon2::Algorithm::Argon2i,
            argon2::Version::V0x13,
            current.build().unwrap(),
        );
        let salt = SaltString::generate(&mut OsRng);
        let hash_i = argon2i
            .hash_password(b"correct horse battery staple", &salt)
            .unwrap()
            .to_string();
        assert!(hash_i.starts_with("$argon2i$"), "{hash_i}");
        assert!(needs_rehash(&hash_i, &current).unwrap());
    }

    #[test]
    fn needs_rehash_malformed_is_an_error() {
        let err = needs_rehash("not-a-phc-string", &Argon2Params::defaults()).unwrap_err();
        assert_eq!(err, PasswordError::MalformedHash);
    }
}
