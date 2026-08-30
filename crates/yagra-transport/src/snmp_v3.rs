// SPDX-License-Identifier: AGPL-3.0-only
//! SNMP v3 (USM) GET over a pure-Rust client (`snmp2`, crypto-rust backend) — resolves
//! the ADR-021 v3 question without a net-snmp FFI fallback.
//!
//! Auth (MD5/SHA1/SHA2 family) and privacy (DES/AES-128/192/256 CFB) come from the
//! credential resolved by core and inlined into the job (ADR-018/020) — this layer never
//! reads the secret store and never logs key material. Values are returned **raw**
//! (counters included) — rates are derived at query time (ADR-012). Live-only (needs a
//! device + UDP); the parameter mapping is unit-tested.

use crate::walk_budget::{is_silence, note_truncation, ColumnOutcome, Truncation, WalkBudget};
use crate::{
    SnmpInstanceRow, SnmpSample, SnmpStringSample, SnmpTableSample, SnmpTableString, SnmpV3Params,
    SnmpValue, TransportError,
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

/// Row budget for the walkers that predate ADR-043 Increment 3's explicit cap.
///
/// They are not unbounded — [`MAX_WALK_REQUESTS`] × [`WALK_MAX_REPETITIONS`] bounds them at 20,000
/// rows per column — so this names *which* bound applies rather than leaving a bare `usize::MAX`
/// that reads as none.
const ROWS_BOUNDED_BY_REQUEST_CEILING: usize = usize::MAX;

/// One failed `snmp2` exchange in which the agent said **nothing at all** (ADR-110 Increment 3).
///
/// Written down here rather than at each of the five loops below, because the rule is subtle and
/// the whole consecutive-failure design is wrong without it: **only the `tokio::time::timeout` arm
/// is silence.** A `snmp2::Error` means the exchange produced *something* — a decode failure, a
/// report PDU, an authentication complaint — and a device answering `noSuchObject` for a column it
/// does not implement must never be read as unreachable. `walk_budget`'s module doc carries the
/// full argument.
///
/// ⚠️ **v3 rarely reaches this at all.** [`open_session`] runs engine discovery before the first
/// column, so a silent device fails the whole *call* in one timeout rather than paying one per
/// column — the asymmetry with v2c, whose `connect` only binds a socket and never speaks. The
/// budget is wired here anyway for the device that goes quiet after the session is up, and so that
/// the tenth loop someone adds inherits it rather than reopening the defect.
const AGENT_SAID_NOTHING: ColumnOutcome = ColumnOutcome::Failed;

/// The agent answered — including by complaining. See [`AGENT_SAID_NOTHING`].
const AGENT_ANSWERED: ColumnOutcome = ColumnOutcome::Answered;

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
    let mut budget = WalkBudget::new(timeout);
    for (asked, oid_str) in oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, oids.len() - asked);
            break;
        }
        let Some(oid) = parse_oid(oid_str) else {
            tracing::warn!(%oid_str, "skipping malformed OID");
            budget.record(ColumnOutcome::Skipped);
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
                budget.record(AGENT_ANSWERED);
            }
            Ok(Err(e)) => {
                tracing::debug!(%oid_str, error = %e, "snmp v3 get failed");
                budget.record(AGENT_ANSWERED);
            }
            Err(_) => {
                tracing::debug!(%oid_str, "snmp v3 get timed out");
                budget.record(AGENT_SAID_NOTHING);
            }
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
    let mut budget = WalkBudget::new(timeout);
    for (asked, oid_str) in oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, oids.len() - asked);
            break;
        }
        let Some(oid) = parse_oid(oid_str) else {
            tracing::warn!(%oid_str, "skipping malformed OID");
            budget.record(ColumnOutcome::Skipped);
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
                budget.record(AGENT_ANSWERED);
            }
            Ok(Err(e)) => {
                tracing::debug!(%oid_str, error = %e, "snmp v3 string get failed");
                budget.record(AGENT_ANSWERED);
            }
            Err(_) => {
                tracing::debug!(%oid_str, "snmp v3 string get timed out");
                budget.record(AGENT_SAID_NOTHING);
            }
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
    let mut budget = WalkBudget::new(timeout);
    for (asked, base_str) in column_oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, column_oids.len() - asked);
            break;
        }
        let outcome = walk_column_v3(
            &mut session,
            base_str,
            timeout,
            ROWS_BOUNDED_BY_REQUEST_CEILING,
            |tail, value| {
                let ifindex = crate::ifindex_from_tail(tail)?;
                numeric(value).map(|v| SnmpTableSample {
                    oid_base: base_str.clone(),
                    ifindex,
                    value: v,
                })
            },
            &mut rows,
        )
        .await;
        budget.record(outcome);
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
    let mut budget = WalkBudget::new(timeout);
    for (asked, base_str) in column_oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, column_oids.len() - asked);
            break;
        }
        let outcome = walk_column_v3(
            &mut session,
            base_str,
            timeout,
            ROWS_BOUNDED_BY_REQUEST_CEILING,
            |tail, value| {
                let ifindex = crate::ifindex_from_tail(tail)?;
                string_value(value).map(|s| SnmpTableString {
                    oid_base: base_str.clone(),
                    ifindex,
                    value: s,
                })
            },
            &mut rows,
        )
        .await;
        budget.record(outcome);
    }
    Ok(rows)
}

