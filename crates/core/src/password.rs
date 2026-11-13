//! detag-core::password — Argon2id hashing (P1-001, FR-A-01).
//!
//! Params come from `Settings` (`PF_ARGON2_*`); the OWASP
//! first-choice defaults are m = 19 MiB, t = 2, p = 1.
//! [`hash_password`] returns the PHC string
//! (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`) —
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
    /// The OWASP first-choice defaults (also the `Settings`
    /// defaults): m = 19 MiB, t = 2, p = 1.
    pub const fn owasp_defaults() -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_params() -> Argon2Params {
        // Small costs keep the unit tests fast; the OWASP
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
    fn owasp_defaults_are_the_documented_values() {
        let params = Argon2Params::owasp_defaults();
        assert_eq!(params.m_cost_kib, 19_456);
        assert_eq!(params.t_cost, 2);
        assert_eq!(params.p_cost, 1);
    }
}
