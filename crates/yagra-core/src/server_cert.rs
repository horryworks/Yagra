// SPDX-License-Identifier: AGPL-3.0-only
//! The WebUI's server certificate (ADR-044): validate an imported one, or mint a self-signed one.
//!
//! Deliberately pure — no database, no filesystem, no clock beyond `Utc::now`. Everything that
//! decides whether an operator's paste is serviceable lives here so it can be tested without a
//! deployment, and `webtls.rs` is left with only storage and materialization.
//!
//! **Named `server_cert`, not `tls`.** [`crate::tls`] builds *client* configurations for outbound
//! peers and is a different subject; `extensibility.md` §5 is explicit that two modules meaning
//! different things by the same word costs orientation on every grep.
//!
//! ## What is checked, and what deliberately is not
//!
//! Checked: the chain parses and is leaf-first, the key parses, **the key actually belongs to the
//! leaf**, the certificate is inside its validity window, and it carries a subject alternative name.
//! Each is something that would otherwise surface as a failed TLS handshake minutes later, with the
//! browser's error and not Yagra's — which is the difference between "Yagra told me the key does not
//! match" and "the site is down and I do not know why".
//!
//! **Not checked: whether the chain is trusted.** No verification against a trust root, deliberately.
//! An enterprise CA is not in the container's bundle, so requiring it would fail every internal
//! certificate, and the pressure that creates is precisely the "let me turn verification off" that
//! [`crate::tls`] exists to resist. Yagra is the *server* here — trust is the browser's decision, and
//! Yagra has no standing to make it.
//!
//! ## The key↔certificate proof
//!
//! rustls already does this: [`CertifiedKey::keys_match`] compares the private key's
//! SubjectPublicKeyInfo against the leaf's. Using it means the check is made by the same library
//! that will serve the connection, rather than by a second implementation that could disagree with
//! it.
//!
//! ⚠️ **`CertifiedKey::from_der` is the wrong entry point and must not be used here.** It treats
//! `InconsistentKeys::Unknown` — "the key type could not report a public key, so no comparison was
//! possible" — as success. That is a reasonable default for a server loading its own configuration
//! and the wrong one for an import form, where *could not check* must never be recorded as
//! *matched*. This module calls `keys_match()` itself and refuses on `Unknown`.

use chrono::{DateTime, Datelike, Duration, Utc};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};

/// How long a generated self-signed certificate is valid.
///
/// 397 days is the CA/Browser Forum's maximum for a publicly trusted leaf, and browsers enforce it
/// on certificates that chain to a root the user added themselves. Going longer would work today and
/// stop working the moment someone does the sensible thing and installs this certificate in their
/// trust store — a failure that would arrive months later with no obvious cause.
pub const SELF_SIGNED_DAYS: i64 = 397;

/// Renew a self-signed certificate once it has this long left.
///
/// A certificate that expires takes the whole WebUI down, and a bootstrap certificate nobody
/// replaced is exactly the one nobody is watching. 30 days is long enough that the renewal happens
/// during ordinary running rather than at 3am on the day it lapses.
pub const RENEW_WITHIN_DAYS: i64 = 30;

/// A validated certificate and its private key, with everything the UI and the expiry check need.
#[derive(Debug, Clone)]
pub struct ServerCert {
    /// The chain, leaf first, re-emitted from the DER this module parsed.
    ///
    /// Re-emitted rather than passed through on purpose: what is stored is then exactly what was
    /// validated, and it cannot contain anything else. The `certificate` column is plaintext and the
    /// UI offers it for download, so a private key smuggled in beside the chain would be published.
    pub chain_pem: String,
    /// The private key in PEM, likewise re-emitted from validated DER.
    pub key_pem: String,
    pub meta: CertMeta,
}

/// What the settings page shows and the expiry check reads, parsed once at import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertMeta {
    pub subject: String,
    pub issuer: String,
    pub sans: Vec<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    /// Lowercase hex SHA-256 of the leaf's DER — the same digest a browser shows.
    pub fingerprint_sha256: String,
    /// Descriptive, e.g. `RSA-2048` or `ECDSA P-256`. Nothing branches on it.
    pub key_algorithm: String,
}

