// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-secrets — envelope encryption for monitoring credentials at rest (ADR-018).
//!
//! Each secret is encrypted with a fresh per-secret **DEK** (AES-256-GCM); the DEK is then
//! **wrapped** by the **KEK** (master key). The database stores only the ciphertext and the
//! wrapped DEK — the KEK lives outside the DB, loaded from a mounted secret file via a
//! [`KeyProvider`] (never an env var). Wrapped DEKs carry a `key_id` so KEKs can be rotated
//! by re-wrapping. Only core holds the KEK; pollers never decrypt (ADR-020).

pub mod password;

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// The AES-GCM nonce length every seal writes, and the only length [`EnvelopeCipher::open`] accepts.
const GCM_NONCE_BYTES: usize = NONCE_LEN;

/// A DEK or KEK: AES-256, so 32 bytes.
const KEY_BYTES: usize = 32;

// The AEAD is `ring`'s AES-256-GCM since ADR-213 decision 2 (it was the `aes-gcm` crate). The stored
// shape did not change: a 12-byte nonce, and the ciphertext with its 16-byte tag appended - which is
// what both libraries produce, and what `opens_a_secret_sealed_before_adr_213` holds them to.

fn aead_key(bytes: &[u8]) -> Result<LessSafeKey, SecretError> {
    UnboundKey::new(&AES_256_GCM, bytes)
        .map(LessSafeKey::new)
        .map_err(|_| SecretError::Crypto)
}

fn random_bytes<const N: usize>() -> Result<[u8; N], SecretError> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| SecretError::Crypto)?;
    Ok(out)
}

/// Encrypt under a fresh random nonce; returns `(nonce, ciphertext || tag)`.
fn encrypt(key: &LessSafeKey, plaintext: &[u8]) -> Result<([u8; NONCE_LEN], Vec<u8>), SecretError> {
    let nonce = random_bytes::<NONCE_LEN>()?;
    let mut buf = plaintext.to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .map_err(|_| SecretError::Crypto)?;
    Ok((nonce, buf))
}