/// Walk table columns keeping each row's **full instance index** and **raw** value via SNMP v3
/// (USM) — the v3 analogue of `snmp::snmp_walk_instances_v2c` (ADR-038). Adjacency data cannot go
/// through the numeric or string walkers: those fold the multi-part index and lossily decode the
/// octets, and `lldpRemTable` needs both preserved.
/// `max_rows` bounds the whole call across every column, enforced while paging — see the v2c twin
/// for why truncating afterwards would not be a bound at all. The pre-existing
/// [`MAX_WALK_REQUESTS`] ceiling is a *request* limit and stays as the defence against an agent
/// that never advances; it says nothing about how many rows a well-behaved agent can return.
pub async fn snmp_walk_instances_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    column_oids: &[String],
    timeout: Duration,
    max_rows: usize,
) -> Result<Vec<SnmpInstanceRow>, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let mut rows = Vec::new();
    let mut budget = WalkBudget::new(timeout);
    // Why the walk stopped, kept rather than dropped so the caller can be told the
    // device said nothing at all (ADR-110 Increment 4).
    let mut stopped: Option<Truncation> = None;
    for (asked, base_str) in column_oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, column_oids.len() - asked);
            stopped = Some(reason);
            break;
        }
        if rows.len() >= max_rows {
            tracing::debug!(%base_str, max_rows, "instance walk row budget spent; skipping column");
            break;
        }
        let row_budget = max_rows - rows.len();
        let outcome = walk_column_v3(
            &mut session,
            base_str,
            timeout,
            row_budget,
            |tail, value| {
                raw_value(value).map(|v| SnmpInstanceRow {
                    oid_base: base_str.clone(),
                    instance: tail.to_vec(),
                    value: v,
                })
            },
            &mut rows,
        )
        .await;
        budget.record(outcome);
    }
    // The loop only consults the budget at the *top* of an iteration, so a walk whose last
    // two columns both failed ends by running out of columns rather than by tripping. Ask
    // once more — the question is about the device, not about where the loop stopped.
    let stopped = stopped.or_else(|| budget.spent());
    // A device that failed two columns in a row and gave up nothing is not a device that
    // does not implement these columns — it is one that is not answering. Saying so is what
    // lets `execute_mau` stop after one walk instead of three (ADR-110 Increment 4).
    if is_silence(rows.len(), stopped) {
        return Err(TransportError::Silent(target));
    }
    Ok(rows)
}

/// Walk one column subtree via repeated GETBULK, mapping each in-subtree varbind to an `R` row
/// (via `map`) and pushing it onto `out`. Pages until the walk leaves the column's subtree, the
/// agent signals end-of-MIB, a request fails/times out, the agent stops advancing, or the
/// per-column request cap ([`MAX_WALK_REQUESTS`]) is hit. Errors are logged and end this column
/// only (one bad column doesn't fail the whole poll).
///
/// `map` receives the instance's **whole** sub-identifier tail. The metric walkers immediately fold
/// it with [`crate::ifindex_from_tail`] (so the v2c and v3 row keying can never diverge); the
/// neighbour walker keeps it, because a folded `lldpRemTable` index cannot be reassembled into
/// which local port faces which peer.
/// `budget` caps how many rows this column may contribute. [`ROWS_BOUNDED_BY_REQUEST_CEILING`] is
/// the value for the walkers that predate ADR-043 Increment 3 and are already bounded by
/// [`MAX_WALK_REQUESTS`] × [`WALK_MAX_REPETITIONS`] rows — naming it says which bound applies rather
/// than leaving a bare `usize::MAX` that reads as "no bound at all".
async fn walk_column_v3<R>(
    session: &mut AsyncSession,
    base_str: &str,
    timeout: Duration,
    row_budget: usize,
    map: impl Fn(&[u32], &Value) -> Option<R>,
    out: &mut Vec<R>,
) -> ColumnOutcome {
    if parse_oid(base_str).is_none() {
        tracing::warn!(%base_str, "skipping malformed table column OID");
        return ColumnOutcome::Skipped;
    }
    let mut cursor_str = base_str.to_owned();
    let mut taken = 0usize;
    for _ in 0..MAX_WALK_REQUESTS {
        if taken >= row_budget {
            return AGENT_ANSWERED;
        }
        let Some(cursor) = parse_oid(&cursor_str) else {
            return ColumnOutcome::Skipped;
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
                return AGENT_ANSWERED;
            }
            Err(_) => {
                tracing::debug!(%base_str, "snmp v3 table walk timed out");
                return AGENT_SAID_NOTHING;
            }
        };
        // Scan this page: collect in-subtree rows and note the last OID reached so the next
        // GETBULK can continue after it. Stop the moment the walk leaves the column subtree or
        // the agent reports it has no more.
        let mut last_in_subtree: Option<String> = None;
        let mut stop = false;
        for (oid, value) in pdu.varbinds {
            // The budget is checked here, inside the page loop, rather than after it: the cost this
            // bound exists to refuse is the memory the rows occupy, and a row already pushed has
            // already cost it.
            if taken >= row_budget {
                stop = true;
                break;
            }
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
            // Counted whether or not `map` produced a row: the budget bounds what the *device* is
            // allowed to make this walk read, and a row the mapper declined still arrived.
            taken += 1;
            if let Some(row) = map(&tail, &value) {
                out.push(row);
            }
            last_in_subtree = Some(oid_str);
        }
        match last_in_subtree {
            // More in-subtree rows may follow — advance past the last one, unless the agent
            // failed to advance (defensive: GETBULK returns strictly-greater OIDs).
            Some(next) if !stop && next != cursor_str => cursor_str = next,
            // The agent answered every page it was asked for; the column ended on its own terms.
            _ => return AGENT_ANSWERED,
        }
    }
    AGENT_ANSWERED
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