impl ServerCert {
    /// Chain then key, which is what nginx reads when both directives name one file.
    pub fn bundle_pem(&self) -> String {
        format!("{}{}", self.chain_pem, self.key_pem)
    }

    /// Whether this certificate is close enough to expiry to be worth renewing or warning about.
    pub fn expires_within(&self, days: i64, now: DateTime<Utc>) -> bool {
        self.meta.not_after <= now + Duration::days(days)
    }
}

/// Why an import was refused. Every variant is operator-facing: the API renders it verbatim.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CertError {
    #[error("the certificate field contains a PRIVATE KEY block — put the key in the private-key field instead. It would otherwise be stored unencrypted and offered for download alongside the certificate")]
    PrivateKeyInCertificateField,
    #[error("no CERTIFICATE block found — paste the certificate in PEM form, beginning with -----BEGIN CERTIFICATE-----")]
    NoCertificate,
    #[error("the certificate is not valid PEM: {0}")]
    BadCertificatePem(String),
    #[error("no private key found — paste it in PEM form, beginning with -----BEGIN PRIVATE KEY----- (or RSA/EC PRIVATE KEY)")]
    NoPrivateKey,
    #[error("the private key is encrypted with a passphrase, which cannot be read here. Convert it first:  openssl pkcs8 -topk8 -nocrypt -in key.pem -out key-plain.pem")]
    EncryptedPrivateKey,
    #[error("the private key is not valid PEM: {0}")]
    BadPrivateKeyPem(String),
    #[error(
        "unsupported private-key type — RSA, ECDSA (P-256/P-384) and Ed25519 keys are accepted"
    )]
    UnsupportedPrivateKey,
    #[error("the private key does not match the certificate")]
    KeyMismatch,
    #[error("the private key's type cannot report a public key, so it could not be checked against the certificate; use an RSA, ECDSA or Ed25519 key")]
    KeyUnverifiable,
    #[error("the certificate is not parseable X.509: {0}")]
    BadCertificate(String),
    #[error("the certificate expired on {0}")]
    Expired(DateTime<Utc>),
    #[error("the certificate is not valid until {0}")]
    NotYetValid(DateTime<Utc>),
    #[error("the certificate has no subject alternative name. Browsers ignore the common name entirely, so a certificate without a SAN is rejected by every current browser — reissue it with the hostnames and IP addresses in the SAN")]
    NoSubjectAltName,
    #[error("could not generate a certificate: {0}")]
    Generate(String),
}

/// Validate an operator-supplied certificate chain and private key.
///
/// # Errors
/// A [`CertError`] naming what is wrong in terms the operator can act on. Nothing here echoes the
/// key material, and no variant carries any part of it.
pub fn validate(chain_pem: &str, key_pem: &str) -> Result<ServerCert, CertError> {
    // Before anything else: a key pasted into the certificate box would be stored in a plaintext
    // column that the API returns and the UI offers as a download. Refusing is the only safe answer;
    // silently stripping it would leave the operator believing a key they exposed is still private.
    if contains_private_key_block(chain_pem) {
        return Err(CertError::PrivateKeyInCertificateField);
    }

    let mut chain: Vec<CertificateDer<'static>> = Vec::new();
    for der in CertificateDer::pem_slice_iter(chain_pem.as_bytes()) {
        chain.push(der.map_err(|e| CertError::BadCertificatePem(e.to_string()))?);
    }
    if chain.is_empty() {
        return Err(CertError::NoCertificate);
    }

    // Detected by label rather than by letting the parser fail, so the message can name the exact
    // openssl invocation. An encrypted key is the single most common thing an operator has to hand.
    if key_pem.contains("ENCRYPTED PRIVATE KEY") {
        return Err(CertError::EncryptedPrivateKey);
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| {
        let text = e.to_string();
        if text.contains("NoItemsFound") || text.contains("no items") {
            CertError::NoPrivateKey
        } else {
            CertError::BadPrivateKeyPem(text)
        }
    })?;

    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|_| CertError::UnsupportedPrivateKey)?;

    // See the module doc: `keys_match()` rather than `from_der`, because `from_der` maps
    // "could not compare" to success and this is an import form.
    let certified = CertifiedKey::new(chain.clone(), signing_key);
    match certified.keys_match() {
        Ok(()) => {}
        Err(rustls::Error::InconsistentKeys(rustls::InconsistentKeys::KeyMismatch)) => {
            return Err(CertError::KeyMismatch)
        }
        Err(_) => return Err(CertError::KeyUnverifiable),
    }

    let meta = parse_meta(&chain[0], &key)?;

    let now = Utc::now();
    if meta.not_after < now {
        return Err(CertError::Expired(meta.not_after));
    }
    if meta.not_before > now {
        return Err(CertError::NotYetValid(meta.not_before));
    }
    if meta.sans.is_empty() {
        return Err(CertError::NoSubjectAltName);
    }

    Ok(ServerCert {
        chain_pem: chain
            .iter()
            .map(|c| to_pem("CERTIFICATE", c))
            .collect::<String>(),
        key_pem: to_pem(private_key_label(&key), key.secret_der()),
        meta,
    })
}

