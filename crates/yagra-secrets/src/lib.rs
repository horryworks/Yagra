// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-secrets — envelope encryption for monitoring credentials at rest (ADR-018).
//!
//! Each secret is encrypted with a fresh per-secret **DEK** (AES-256-GCM); the DEK is then
//! **wrapped** by the **KEK** (master key). The database stores only the ciphertext and the
//! wrapped DEK — the KEK lives outside the DB, loaded from a mounted secret file via a
//! [`KeyProvider`] (never an env var). Wrapped DEKs carry a `key_id` so KEKs can be rotated
//! by re-wrapping. Only core holds the KEK; pollers never decrypt (ADR-020).

pub mod password;

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Errors from secret sealing/opening.
#[derive(Debug, Error)]
pub enum SecretError {
    /// AEAD encrypt/decrypt failed (bad key, tampered ciphertext, wrong nonce).
    #[error("cryptographic operation failed")]
    Crypto,
    /// No KEK is available for the referenced `key_id` (e.g. a retired generation).
    #[error("no key for key_id {0}")]
    KeyNotFound(u32),
    /// The KEK material was the wrong size (must be 32 bytes for AES-256).
    #[error("invalid key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    /// Failure reading a KEK from its source.
    #[error("key load failed: {0}")]
    KeyLoad(String),
}

/// Supplies KEK material by generation id. The active generation is used for new seals;
/// older generations are kept available so existing secrets stay decryptable during rotation.
pub trait KeyProvider: Send + Sync {
    /// The KEK bytes for a generation, if known.
    fn kek(&self, key_id: u32) -> Option<[u8; 32]>;
    /// The generation new secrets should be sealed with.
    fn active_key_id(&self) -> u32;
}

/// Lets one loaded provider be shared by every envelope-encrypted store behind an `Arc`, instead of
/// each store loading the KEK for itself. That is not just tidier: when the load falls back to an
/// ephemeral key, per-store loading produces a *different* key per store, so two stores could not
/// open each other's seals — which the shared-KEK doc comment claimed they could.
impl<T: KeyProvider + ?Sized> KeyProvider for std::sync::Arc<T> {
    fn kek(&self, key_id: u32) -> Option<[u8; 32]> {
        (**self).kek(key_id)
    }
    fn active_key_id(&self) -> u32 {
        (**self).active_key_id()
    }
}

/// An in-memory key provider (tests, and the default until a file is mounted).
pub struct StaticKeyProvider {
    keys: HashMap<u32, [u8; 32]>,
    active: u32,
}

impl StaticKeyProvider {
    /// A provider with a single active KEK at generation 1.
    #[must_use]
    pub fn single(kek: [u8; 32]) -> Self {
        let mut keys = HashMap::new();
        keys.insert(1, kek);
        Self { keys, active: 1 }
    }

    /// Add a KEK generation (used to model rotation).
    pub fn add_generation(&mut self, key_id: u32, kek: [u8; 32]) {
        self.keys.insert(key_id, kek);
    }

    /// Set the active generation for new seals.
    pub fn set_active(&mut self, key_id: u32) {
        self.active = key_id;
    }
}

impl KeyProvider for StaticKeyProvider {
    fn kek(&self, key_id: u32) -> Option<[u8; 32]> {
        self.keys.get(&key_id).copied()
    }
    fn active_key_id(&self) -> u32 {
        self.active
    }
}

/// Load the KEK from a mounted secret file (ADR-018). The file must contain exactly 32
/// raw bytes. Sealed at generation 1.
pub fn key_provider_from_file(path: &std::path::Path) -> Result<StaticKeyProvider, SecretError> {
    let bytes = std::fs::read(path).map_err(|e| SecretError::KeyLoad(e.to_string()))?;
    let kek: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| SecretError::InvalidKeyLength(bytes.len()))?;
    Ok(StaticKeyProvider::single(kek))
}

/// A sealed secret as stored in the database: ciphertext plus its wrapped DEK and nonces.
/// Carries no plaintext and no KEK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSecret {
    /// KEK generation that wrapped the DEK (for rotation).
    pub key_id: u32,
    /// DEK encrypted under the KEK.
    pub wrapped_dek: Vec<u8>,
    /// Nonce used to wrap the DEK.
    pub dek_nonce: Vec<u8>,
    /// Secret encrypted under the DEK.
    pub ciphertext: Vec<u8>,
    /// Nonce used to encrypt the secret.
    pub ct_nonce: Vec<u8>,
}

