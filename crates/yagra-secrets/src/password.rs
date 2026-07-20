// SPDX-License-Identifier: AGPL-3.0-only
//! Local-auth password hashing (Argon2id).
//!
//! User login passwords are hashed with Argon2id and a per-password random salt, stored as
//! a PHC string. This is distinct from credential encryption ([`crate`] envelope): passwords
//! are *verified*, never decrypted. Never log a password or its hash.

use crate::SecretError;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

/// Hash a password into a PHC string (includes algorithm, salt, and parameters).
pub fn hash_password(password: &str) -> Result<String, SecretError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| SecretError::Crypto)
}

/// Verify a password against a stored PHC hash. Returns `Ok(false)` on mismatch and an
/// error only if the stored hash is malformed.
pub fn verify_password(password: &str, phc: &str) -> Result<bool, SecretError> {
    let parsed = PasswordHash::new(phc).map_err(|_| SecretError::Crypto)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correct_password_verifies() {
        let phc = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &phc).unwrap());
    }

    #[test]
    fn wrong_password_rejected() {
        let phc = hash_password("s3cr3t").unwrap();
        assert!(!verify_password("guess", &phc).unwrap());
    }

    #[test]
    fn same_password_hashes_differently_each_time() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b); // random salt
        assert!(verify_password("same", &a).unwrap());
        assert!(verify_password("same", &b).unwrap());
    }

    #[test]
    fn malformed_hash_is_an_error() {
        assert!(verify_password("x", "not-a-phc-string").is_err());
    }
}
