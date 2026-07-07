//! SNMP v3 (USM) GET over a pure-Rust client (`snmp2`, crypto-rust backend) — resolves
//! the ADR-021 v3 question without a net-snmp FFI fallback.
//!
//! Auth (MD5/SHA1/SHA2 family) and privacy (DES/AES-128/192/256 CFB) come from the
//! credential resolved by core and inlined into the job (ADR-018/020) — this layer never
//! reads the secret store and never logs key material. Values are returned **raw**
//! (counters included) — rates are derived at query time (ADR-012). Live-only (needs a
//! device + UDP); the parameter mapping is unit-tested.

use crate::{
    SnmpSample, SnmpStringSample, SnmpTableSample, SnmpTableString, SnmpV3Params, TransportError,
};
use snmp2::{v3, AsyncSession, Oid, Value};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Standard SNMP agent port.
const SNMP_PORT: u16 = 161;

/// GETBULK max-repetitions per request — bounds one response PDU's size so a large table is paged,
/// not pulled in one oversized PDU. Mirrors the v2c walker's cap.
const WALK_MAX_REPETITIONS: u32 = 20;

/// Safety cap on GETBULK requests per column: a broken or looping agent can't spin the walk
/// forever (`WALK_MAX_REPETITIONS` × this bounds the rows collected per column).
const MAX_WALK_REQUESTS: usize = 1000;

/// Open a v3 session against `target` and run engine discovery (id/boots/time) — required
/// before authenticated requests.
async fn open_session(
    target: IpAddr,
    params: &SnmpV3Params,
    timeout: Duration,
) -> Result<AsyncSession, TransportError> {
    let security = build_security(params).map_err(TransportError::Io)?;
    let addr = SocketAddr::new(target, SNMP_PORT);
    let mut session = AsyncSession::new_v3(addr, 0, security)
        .await
        .map_err(|e| TransportError::Io(format!("snmp v3 connect {addr}: {e}")))?;
    tokio::time::timeout(timeout, session.init())
        .await
        .map_err(|_| TransportError::Io(format!("snmp v3 engine discovery {addr}: timeout")))?
        .map_err(|e| TransportError::Io(format!("snmp v3 engine discovery {addr}: {e}")))?;
    Ok(session)
}

/// Fetch `oids` from `target` via SNMP v3 (USM). Per-OID failures are logged and skipped
/// so a single bad OID doesn't fail the whole poll; an auth/engine failure fails the call.
pub async fn snmp_get_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpSample>, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let mut samples = Vec::with_capacity(oids.len());
    for oid_str in oids {
        let Some(oid) = parse_oid(oid_str) else {
            tracing::warn!(%oid_str, "skipping malformed OID");
            continue;
        };
        match tokio::time::timeout(timeout, session.get(&oid)).await {
            Ok(Ok(pdu)) => {
                for (_, value) in pdu.varbinds {
                    if let Some(v) = numeric(&value) {
                        samples.push(SnmpSample {
                            oid: oid_str.clone(),
                            value: v,
                        });
                    }
                }
            }
            Ok(Err(e)) => tracing::debug!(%oid_str, error = %e, "snmp v3 get failed"),
            Err(_) => tracing::debug!(%oid_str, "snmp v3 get timed out"),
        }
    }
    Ok(samples)
}

/// Fetch string-valued scalar `oids` (e.g. `sysDescr.0` / `sysName.0`) from `target` via
/// SNMP v3 (USM). Non-string values are skipped. Used by discovery for device identity.
pub async fn snmp_get_v3_strings(
    target: IpAddr,
    params: &SnmpV3Params,
    oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpStringSample>, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let mut samples = Vec::with_capacity(oids.len());
    for oid_str in oids {
        let Some(oid) = parse_oid(oid_str) else {
            tracing::warn!(%oid_str, "skipping malformed OID");
            continue;
        };
        match tokio::time::timeout(timeout, session.get(&oid)).await {
            Ok(Ok(pdu)) => {
                for (_, value) in pdu.varbinds {
                    if let Some(v) = string_value(&value) {
                        samples.push(SnmpStringSample {
                            oid: oid_str.clone(),
                            value: v,
                        });
                    }
                }
            }
            Ok(Err(e)) => tracing::debug!(%oid_str, error = %e, "snmp v3 string get failed"),
            Err(_) => tracing::debug!(%oid_str, "snmp v3 string get timed out"),
        }
    }
    Ok(samples)
}