/// Seals and opens secrets using envelope encryption over a [`KeyProvider`].
pub struct EnvelopeCipher<K: KeyProvider> {
    keys: K,
}

impl<K: KeyProvider> EnvelopeCipher<K> {
    /// New cipher over the given key provider.
    pub fn new(keys: K) -> Self {
        Self { keys }
    }

    /// Encrypt `plaintext` into a [`SealedSecret`] using the active KEK generation.
    pub fn seal(&self, plaintext: &[u8]) -> Result<SealedSecret, SecretError> {
        let key_id = self.keys.active_key_id();
        let kek = self
            .keys
            .kek(key_id)
            .ok_or(SecretError::KeyNotFound(key_id))?;

        // Fresh per-secret DEK.
        let dek = Aes256Gcm::generate_key(&mut OsRng);
        let dek_cipher = Aes256Gcm::new(&dek);
        let ct_nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = dek_cipher
            .encrypt(&ct_nonce, plaintext)
            .map_err(|_| SecretError::Crypto)?;

        // Wrap the DEK under the KEK.
        let kek_cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&kek));
        let dek_nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let wrapped_dek = kek_cipher
            .encrypt(&dek_nonce, dek.as_slice())
            .map_err(|_| SecretError::Crypto)?;

        Ok(SealedSecret {
            key_id,
            wrapped_dek,
            dek_nonce: dek_nonce.to_vec(),
            ciphertext,
            ct_nonce: ct_nonce.to_vec(),
        })
    }

    /// Decrypt a [`SealedSecret`] back to plaintext.
    pub fn open(&self, sealed: &SealedSecret) -> Result<Vec<u8>, SecretError> {
        let kek = self
            .keys
            .kek(sealed.key_id)
            .ok_or(SecretError::KeyNotFound(sealed.key_id))?;

        // Unwrap the DEK.
        let kek_cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&kek));
        let dek_bytes = kek_cipher
            .decrypt(
                Nonce::from_slice(&sealed.dek_nonce),
                sealed.wrapped_dek.as_slice(),
            )
            .map_err(|_| SecretError::Crypto)?;

        // Decrypt the secret.
        let dek_cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&dek_bytes));
        dek_cipher
            .decrypt(
                Nonce::from_slice(&sealed.ct_nonce),
                sealed.ciphertext.as_slice(),
            )
            .map_err(|_| SecretError::Crypto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kek(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn round_trip_recovers_plaintext() {
        let cipher = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x11)));
        let secret = b"public-community-v2c";
        let sealed = cipher.seal(secret).unwrap();
        assert_eq!(cipher.open(&sealed).unwrap(), secret);
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let cipher = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x22)));
        let secret = b"super-secret-password";
        let sealed = cipher.seal(secret).unwrap();
        assert_ne!(sealed.ciphertext.as_slice(), secret.as_slice());
        // Two seals of the same plaintext differ (fresh DEK + nonce each time).
        let sealed2 = cipher.seal(secret).unwrap();
        assert_ne!(sealed.ciphertext, sealed2.ciphertext);
    }

    #[test]
    fn wrong_kek_cannot_open() {
        let good = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x33)));
        let sealed = good.seal(b"snmpv3-priv-pass").unwrap();

        let bad = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x44)));
        assert!(matches!(bad.open(&sealed), Err(SecretError::Crypto)));
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let cipher = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x55)));
        let mut sealed = cipher.seal(b"api-token").unwrap();
        sealed.ciphertext[0] ^= 0xff; // flip a bit
        assert!(matches!(cipher.open(&sealed), Err(SecretError::Crypto)));
    }

    #[test]
    fn rotation_keeps_old_generation_decryptable() {
        // Seal under gen 1, then rotate active to gen 2 keeping gen 1 available.
        let cipher_v1 = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x01)));
        let sealed = cipher_v1.seal(b"old-secret").unwrap();
        assert_eq!(sealed.key_id, 1);

        let mut provider = StaticKeyProvider::single(kek(0x01));
        provider.add_generation(2, kek(0x02));
        provider.set_active(2);
        let cipher = EnvelopeCipher::new(provider);

        // New seals use gen 2...
        assert_eq!(cipher.seal(b"new").unwrap().key_id, 2);
        // ...but the gen-1 secret still opens.
        assert_eq!(cipher.open(&sealed).unwrap(), b"old-secret");
    }
}
