// SPDX-License-Identifier: AGPL-3.0-only
//! PEM checks shared by the places that take a pasted certificate (ADR-202 Inc.5).

/// Whether a paste that is supposed to hold only certificates contains a private key block.
///
/// Two callers: core's `server_cert` (the WebUI's chain) and `yagra-netbox`'s `validate_ca_pem`
/// (ADR-100 decision 8), which takes a pasted CA into another **plaintext, API-readable** column
/// and so owes the same refusal. One implementation rather than two — the security boundary is
/// the same one.
#[must_use]
pub fn contains_private_key_block(pem: &str) -> bool {
    pem.contains("PRIVATE KEY-----")
}