fn decrypt(key: &LessSafeKey, nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, SecretError> {
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| SecretError::Crypto)?;
    let mut buf = ciphertext.to_vec();
    let plain_len = key
        .open_in_place(nonce, Aad::empty(), &mut buf)
        .map_err(|_| SecretError::Crypto)?
        .len();
    buf.truncate(plain_len);
    Ok(buf)
}

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

    /// Add a KEK generation. **The deferred KEK-rotation surface** (ADR-018), together with
    /// [`Self::set_active`]: rotation is "generate a new KEK → re-wrap every DEK → retire the old
    /// one", which needs several generations readable at once. That is why [`SealedSecret`] carries
    /// a `key_id` and why [`KeyProvider`] is keyed by generation rather than holding one key.
    ///
    /// Nothing wires it yet — `key_provider_from_file` mounts a single generation-1 KEK and
    /// `yagra-core` never advances it, so the only callers today are tests. Do not delete it as
    /// dead code: the decision is made and the storage format already commits to it; what is
    /// missing is the operator-facing re-wrap procedure, not the design.
    pub fn add_generation(&mut self, key_id: u32, kek: [u8; 32]) {
        self.keys.insert(key_id, kek);
    }

    /// Set the active generation for new seals — the second half of the deferred rotation surface
    /// described on [`Self::add_generation`]. Older generations stay readable, so secrets sealed
    /// before the switch keep opening while they are re-wrapped.
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
        let dek = random_bytes::<KEY_BYTES>()?;
        let (ct_nonce, ciphertext) = encrypt(&aead_key(&dek)?, plaintext)?;

        // Wrap the DEK under the KEK.
        let (dek_nonce, wrapped_dek) = encrypt(&aead_key(&kek)?, &dek)?;

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
        // A stored row whose nonce is not 12 bytes is corrupt, and a corrupt row is exactly what
        // `decrypt_report` exists to name and what the scheduler steps over with a warning — both
        // need an `Err` to do it, never a panic (ADR-158 C8). `decrypt` would refuse it too; the
        // explicit check keeps the rule readable here rather than resting on a library's choice.
        if sealed.dek_nonce.len() != GCM_NONCE_BYTES || sealed.ct_nonce.len() != GCM_NONCE_BYTES {
            return Err(SecretError::Crypto);
        }
        let kek = self
            .keys
            .kek(sealed.key_id)
            .ok_or(SecretError::KeyNotFound(sealed.key_id))?;

        // Unwrap the DEK. A DEK that is not 32 bytes is refused by `aead_key`.
        let dek = decrypt(&aead_key(&kek)?, &sealed.dek_nonce, &sealed.wrapped_dek)?;

        // Decrypt the secret.
        decrypt(&aead_key(&dek)?, &sealed.ct_nonce, &sealed.ciphertext)
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

    /// A stored row whose nonce is not 12 bytes is corrupt, and corrupt is an `Err`, never a panic
    /// (ADR-158 C8). The callers are built on that: `decrypt_report` exists to *report* the rows a
    /// restore broke, and the scheduler warns and moves on when a credential will not open. A panic
    /// takes down the check meant to find the row, and the poll dispatch along with it.
    #[test]
    fn open_with_a_wrong_length_nonce_is_an_error_not_a_panic() {
        let cipher = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x66)));
        let sealed = cipher.seal(b"community").unwrap();

        for len in [0, 11, 13, 16] {
            let mut short_dek = sealed.clone();
            short_dek.dek_nonce = vec![0; len];
            assert!(
                matches!(cipher.open(&short_dek), Err(SecretError::Crypto)),
                "a {len}-byte DEK nonce"
            );

            let mut short_ct = sealed.clone();
            short_ct.ct_nonce = vec![0; len];
            assert!(
                matches!(cipher.open(&short_ct), Err(SecretError::Crypto)),
                "a {len}-byte ciphertext nonce"
            );
        }
        // The intact row still opens: the check refuses the length, not the secret.
        assert_eq!(cipher.open(&sealed).unwrap(), b"community");
        // And the length it accepts is the one the cipher writes, not a number typed beside it.
        assert_eq!(sealed.dek_nonce.len(), GCM_NONCE_BYTES);
        assert_eq!(sealed.ct_nonce.len(), GCM_NONCE_BYTES);
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// 🚨 **Every credential already stored was sealed by the `aes-gcm` crate** (ADR-213 decision 2
    /// moved the AEAD to `ring`). This row was produced by that implementation, under the KEK
    /// `[0x5a; 32]`, immediately before the switch. If it stops opening, every deployment that
    /// upgrades loses every SNMP community, v3 key and API token it holds.
    #[test]
    fn opens_a_secret_sealed_before_adr_213() {
        let sealed = SealedSecret {
            key_id: 1,
            dek_nonce: unhex("d6423ae33e5afdad1951d799"),
            wrapped_dek: unhex(concat!(
                "db061fa07768217bc33e476ae9ecceaa4351945107004f5ba58125ae840d886c",
                "479b0b6a9133edee05eba4d7a60cdfe0",
            )),
            ct_nonce: unhex("851d8cc95a23fe39b45a598b"),
            ciphertext: unhex(concat!(
                "c08f87947a1a3db0d268202a418c21823769ec1402180c163dfdfe638143cb84",
                "a8ec506e2e",
            )),
        };
        let cipher = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x5a)));
        assert_eq!(cipher.open(&sealed).unwrap(), b"sealed-before-ADR-213");

        // The stored shape is unchanged: a seal made now has the same lengths for the same input.
        let now = cipher.seal(b"sealed-before-ADR-213").unwrap();
        assert_eq!(now.wrapped_dek.len(), sealed.wrapped_dek.len());
        assert_eq!(now.ciphertext.len(), sealed.ciphertext.len());

        // And the fixture is not opening by accident: the wrong KEK refuses it.
        let wrong = EnvelopeCipher::new(StaticKeyProvider::single(kek(0x5b)));
        assert!(matches!(wrong.open(&sealed), Err(SecretError::Crypto)));
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