/// Mint a self-signed certificate covering `names`, valid for [`SELF_SIGNED_DAYS`].
///
/// This is the bootstrap so a fresh deployment is HTTPS with no operator action. It is not the
/// destination — every surface that shows it says so.
///
/// # Errors
/// [`CertError::Generate`] if key generation or signing fails, or the ordinary validation errors:
/// the result is put through [`validate`] rather than trusted, so a generated certificate cannot
/// satisfy weaker rules than an imported one.
pub fn generate_self_signed(names: &[String], now: DateTime<Utc>) -> Result<ServerCert, CertError> {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

    // Deduplicated but order-preserving: the first name becomes the common name, and an operator who
    // listed their real hostname first should see it there.
    let mut seen = std::collections::HashSet::new();
    let names: Vec<String> = names
        .iter()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .filter(|n| seen.insert(n.clone()))
        .collect();

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(
        DnType::CommonName,
        names.first().map_or("yagra", String::as_str).to_owned(),
    );
    params.distinguished_name = dn;

    for name in &names {
        let san = match name.parse::<std::net::IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(
                rcgen::Ia5String::try_from(name.as_str().to_owned())
                    .map_err(|e| CertError::Generate(format!("{name}: {e}")))?,
            ),
        };
        params.subject_alt_names.push(san);
    }

    // rcgen's own default validity is effectively forever, which is not a default anyone should
    // inherit. Dates come from chrono (already in the tree) via rcgen's ymd helper, so `time` does
    // not become a direct dependency for two calls.
    let end = now + Duration::days(SELF_SIGNED_DAYS);
    params.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
    params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);

    let key_pair = KeyPair::generate().map_err(|e| CertError::Generate(e.to_string()))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| CertError::Generate(e.to_string()))?;

    // Straight back through `validate`. If a change here ever produced something that would not
    // survive the import path, this is where it fails — in a unit test, not on a deployment.
    validate(&cert.pem(), &key_pair.serialize_pem())
}

/// The names a bootstrap certificate should cover when the operator has not chosen any.
///
/// Loopback plus whatever the container calls itself. It will not match the address most operators
/// browse to, and cannot: nothing inside the container knows that name. That is the reason the
/// settings page offers regeneration with names the operator supplies.
pub fn default_self_signed_names(hostname: Option<&str>) -> Vec<String> {
    let mut names = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    if let Some(h) = hostname
        .map(str::trim)
        .filter(|h| !h.is_empty() && *h != "localhost")
    {
        names.insert(0, h.to_owned());
    }
    names
}

// ── internals ────────────────────────────────────────────────────────────────────────────────