/// Walk numeric table columns from `target` via SNMP v3 (USM) GETBULK — the v3 analogue of
/// `snmp_walk_v2c`. Each column base yields one numeric row per instance, keyed by ifIndex (a
/// single trailing sub-id) or a folded synthetic key (multi-index tables). A per-column walk
/// failure is logged and skipped. Counters are returned **raw** (rates derived at query time,
/// ADR-012).
pub async fn snmp_walk_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    column_oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpTableSample>, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let mut rows = Vec::new();
    for base_str in column_oids {
        walk_column_v3(
            &mut session,
            base_str,
            timeout,
            |ifindex, value| {
                numeric(value).map(|v| SnmpTableSample {
                    oid_base: base_str.clone(),
                    ifindex,
                    value: v,
                })
            },
            &mut rows,
        )
        .await;
    }
    Ok(rows)
}

/// Walk string-valued table columns (e.g. `ifName`, `ifAlias`) from `target` via SNMP v3 (USM)
/// GETBULK — the v3 analogue of `snmp_walk_strings_v2c`, for interface metadata (PostgreSQL, never
/// TSDB labels — ADR-011). Same per-column skip-on-error behaviour; non-string values are skipped.
pub async fn snmp_walk_strings_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    column_oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpTableString>, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let mut rows = Vec::new();
    for base_str in column_oids {
        walk_column_v3(
            &mut session,
            base_str,
            timeout,
            |ifindex, value| {
                string_value(value).map(|s| SnmpTableString {
                    oid_base: base_str.clone(),
                    ifindex,
                    value: s,
                })
            },
            &mut rows,
        )
        .await;
    }
    Ok(rows)
}

/// Walk one column subtree via repeated GETBULK, mapping each in-subtree varbind to an `R` row
/// (via `map`) and pushing it onto `out`. Pages until the walk leaves the column's subtree, the
/// agent signals end-of-MIB, a request fails/times out, the agent stops advancing, or the
/// per-column request cap ([`MAX_WALK_REQUESTS`]) is hit. Errors are logged and end this column
/// only (one bad column doesn't fail the whole poll).
async fn walk_column_v3<R>(
    session: &mut AsyncSession,
    base_str: &str,
    timeout: Duration,
    map: impl Fn(u32, &Value) -> Option<R>,
    out: &mut Vec<R>,
) {
    if parse_oid(base_str).is_none() {
        tracing::warn!(%base_str, "skipping malformed table column OID");
        return;
    }
    let mut cursor_str = base_str.to_owned();
    for _ in 0..MAX_WALK_REQUESTS {
        let Some(cursor) = parse_oid(&cursor_str) else {
            return;
        };
        let pdu = match tokio::time::timeout(
            timeout,
            session.getbulk(&[&cursor], 0, WALK_MAX_REPETITIONS),
        )
        .await
        {
            Ok(Ok(pdu)) => pdu,
            Ok(Err(e)) => {
                tracing::debug!(%base_str, error = %e, "snmp v3 table walk failed");
                return;
            }
            Err(_) => {
                tracing::debug!(%base_str, "snmp v3 table walk timed out");
                return;
            }
        };
        // Scan this page: collect in-subtree rows and note the last OID reached so the next
        // GETBULK can continue after it. Stop the moment the walk leaves the column subtree or
        // the agent reports it has no more.
        let mut last_in_subtree: Option<String> = None;
        let mut stop = false;
        for (oid, value) in pdu.varbinds {
            if matches!(
                value,
                Value::EndOfMibView | Value::NoSuchObject | Value::NoSuchInstance
            ) {
                stop = true;
                break;
            }
            let oid_str = oid.to_id_string();
            let Some(tail) = tail_subids(&oid_str, base_str) else {
                stop = true; // walked past this column's subtree — done
                break;
            };
            if let Some(ifindex) = crate::ifindex_from_tail(&tail) {
                if let Some(row) = map(ifindex, &value) {
                    out.push(row);
                }
            }
            last_in_subtree = Some(oid_str);
        }
        match last_in_subtree {
            // More in-subtree rows may follow — advance past the last one, unless the agent
            // failed to advance (defensive: GETBULK returns strictly-greater OIDs).
            Some(next) if !stop && next != cursor_str => cursor_str = next,
            _ => return,
        }
    }
}

/// Sub-identifiers of `oid_str` past the column `base_str`, or `None` when `oid_str` is not a
/// strict descendant of `base_str` (a different subtree, or the base itself — no instance).
/// Compared on the dotted-decimal form so this doesn't depend on the client's relative-OID API;
/// requires a `.` boundary after the base so `…2` is not read as a prefix of `…20`.
fn tail_subids(oid_str: &str, base_str: &str) -> Option<Vec<u32>> {
    let rest = oid_str
        .strip_prefix(base_str)
        .and_then(|r| r.strip_prefix('.'))?;
    rest.split('.').map(|p| p.parse::<u32>().ok()).collect()
}

