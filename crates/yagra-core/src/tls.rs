// SPDX-License-Identifier: AGPL-3.0-only
//! The one place a TLS client configuration is built for an operator-configured outbound peer.
//!
//! Two subsystems need this — forwarding destinations (ADR-034) and directory authentication
//! (ADR-041) — and both need exactly the same three things: the container's trust anchors, an
//! optional private CA the operator supplied, and the `ring` crypto provider named explicitly.
//! Copying it would have meant two copies of the sentence that matters most, which is the next one.
//!
//! **Certificate verification is never disabled and there is no flag to disable it** (security.md).
//! Both callers exist to send credentials to another system — a syslog body, an LDAP bind password —
//! so an unauthenticated peer is precisely the failure TLS is here to prevent. The answer to a
//! private CA is to configure it, which is why `ca_cert` exists; the answer is never to skip the
//! check. Reaching for rustls's escape hatch here would be a defect, and the test at the bottom
//! reads this file to prove nobody has — which is why that escape hatch is not named in this prose.

use std::sync::Arc;

/// Build a TLS client configuration trusting the system bundle plus `ca_cert`, if given.
///
/// # Errors
/// Returns operator-facing text: the PEM did not parse, contained no certificate, was rejected, or
/// there are no trust anchors at all. Every one of these is the caller's configuration, so the text
/// is safe to show them and is what a settings page or a `/test` button renders.
pub fn client_config(ca_cert: Option<&str>) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        // Individual malformed anchors in a system bundle are skipped by `add`; that is not fatal.
        let _ = roots.add(cert);
    }
    let mut added = 0usize;
    if let Some(pem) = ca_cert.map(str::trim).filter(|p| !p.is_empty()) {
        use rustls::pki_types::{pem::PemObject, CertificateDer};
        for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let cert = cert.map_err(|e| format!("CA certificate is not valid PEM: {e}"))?;
            roots
                .add(cert)
                .map_err(|e| format!("CA certificate rejected: {e}"))?;
            added += 1;
        }
        if added == 0 {
            return Err("CA certificate contained no CERTIFICATE block".to_owned());
        }
    }
    if roots.is_empty() {
        return Err(
            "no trust anchors available (no system CA bundle and no CA certificate set)".to_owned(),
        );
    }
    // `ring` is named explicitly: both crypto providers end up enabled in this dependency graph, and
    // with both on rustls installs no process default — `ClientConfig::builder()` would panic.
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("TLS configuration: {e}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_ca_certificate_is_rejected_with_operator_facing_text() {
        let err = client_config(Some("-----BEGIN CERTIFICATE-----\nnot base64\n"))
            .expect_err("a malformed PEM must not build a config");
        assert!(err.contains("CA certificate"), "{err}");
    }

    #[test]
    fn a_pem_with_no_certificate_block_is_refused_rather_than_ignored() {
        // Silently falling back to the system bundle would mean an operator who pasted the wrong
        // file gets a working connection to a peer they did not intend to trust.
        let err = client_config(Some("-----BEGIN PRIVATE KEY-----\nMIIBOgIB\n"))
            .expect_err("a PEM with no CERTIFICATE must not build a config");
        assert!(err.contains("CA certificate"), "{err}");
    }

    #[test]
    fn blank_ca_text_is_the_same_as_none() {
        // The settings form sends "" for an empty textarea; that means "use the system bundle",
        // not "trust nothing".
        assert!(client_config(Some("   \n")).is_ok());
    }

    // The property this module exists to hold. Installing a custom verifier here would disable
    // certificate checking for **both** forwarding and directory login at once, and nothing about
    // the resulting connection would look different. The needles are assembled at runtime because
    // this test reads its own file (`reports/guards.rs::the_run_state_sql_is_built_from_the_enum`'s shape)
    // — and for the same reason none of them may be spelled out in this comment.
    #[test]
    fn this_module_never_disables_certificate_verification() {
        let src = include_str!("tls.rs");
        for needle in [
            format!("{}()", "dangerous"),
            format!("{}CertVerifier", "Server"),
            format!("set_{}_verifier", "certificate"),
        ] {
            assert!(
                !src.contains(&needle),
                "tls.rs reaches for `{needle}` — certificate verification must never be bypassed"
            );
        }
    }
}