/// Map an SNMP value onto [`SnmpValue`] without coercing — the neighbour walk's mapper. `None` for
/// types with no representation (null, end-of-MIB, the exception markers), skipped like any other
/// unusable row. Mirrors `snmp::raw_value`; kept per-module because the two clients have separate
/// value enums.
///
/// Matched variant-by-variant rather than with a wildcard, for the same reason `snmp::raw_value`
/// is: this is a walker that must not silently drop a column. It used to end in `_ => None`, and
/// that wildcard swallowed two types the v2c mapper handles — `IpAddress` and `Opaque`. Nothing
/// noticed, because the only consumer at the time (the neighbour walk) reads neither. `ipAdEntNetMask`
/// is an ASN.1 `IpAddress`, so ADR-043's IPv4 mask column would have come back empty on every
/// SNMPv3 node while working perfectly on v2c. A new variant should be a compile error here, not a
/// row that quietly disappears.
pub(crate) fn raw_value(value: &Value) -> Option<SnmpValue> {
    match value {
        Value::Integer(i) => Some(SnmpValue::Int(*i)),
        Value::Counter32(c) | Value::Unsigned32(c) => Some(SnmpValue::Int(i64::from(*c))),
        Value::Timeticks(t) => Some(SnmpValue::Int(i64::from(*t))),
        // Saturate rather than wrap: a negative value would be read as a different subtype.
        Value::Counter64(c) => Some(SnmpValue::Int(i64::try_from(*c).unwrap_or(i64::MAX))),
        Value::OctetString(bytes) | Value::Opaque(bytes) => {
            Some(SnmpValue::Bytes((*bytes).to_vec()))
        }
        // Kept as octets so the caller reads it the same way it reads any other address column.
        Value::IpAddress(octets) => Some(SnmpValue::Bytes(octets.to_vec())),
        Value::ObjectIdentifier(oid) => Some(SnmpValue::Oid(oid.to_id_string())),
        // No representation as a scalar column value: structural types, the three exception
        // markers an agent returns instead of a value, and the PDU tags that never appear in a
        // varbind at all. Listed so the next variant added upstream lands here as a compile error.
        Value::Boolean(_)
        | Value::Null
        | Value::Sequence(_)
        | Value::Set(_)
        | Value::Constructed(..)
        | Value::EndOfMibView
        | Value::NoSuchObject
        | Value::NoSuchInstance
        | Value::GetRequest(_)
        | Value::GetNextRequest(_)
        | Value::GetBulkRequest(_)
        | Value::Response(_)
        | Value::SetRequest(_)
        | Value::InformRequest(_)
        | Value::Trap(_)
        | Value::Report(_) => None,
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

    /// `walk_column_v3` hands the mapper the **whole** tail. The metric walkers fold it; the
    /// neighbour walker must be able to keep it, or `lldpRemTable`'s three-part index is lost.
    #[test]
    fn the_walk_mapper_sees_the_unfolded_instance() {
        let base = "1.0.8802.1.1.2.1.4.1.1.5";
        let tail = tail_subids("1.0.8802.1.1.2.1.4.1.1.5.0.7.3", base).unwrap();
        assert_eq!(tail, vec![0, 7, 3]);
        // Folding it — what the metric mappers do — is a one-way trip.
        assert!(crate::ifindex_from_tail(&tail).is_some());
    }

    #[test]
    fn raw_value_keeps_octets_verbatim_where_string_value_would_mangle_them() {
        let mac = b"\x00\x1bT\xff\x00\x9a";
        assert_eq!(
            raw_value(&Value::OctetString(mac)),
            Some(SnmpValue::Bytes(mac.to_vec()))
        );
        assert_ne!(
            string_value(&Value::OctetString(mac)).map(String::into_bytes),
            Some(mac.to_vec())
        );
        assert_eq!(raw_value(&Value::Integer(4)), Some(SnmpValue::Int(4)));
        assert_eq!(raw_value(&Value::NoSuchObject), None);
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