/// Map job-level v3 params onto the `snmp2` USM security config. Key material flows
/// through untouched and is never logged. `Err` carries a *static* description only
/// (no secrets) suitable for logs.
fn build_security(params: &SnmpV3Params) -> Result<v3::Security, String> {
    let auth_key = params.auth_key.as_deref().unwrap_or("");
    let mut security = v3::Security::new(params.user.as_bytes(), auth_key.as_bytes());

    match params.security_level.as_str() {
        "noauth" => {
            security = security.with_auth(v3::Auth::NoAuthNoPriv);
        }
        "auth" => {
            security = security
                .with_auth_protocol(parse_auth_protocol(params.auth_protocol.as_deref())?)
                .with_auth(v3::Auth::AuthNoPriv);
        }
        "authpriv" => {
            let priv_key = params
                .priv_key
                .as_deref()
                .ok_or_else(|| "snmp v3 authpriv requires a privacy key".to_owned())?;
            security = security
                .with_auth_protocol(parse_auth_protocol(params.auth_protocol.as_deref())?)
                .with_auth(v3::Auth::AuthPriv {
                    cipher: parse_cipher(params.priv_protocol.as_deref())?,
                    privacy_password: priv_key.as_bytes().to_vec(),
                });
        }
        other => return Err(format!("unknown snmp v3 security level: {other}")),
    }
    Ok(security)
}

/// Parse an auth-protocol token (defaults to SHA-1 when unset — the common modern floor;
/// MD5 must be opted into explicitly).
fn parse_auth_protocol(token: Option<&str>) -> Result<v3::AuthProtocol, String> {
    match token.unwrap_or("sha") {
        "md5" => Ok(v3::AuthProtocol::Md5),
        "sha" | "sha1" => Ok(v3::AuthProtocol::Sha1),
        "sha224" => Ok(v3::AuthProtocol::Sha224),
        "sha256" => Ok(v3::AuthProtocol::Sha256),
        "sha384" => Ok(v3::AuthProtocol::Sha384),
        "sha512" => Ok(v3::AuthProtocol::Sha512),
        other => Err(format!("unknown snmp v3 auth protocol: {other}")),
    }
}

/// Parse a privacy-cipher token (defaults to AES-128 when unset; DES is explicit-only).
fn parse_cipher(token: Option<&str>) -> Result<v3::Cipher, String> {
    match token.unwrap_or("aes") {
        "des" => Ok(v3::Cipher::Des),
        "aes" | "aes128" => Ok(v3::Cipher::Aes128),
        "aes192" => Ok(v3::Cipher::Aes192),
        "aes256" => Ok(v3::Cipher::Aes256),
        other => Err(format!("unknown snmp v3 privacy protocol: {other}")),
    }
}

/// Parse a dotted OID string into an `snmp2` [`Oid`].
fn parse_oid(s: &str) -> Option<Oid<'static>> {
    let parts: Vec<u64> = s
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    Oid::from(parts.as_slice()).ok()
}

/// Map a numeric SNMP value to `f64`; non-numeric values yield `None` (skipped).
#[allow(clippy::cast_precision_loss)]
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => Some(*i as f64),
        Value::Counter32(c) => Some(f64::from(*c)),
        Value::Unsigned32(u) => Some(f64::from(*u)),
        Value::Timeticks(t) => Some(f64::from(*t)),
        Value::Counter64(c) => Some(*c as f64),
        _ => None,
    }
}

