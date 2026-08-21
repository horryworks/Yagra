// SPDX-License-Identifier: AGPL-3.0-only
//! SNMPv3 USM credentials — the one definition the whole workspace shares.
//!
//! Before ADR-084 these six fields were declared **eleven times**: nine bus check structs in
//! `yagra-bus`, `SnmpV3Params` in `yagra-transport`, and `SnmpV3Secret` in `yagra-core`. All
//! eleven were byte-identical down to the serde attributes, and every one of them had to be
//! edited in lockstep. This crate is the only place that can hold one copy: `yagra-bus` and
//! `yagra-transport` are siblings that depend on `yagra-common` and on nothing else, so a
//! shared parent is the only spot from which both can be reached.
//!
//! **The keys are plaintext here, and that is the design, not an oversight.** Core resolves and
//! decrypts an `snmp_v3` credential at dispatch time and inlines the passphrases into the job
//! (ADR-018/020) so a poller never reads the secret store. Nothing in this type may be logged —
//! see `.claude/rules/security.md`.

use serde::{Deserialize, Serialize};

/// SNMPv3 (USM) authentication parameters, resolved and decrypted by core.
///
/// Field tokens are lowercase and validated at the API edge: `security_level` is
/// `noauth` | `auth` | `authpriv`; `auth_protocol` is `md5` | `sha` | `sha256` | …;
/// `priv_protocol` is `des` | `aes` | `aes128` | ….
///
/// **This type is flattened into every bus check that carries v3 credentials**
/// (`#[serde(flatten)]`), so the six fields appear at the *top level* of the check's JSON exactly
/// as they did when each check declared them itself. That is what makes ADR-084 a pure Rust-side
/// change: an N-1 poller deserialising a new core's job sees the same flat object it always saw.
/// 🚨 **Do not nest it under a key.** Wrapping these fields in `"auth": { … }` changes the wire
/// and breaks the rollout in the one direction ADR-017 forbids. `messages.rs` pins the key set
/// against that.
///
/// The four optional fields carry `#[serde(default)]` for N-1 tolerance (ADR-017): a producer
/// that omits them must not make the consumer fail. `user` and `security_level` are required and
/// deliberately have no default — a v3 job with neither is not a job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnmpV3Auth {
    /// USM user name.
    pub user: String,
    /// `noauth` | `auth` | `authpriv`.
    pub security_level: String,
    /// Auth protocol (`md5` | `sha` | …), if `security_level` is auth/authpriv.
    #[serde(default)]
    pub auth_protocol: Option<String>,
    /// Auth passphrase.
    #[serde(default)]
    pub auth_key: Option<String>,
    /// Privacy protocol (`des` | `aes` | …), if `security_level` is authpriv.
    #[serde(default)]
    pub priv_protocol: Option<String>,
    /// Privacy passphrase.
    #[serde(default)]
    pub priv_key: Option<String>,
}

impl SnmpV3Auth {
    /// The plaintext secrets this credential carries, borrowed, in field order, absent entries
    /// skipped. Feeds the support bundle's fail-closed redaction scan (ADR-045 Inc.4).
    ///
    /// Lives on the credential rather than at each of the eight `CheckSpec::SnmpV3*` arms that
    /// used to spell it out, because "which of these six fields is a secret" is a property of the
    /// credential and not of the check carrying it. The arms stay exhaustive over *variants*, so
    /// a new check kind still has to be classified by hand; what moved here is only the answer
    /// for a variant that carries this type.
    ///
    /// ⚠️ **Never log the result.**
    #[must_use]
    pub fn secret_literals(&self) -> Vec<&str> {
        // Destructured rather than field-accessed on purpose. A seventh field added to this
        // struct will not compile until somebody decides whether it is a secret; plain field
        // access would make the *unsafe* answer the silent one — a new passphrase quietly stops
        // being redacted, with every test green. Same reasoning as `CheckSpec::secret_literals`'s
        // refusal of a `_ =>` arm.
        let Self {
            user: _,
            security_level: _,
            auth_protocol: _,
            auth_key,
            priv_protocol: _,
            priv_key,
        } = self;
        auth_key
            .as_deref()
            .into_iter()
            .chain(priv_key.as_deref())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire contract in one line: six keys, at the top level, spelled exactly like this.
    ///
    /// This is the check that survives someone "tidying" the type into a nested object. A test
    /// that only round-trips through Rust cannot see that change — `SnmpV3Auth` would serialize
    /// and deserialize itself happily either way, and every N-1 poller would stop reading v3
    /// jobs. So assert on the *key set of the JSON*, not on the Rust value.
    #[test]
    fn the_six_usm_keys_are_flat_and_spelled_exactly_this_way() {
        let auth = SnmpV3Auth {
            user: "monitor".into(),
            security_level: "authpriv".into(),
            auth_protocol: Some("sha".into()),
            auth_key: Some("k1".into()),
            priv_protocol: Some("aes".into()),
            priv_key: Some("k2".into()),
        };
        let v = serde_json::to_value(&auth).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "auth_key",
                "auth_protocol",
                "priv_key",
                "priv_protocol",
                "security_level",
                "user",
            ],
            "the USM key set is the wire contract (ADR-084); nesting or renaming breaks N-1"
        );
    }

    /// N-1 tolerance: a producer that sends only the two required fields must deserialize.
    #[test]
    fn the_four_optional_fields_may_be_absent() {
        let auth: SnmpV3Auth =
            serde_json::from_str(r#"{"user":"monitor","security_level":"authpriv"}"#).unwrap();
        assert_eq!(auth.user, "monitor");
        assert!(auth.auth_protocol.is_none());
        assert!(auth.priv_key.is_none());
    }

    /// N+1 tolerance, the other half of ADR-017: a field this build has never heard of is
    /// ignored rather than refused. Guarded here because `deny_unknown_fields` anywhere in this
    /// type would silently make every flattened check reject a newer core's job.
    #[test]
    fn an_unknown_field_is_ignored_not_refused() {
        let auth: SnmpV3Auth = serde_json::from_str(
            r#"{"user":"monitor","security_level":"auth","context_engine_id":"80001f88"}"#,
        )
        .unwrap();
        assert_eq!(auth.security_level, "auth");
    }

    /// Both passphrases are reported, and the identifying fields are not.
    ///
    /// The negative half is the load-bearing one: `user` and the two protocol tokens are
    /// structural — they say *which* account and *how*, not the secret — and redacting them
    /// would strip exactly the words that keep a misconfiguration diagnosable.
    #[test]
    fn only_the_two_passphrases_count_as_secrets() {
        let auth = SnmpV3Auth {
            user: "monitor".into(),
            security_level: "authpriv".into(),
            auth_protocol: Some("sha".into()),
            auth_key: Some("authpass".into()),
            priv_protocol: Some("aes".into()),
            priv_key: Some("privpass".into()),
        };
        assert_eq!(auth.secret_literals(), vec!["authpass", "privpass"]);

        let bare = SnmpV3Auth {
            user: "monitor".into(),
            security_level: "noauth".into(),
            ..SnmpV3Auth::default()
        };
        assert!(
            bare.secret_literals().is_empty(),
            "noauth carries no secret"
        );
    }
}