/// Whether a paste that is supposed to hold only certificates contains a private key block.
///
/// `pub(crate)` for a second caller: `netbox::validate_ca_pem` (ADR-100 decision 8) takes a pasted
/// CA into another **plaintext, API-readable** column, so it owes the same refusal. One
/// implementation rather than two — the security boundary is the same one.
pub(crate) fn contains_private_key_block(pem: &str) -> bool {
    pem.contains("PRIVATE KEY-----")
}

fn private_key_label(key: &PrivateKeyDer<'_>) -> &'static str {
    match key {
        PrivateKeyDer::Pkcs1(_) => "RSA PRIVATE KEY",
        PrivateKeyDer::Sec1(_) => "EC PRIVATE KEY",
        // `PrivateKeyDer` is non_exhaustive, so this cannot be an exhaustive match. PKCS#8 is both
        // the remaining variant today and the right thing to emit for anything added later.
        _ => "PRIVATE KEY",
    }
}

/// DER → PEM, 64-column base64. Uses `data-encoding`, already a direct dependency for session
/// tokens, rather than adding a PEM crate for eleven lines.
fn to_pem(label: &str, der: &[u8]) -> String {
    let b64 = data_encoding::BASE64.encode(der);
    let mut out = String::with_capacity(b64.len() + b64.len() / 64 + 2 * label.len() + 32);
    out.push_str("-----BEGIN ");
    out.push_str(label);
    out.push_str("-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END ");
    out.push_str(label);
    out.push_str("-----\n");
    out
}

fn parse_meta(leaf: &CertificateDer<'_>, key: &PrivateKeyDer<'_>) -> Result<CertMeta, CertError> {
    use x509_parser::prelude::*;

    let (_, cert) = X509Certificate::from_der(leaf.as_ref())
        .map_err(|e| CertError::BadCertificate(e.to_string()))?;

    let mut sans = Vec::new();
    if let Ok(Some(ext)) = cert.subject_alternative_name() {
        for name in &ext.value.general_names {
            match name {
                GeneralName::DNSName(n) => sans.push((*n).to_owned()),
                GeneralName::IPAddress(bytes) => {
                    if let Some(ip) = ip_from_bytes(bytes) {
                        sans.push(ip.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    let to_utc = |t: ASN1Time| {
        DateTime::<Utc>::from_timestamp(t.timestamp(), 0).unwrap_or(DateTime::UNIX_EPOCH)
    };

    Ok(CertMeta {
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        sans,
        not_before: to_utc(cert.validity().not_before),
        not_after: to_utc(cert.validity().not_after),
        fingerprint_sha256: data_encoding::HEXLOWER.encode(&Sha256::digest(leaf.as_ref())),
        key_algorithm: key_algorithm(&cert, key),
    })
}

fn ip_from_bytes(bytes: &[u8]) -> Option<std::net::IpAddr> {
    match bytes.len() {
        4 => Some(std::net::IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?)),
        16 => Some(std::net::IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?)),
        _ => None,
    }
}

/// Best-effort human label for the key. Descriptive only — an unrecognised algorithm yields its OID
/// rather than a guess, because a wrong label is worse than an unfamiliar one.
fn key_algorithm(
    cert: &x509_parser::certificate::X509Certificate<'_>,
    key: &PrivateKeyDer<'_>,
) -> String {
    use x509_parser::public_key::PublicKey;

    let spki = cert.public_key();
    match spki.parsed() {
        Ok(PublicKey::RSA(rsa)) => format!("RSA-{}", rsa.key_size()),
        Ok(PublicKey::EC(_)) => match spki
            .algorithm
            .parameters
            .as_ref()
            .and_then(|p| p.as_oid().ok())
            .map(|o| o.to_id_string())
            .as_deref()
        {
            Some("1.2.840.10045.3.1.7") => "ECDSA P-256".to_owned(),
            Some("1.3.132.0.34") => "ECDSA P-384".to_owned(),
            Some("1.3.132.0.35") => "ECDSA P-521".to_owned(),
            _ => "ECDSA".to_owned(),
        },
        _ => match spki.algorithm.algorithm.to_id_string().as_str() {
            "1.3.101.112" => "Ed25519".to_owned(),
            other => {
                let _ = key;
                other.to_owned()
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(names: &[&str]) -> ServerCert {
        generate_self_signed(
            &names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            Utc::now(),
        )
        .expect("generation must succeed")
    }

    #[test]
    fn a_generated_certificate_survives_the_import_path() {
        // `generate_self_signed` runs its output through `validate`, so this asserts the two agree.
        // If generation ever produced something the import form would reject, a deployment would
        // bootstrap into a state an operator could not reproduce.
        let c = gen(&["yagra.example.net", "127.0.0.1"]);
        assert!(c.meta.sans.contains(&"yagra.example.net".to_owned()));
        assert!(c.meta.sans.contains(&"127.0.0.1".to_owned()));
        assert!(c.meta.subject.contains("yagra.example.net"));
        assert_eq!(c.meta.key_algorithm, "ECDSA P-256");
        assert!(c.chain_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(c.key_pem.contains("PRIVATE KEY"));
    }

    #[test]
    fn a_generated_certificate_is_valid_for_the_documented_window() {
        let c = gen(&["localhost"]);
        let span = (c.meta.not_after - c.meta.not_before).num_days();
        // ±1 for the midnight truncation rcgen's ymd helper applies at both ends.
        assert!(
            (SELF_SIGNED_DAYS - 1..=SELF_SIGNED_DAYS + 1).contains(&span),
            "validity was {span} days, expected about {SELF_SIGNED_DAYS}"
        );
        assert!(!c.expires_within(RENEW_WITHIN_DAYS, Utc::now()));
        assert!(c.expires_within(SELF_SIGNED_DAYS + 1, Utc::now()));
    }

    #[test]
    fn a_valid_pair_round_trips() {
        let c = gen(&["localhost"]);
        let again = validate(&c.chain_pem, &c.key_pem).expect("re-validating our own output");
        assert_eq!(again.meta, c.meta);
        assert_eq!(again.bundle_pem(), c.bundle_pem());
    }

    #[test]
    fn a_key_from_a_different_certificate_is_rejected() {
        let a = gen(&["a.example.net"]);
        let b = gen(&["b.example.net"]);
        assert_eq!(
            validate(&a.chain_pem, &b.key_pem).unwrap_err(),
            CertError::KeyMismatch
        );
    }

    /// The invariant the `KeyMismatch` branch depends on.
    ///
    /// `keys_match()` returns `Unknown` when the key type cannot report a public key, and this
    /// module maps that to a refusal. If rustls ever stopped implementing `public_key()` for a key
    /// type `any_supported_type` accepts, every import of that type would start being refused with a
    /// confusing message — or, had this module used `from_der`, silently accepted without the check.
    /// Either way the change should fail here first.
    #[test]
    fn the_key_check_reaches_a_verdict_for_the_key_types_we_accept() {
        let c = gen(&["localhost"]);
        let key = PrivateKeyDer::from_pem_slice(c.key_pem.as_bytes()).unwrap();
        let chain: Vec<_> = CertificateDer::pem_slice_iter(c.chain_pem.as_bytes())
            .map(Result::unwrap)
            .collect();
        let signing = rustls::crypto::ring::sign::any_supported_type(&key).unwrap();
        assert!(
            CertifiedKey::new(chain, signing).keys_match().is_ok(),
            "keys_match() could not compare a key rustls itself generated"
        );
    }

    #[test]
    fn a_private_key_in_the_certificate_field_is_refused() {
        // It would land in a plaintext column the API returns and the UI offers for download.
        let c = gen(&["localhost"]);
        let smuggled = format!("{}{}", c.chain_pem, c.key_pem);
        assert_eq!(
            validate(&smuggled, &c.key_pem).unwrap_err(),
            CertError::PrivateKeyInCertificateField
        );
    }

    #[test]
    fn an_encrypted_key_names_the_conversion_command() {
        let c = gen(&["localhost"]);
        let err = validate(
            &c.chain_pem,
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIB\n-----END ENCRYPTED PRIVATE KEY-----\n",
        )
        .unwrap_err();
        assert_eq!(err, CertError::EncryptedPrivateKey);
        // The message is the whole point of detecting this by label instead of letting the parser
        // fail with "unsupported": an operator with a passphrase-protected key needs the command.
        let text = err.to_string();
        assert!(text.contains("openssl pkcs8 -topk8 -nocrypt"), "{text}");
    }

    #[test]
    fn a_certificate_with_no_san_is_refused() {
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "cn-only.example.net");
        params.distinguished_name = dn;
        let now = Utc::now();
        let end = now + Duration::days(30);
        params.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
        params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
        let kp = KeyPair::generate().unwrap();
        let cert = params.self_signed(&kp).unwrap();

        assert_eq!(
            validate(&cert.pem(), &kp.serialize_pem()).unwrap_err(),
            CertError::NoSubjectAltName
        );
    }

    #[test]
    fn an_expired_certificate_is_refused_and_says_when() {
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "old.example.net");
        params.distinguished_name = dn;
        params.subject_alt_names.push(SanType::DnsName(
            rcgen::Ia5String::try_from("old.example.net".to_owned()).unwrap(),
        ));
        let start = Utc::now() - Duration::days(400);
        let end = Utc::now() - Duration::days(10);
        params.not_before =
            rcgen::date_time_ymd(start.year(), start.month() as u8, start.day() as u8);
        params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
        let kp = KeyPair::generate().unwrap();
        let cert = params.self_signed(&kp).unwrap();

        match validate(&cert.pem(), &kp.serialize_pem()).unwrap_err() {
            CertError::Expired(when) => assert!(when < Utc::now()),
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn a_not_yet_valid_certificate_is_refused() {
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "future.example.net");
        params.distinguished_name = dn;
        params.subject_alt_names.push(SanType::DnsName(
            rcgen::Ia5String::try_from("future.example.net".to_owned()).unwrap(),
        ));
        let start = Utc::now() + Duration::days(10);
        let end = Utc::now() + Duration::days(100);
        params.not_before =
            rcgen::date_time_ymd(start.year(), start.month() as u8, start.day() as u8);
        params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
        let kp = KeyPair::generate().unwrap();
        let cert = params.self_signed(&kp).unwrap();

        match validate(&cert.pem(), &kp.serialize_pem()).unwrap_err() {
            CertError::NotYetValid(when) => assert!(when > Utc::now()),
            other => panic!("expected NotYetValid, got {other:?}"),
        }
    }

    #[test]
    fn text_that_is_not_a_certificate_is_refused() {
        let c = gen(&["localhost"]);
        assert_eq!(
            validate("hello", &c.key_pem).unwrap_err(),
            CertError::NoCertificate
        );
        assert_eq!(
            validate("", &c.key_pem).unwrap_err(),
            CertError::NoCertificate
        );
    }

    #[test]
    fn a_certificate_with_no_key_is_refused() {
        let c = gen(&["localhost"]);
        assert!(matches!(
            validate(&c.chain_pem, "").unwrap_err(),
            CertError::NoPrivateKey | CertError::BadPrivateKeyPem(_)
        ));
    }

    #[test]
    fn the_bundle_is_chain_then_key_because_that_is_what_nginx_reads() {
        let c = gen(&["localhost"]);
        let bundle = c.bundle_pem();
        let cert_at = bundle.find("BEGIN CERTIFICATE").expect("chain present");
        let key_at = bundle.find("PRIVATE KEY-----").expect("key present");
        assert!(
            cert_at < key_at,
            "nginx reads one file; the chain comes first"
        );
    }

    #[test]
    fn default_names_cover_loopback_and_prefer_the_hostname() {
        let names = default_self_signed_names(Some("yagra-host"));
        assert_eq!(names.first().map(String::as_str), Some("yagra-host"));
        for expected in ["localhost", "127.0.0.1", "::1"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
        // A container whose hostname is literally "localhost" must not produce a duplicate SAN.
        assert_eq!(default_self_signed_names(Some("localhost")).len(), 3);
        assert_eq!(default_self_signed_names(None).len(), 3);
    }
}