/// Map a string-ish SNMP value to a `String`: octet string (lossy UTF-8) or object
/// identifier (dotted decimal — e.g. `sysObjectID`). Device-supplied, treat as untrusted;
/// other value types yield `None` (skipped).
fn string_value(value: &Value) -> Option<String> {
    match value {
        Value::OctetString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::ObjectIdentifier(oid) => Some(oid.to_id_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(level: &str) -> SnmpV3Params {
        SnmpV3Params {
            user: "monitor".to_owned(),
            security_level: level.to_owned(),
            auth_protocol: Some("sha256".to_owned()),
            auth_key: Some("auth-pass-12345".to_owned()),
            priv_protocol: Some("aes256".to_owned()),
            priv_key: Some("priv-pass-12345".to_owned()),
        }
    }

    #[test]
    fn builds_security_for_every_level() {
        assert!(build_security(&params("noauth")).is_ok());
        assert!(build_security(&params("auth")).is_ok());
        assert!(build_security(&params("authpriv")).is_ok());
    }

    #[test]
    fn authpriv_without_priv_key_is_rejected() {
        let mut p = params("authpriv");
        p.priv_key = None;
        let err = build_security(&p).expect_err("must require a privacy key");
        // The error names the problem without echoing any key material.
        assert!(err.contains("privacy key"));
    }

    #[test]
    fn unknown_level_and_protocols_are_rejected() {
        let mut p = params("paranoid");
        assert!(build_security(&p).is_err());
        p.security_level = "auth".to_owned();
        p.auth_protocol = Some("rot13".to_owned());
        assert!(build_security(&p).is_err());
        let mut p2 = params("authpriv");
        p2.priv_protocol = Some("xor".to_owned());
        assert!(build_security(&p2).is_err());
    }

    #[test]
    fn protocol_tokens_map_and_default_safely() {
        // Defaults: SHA-1 auth, AES-128 privacy. MD5/DES only on explicit request.
        assert!(matches!(
            parse_auth_protocol(None),
            Ok(v3::AuthProtocol::Sha1)
        ));
        assert!(matches!(
            parse_auth_protocol(Some("sha512")),
            Ok(v3::AuthProtocol::Sha512)
        ));
        assert!(matches!(
            parse_auth_protocol(Some("md5")),
            Ok(v3::AuthProtocol::Md5)
        ));
        assert!(matches!(parse_cipher(None), Ok(v3::Cipher::Aes128)));
        assert!(matches!(
            parse_cipher(Some("aes256")),
            Ok(v3::Cipher::Aes256)
        ));
        assert!(matches!(parse_cipher(Some("des")), Ok(v3::Cipher::Des)));
    }

    #[test]
    fn parses_valid_oid_and_rejects_garbage() {
        assert!(parse_oid("1.3.6.1.2.1.1.3.0").is_some());
        assert!(parse_oid("1.3.x.1").is_none());
        assert!(parse_oid("").is_none());
    }

    #[test]
    fn tail_subids_extracts_instance_and_rejects_non_descendants() {
        let base = "1.3.6.1.2.1.31.1.1.1.6";
        // A single-instance row (ifIndex 7).
        assert_eq!(tail_subids("1.3.6.1.2.1.31.1.1.1.6.7", base), Some(vec![7]));
        // A multi-part instance (multi-index table) returns every sub-id.
        assert_eq!(
            tail_subids("1.3.6.1.2.1.31.1.1.1.6.1.0.0", base),
            Some(vec![1, 0, 0])
        );
        // The column base itself has no instance.
        assert_eq!(tail_subids(base, base), None);
        // A different subtree is not a descendant.
        assert_eq!(tail_subids("1.3.6.1.2.1.2.2.1.8.7", base), None);
        // A string prefix that is NOT an OID-boundary descendant must be rejected: `…1.6` must not
        // capture `…1.60.1` (the `.` boundary guards this).
        assert_eq!(tail_subids("1.3.6.1.2.1.31.1.1.1.60.1", base), None);
    }

    #[test]
    fn tail_subids_feeds_shared_ifindex_keying() {
        // A single trailing sub-id keys directly; a multi-part tail folds to a stable, distinct key
        // — the exact same [`crate::ifindex_from_tail`] the v2c walker uses (no divergence).
        let base = "1.3.6.1.2.1.31.1.1.1.6";
        let single = tail_subids("1.3.6.1.2.1.31.1.1.1.6.7", base).unwrap();
        assert_eq!(crate::ifindex_from_tail(&single), Some(7));
        let a = tail_subids("1.3.6.1.2.1.31.1.1.1.6.1.0.0", base).unwrap();
        let b = tail_subids("1.3.6.1.2.1.31.1.1.1.6.2.0.0", base).unwrap();
        let ka = crate::ifindex_from_tail(&a);
        let kb = crate::ifindex_from_tail(&b);
        assert!(ka.is_some() && kb.is_some());
        assert_ne!(ka, kb);
    }

    #[test]
    fn maps_numeric_values_and_skips_others() {
        assert_eq!(numeric(&Value::Counter64(1_000)), Some(1_000.0));
        assert_eq!(numeric(&Value::Integer(-5)), Some(-5.0));
        assert_eq!(numeric(&Value::Timeticks(42)), Some(42.0));
        assert_eq!(numeric(&Value::OctetString(b"x")), None);
        assert_eq!(numeric(&Value::NoSuchObject), None);
    }

    #[test]
    fn maps_string_values_and_skips_others() {
        assert_eq!(
            string_value(&Value::OctetString(b"Huawei USG")),
            Some("Huawei USG".to_owned())
        );
        // Invalid UTF-8 decodes lossily rather than failing (device data is untrusted).
        assert_eq!(
            string_value(&Value::OctetString(b"fw\xff01")),
            Some("fw\u{fffd}01".to_owned())
        );
        assert_eq!(string_value(&Value::Integer(1)), None);
        assert_eq!(string_value(&Value::NoSuchObject), None);
    }

    #[test]
    fn string_value_renders_object_id_as_dotted_decimal() {
        // sysObjectID comes back as an OBJECT IDENTIFIER — render it dotted for classification.
        let oid = parse_oid("1.3.6.1.4.1.2011.2.1").unwrap();
        assert_eq!(
            string_value(&Value::ObjectIdentifier(oid)),
            Some("1.3.6.1.4.1.2011.2.1".to_owned())
        );
    }
}
